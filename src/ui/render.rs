//! Tile rendering (sprite-layer version, replacing the flat-colored-
//! square placeholder) + click selection + camera pan/zoom.
//!
//! **This batch's change (requirements §10's rendering/art-pipeline
//! phase):** each tile entity now has three child sprites instead of
//! one flat-colored square + text label:
//!   - `SoilLayer` — textured by `SoilType`, tinted at runtime by
//!     moisture/fertility (see `sprites::soil_tint`)
//!   - `TilledOverlayLayer` — a furrow-line overlay, visibility toggled
//!     by `soil.tilled` rather than baked into the soil texture
//!   - `CropLayer` — textured by the planted crop's current growth-
//!     stage name (or `"dead"`), hidden entirely when nothing's planted
//! This is what "soil visuals are layered, not flat per-combination
//! textures" (requirements §10) and "growth-stage art stays data-driven
//! per crop" mean in code — see `sprites.rs` for the asset-loading half.
//!
//! **Removed in this batch:** the old `tile_color`/`tile_letter`
//! functions and the `TileLabel` text-child approach — sprites now
//! carry that information visually, and the tile inspector panel
//! already surfaces the same state (crop name/stage/dead) as text for
//! anyone who wants it precisely, not just at-a-glance.
//!
//! **Compile-risk note:** `Sprite { image: Handle<Image>, .. }` as a
//! settable field (rather than a separate `SpriteBundle`+image
//! component) is matched from memory against Bevy 0.16's sprite API,
//! consistent with how the rest of this file already builds `Sprite`
//! via struct-update syntax. Also assuming `Visibility::Hidden`/
//! `Visibility::Visible` are usable as a plain component insert/update
//! (not requiring `InheritedVisibility` wiring) - flagging both since
//! `cargo check` isn't run in this workflow.
//!
//! Camera pan/zoom logic below this point is **unchanged** from the
//! previous batch - it operates on `Camera`/`Transform`/`Projection`,
//! not on what's drawn, so it was expected to survive this swap intact
//! and did.

use bevy::input::mouse::MouseWheel;
use bevy::prelude::*;
use bevy_egui::EguiContexts;

use crate::crops::{CropDef, PlantedCrop};
use crate::farm::{FarmConfig, SoilState, Tile, TileMap};

use super::sprites::{soil_tint, CropSpriteRoster, SoilSpriteRoster};
use super::SelectedTile;

/// World-space size of one tile's square, plus a small gap so tiles
/// read as a grid rather than a solid block. Both placeholders, same
/// spirit as the design doc's other §5-style tunables - not meant to
/// be final visual scale, just enough to click on individual tiles.
pub const TILE_SIZE: f32 = 12.0;
const TILE_GAP: f32 = 1.0;
const TILE_STRIDE: f32 = TILE_SIZE + TILE_GAP;

/// Zoom bounds. `MIN_ZOOM_FACTOR` (1.0) is "whole farm fills the
/// screen" - recomputed against the actual window size each frame (see
/// `fit_scale`), not a fixed pixel target, so it survives a resize
/// gracefully rather than drifting or needing a fixed reference
/// resolution. `MAX_ZOOM_FACTOR` is 16x more zoomed-in than that, per
/// your ask.
pub const MIN_ZOOM_FACTOR: f32 = 1.0;
pub const MAX_ZOOM_FACTOR: f32 = 16.0;

/// Fixed world-units/second pan speed for arrow-key panning,
/// independent of current zoom level. A §5-style placeholder - worth
/// tuning if it feels too fast zoomed in or too slow zoomed out.
const KEYBOARD_PAN_SPEED: f32 = 400.0;

/// Multiplicative zoom step per scroll-wheel "notch" (`MouseWheel.y`
/// unit) - `1.15^delta`, so scrolling several notches at once
/// compounds smoothly rather than linearly.
const SCROLL_ZOOM_STEP: f32 = 1.15;

/// Multiplicative zoom step for the panel's Zoom In/Out buttons - a
/// coarser, single-click-sized step compared to one scroll notch.
/// `pub` so `panels.rs` can use the same step size rather than
/// duplicating a slightly different constant.
pub const BUTTON_ZOOM_STEP: f32 = 1.5;

/// Camera pan/zoom state, decoupled from the actual `Transform`/
/// `Projection` on the camera entity so clamping and resize-handling
/// have one clear place to read/write from. `center` is a world-space
/// point (farm coordinates); `zoom_factor` is relative to
/// `MIN_ZOOM_FACTOR` (whole farm visible), not an absolute
/// `OrthographicProjection::scale` value - see `fit_scale` for how the
/// two relate.
#[derive(Resource, Debug, Clone)]
pub struct CameraState {
    pub center: Vec2,
    pub zoom_factor: f32,
    /// Cursor position (viewport pixels) as of last frame, while a drag
    /// is in progress - `None` when no drag is active. Used to compute
    /// a per-frame world-space delta rather than an absolute position,
    /// so the drag works regardless of where on screen it started.
    last_drag_cursor: Option<Vec2>,
}

impl Default for CameraState {
    fn default() -> Self {
        Self {
            center: Vec2::ZERO,
            zoom_factor: MIN_ZOOM_FACTOR,
            last_drag_cursor: None,
        }
    }
}

/// Public so `panels.rs` can compute a "reset view" target without
/// duplicating the tile-stride math or reaching into this module's
/// private constants.
pub fn farm_world_center(config: &FarmConfig) -> Vec2 {
    farm_world_size(config) / 2.0
}

fn farm_world_size(config: &FarmConfig) -> Vec2 {
    let width = config
        .width
        .clamp(FarmConfig::MIN_DIMENSION, FarmConfig::MAX_DIMENSION) as f32;
    let height = config
        .height
        .clamp(FarmConfig::MIN_DIMENSION, FarmConfig::MAX_DIMENSION) as f32;
    Vec2::new(width * TILE_STRIDE, height * TILE_STRIDE)
}

/// The `OrthographicProjection::scale` at which the whole farm is
/// visible in the given viewport (logical pixels) - i.e. `zoom_factor
/// == MIN_ZOOM_FACTOR`. Recomputed from the *current* window size every
/// time it's needed (not cached against a fixed reference resolution
/// the way the old placeholder camera was), which is what makes
/// zoom/pan behave sensibly across a resize rather than needing an
/// explicit resize-event handler.
fn fit_scale(world_size: Vec2, viewport_px: Vec2) -> f32 {
    (world_size.x / viewport_px.x.max(1.0))
        .max(world_size.y / viewport_px.y.max(1.0))
        .max(0.0001)
}

/// Camera at the farm's center, zoomed all the way out. `CameraState`
/// is initialized here too (rather than via `init_resource`) so its
/// starting `center` is the farm's actual center, not `Vec2::ZERO`
/// (which would be off-center for a non-square or non-origin-aligned
/// farm - not the case today, but no reason to bake in that
/// assumption).
///
/// **Compile-risk note (unchanged from before this batch):**
/// `OrthographicProjection::default_2d()` / `Projection::Orthographic`
/// are matched from memory against the Bevy 0.16 camera API.
pub fn setup_camera(mut commands: Commands, config: Res<FarmConfig>) {
    let world_size = farm_world_size(&config);
    let center = world_size / 2.0;

    commands.insert_resource(CameraState {
        center,
        zoom_factor: MIN_ZOOM_FACTOR,
        last_drag_cursor: None,
    });

    commands.spawn((
        Camera2d,
        Transform::from_xyz(center.x, center.y, 0.0),
        Projection::Orthographic(OrthographicProjection {
            // Real value gets set by `camera_apply_system` on the first
            // Update pass; this is just a reasonable starting guess
            // (matches the old placeholder camera's fixed-scale
            // approach) so there's no one-frame flash of a wildly wrong
            // zoom level before that system runs.
            scale: fit_scale(world_size, Vec2::new(900.0, 700.0)),
            ..OrthographicProjection::default_2d()
        }),
    ));
}

/// Reads scroll/keyboard/drag input and updates `CameraState` (center +
/// zoom_factor) accordingly. Does NOT touch the camera's actual
/// `Transform`/`Projection` - that's `camera_apply_system`'s job, kept
/// separate so this system can read the camera's last-frame
/// `GlobalTransform` (for drag math) without a query-mutability
/// conflict against writing it in the same system.
///
/// Ordered before `camera_apply_system` (see `ui/mod.rs`) so the
/// clamping/apply step always sees this frame's input already folded
/// in, not last frame's.
pub fn camera_input_system(
    mut contexts: EguiContexts,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut scroll_events: EventReader<MouseWheel>,
    mut state: ResMut<CameraState>,
    time: Res<Time>,
) {
    let egui_wants_pointer = contexts.ctx_mut().wants_pointer_input();
    let Ok(window) = windows.single() else {
        scroll_events.clear();
        return;
    };
    let Ok((camera, camera_transform)) = camera_q.single() else {
        scroll_events.clear();
        return;
    };

    // --- Zoom: scroll wheel ---
    // Skipped while egui wants pointer input (e.g. scrolling inside a
    // side panel) so panel scrolling and world zoom don't fight over
    // the same wheel events.
    if !egui_wants_pointer {
        let scroll_delta: f32 = scroll_events.read().map(|ev| ev.y).sum();
        if scroll_delta != 0.0 {
            let factor = SCROLL_ZOOM_STEP.powf(scroll_delta);
            state.zoom_factor = (state.zoom_factor * factor).clamp(MIN_ZOOM_FACTOR, MAX_ZOOM_FACTOR);
        }
    } else {
        scroll_events.clear();
    }

    // --- Pan: arrow keys ---
    // Fixed world-units/sec regardless of zoom (see const doc comment).
    let mut dir = Vec2::ZERO;
    if keys.pressed(KeyCode::ArrowLeft) {
        dir.x -= 1.0;
    }
    if keys.pressed(KeyCode::ArrowRight) {
        dir.x += 1.0;
    }
    if keys.pressed(KeyCode::ArrowUp) {
        dir.y += 1.0;
    }
    if keys.pressed(KeyCode::ArrowDown) {
        dir.y -= 1.0;
    }
    if dir != Vec2::ZERO {
        state.center += dir.normalize() * KEYBOARD_PAN_SPEED * time.delta_secs();
    }

    // --- Pan: middle-click drag, or left-click + Ctrl/Cmd drag ---
    // Checking both Ctrl and Super/Cmd (rather than OS-detecting) keeps
    // this cross-platform with one code path: Ctrl+drag works
    // everywhere, Cmd+drag additionally works on Mac.
    let modifier_held = keys.pressed(KeyCode::ControlLeft)
        || keys.pressed(KeyCode::ControlRight)
        || keys.pressed(KeyCode::SuperLeft)
        || keys.pressed(KeyCode::SuperRight);
    let drag_button_down =
        mouse.pressed(MouseButton::Middle) || (mouse.pressed(MouseButton::Left) && modifier_held);

    if !egui_wants_pointer && drag_button_down {
        if let Some(cursor) = window.cursor_position() {
            if let Some(last) = state.last_drag_cursor {
                // Convert both the previous and current cursor
                // positions to world space via the *current* camera
                // transform/projection, so the drag amount is correct
                // regardless of zoom level - dragging N screen pixels
                // moves fewer world units when zoomed in.
                if let (Ok(prev_world), Ok(cur_world)) = (
                    camera.viewport_to_world_2d(camera_transform, last),
                    camera.viewport_to_world_2d(camera_transform, cursor),
                ) {
                    // Content should follow the cursor, so the camera
                    // moves opposite the drag direction.
                    state.center -= cur_world - prev_world;
                }
            }
            state.last_drag_cursor = Some(cursor);
        }
    } else {
        state.last_drag_cursor = None;
    }
}

/// Writes `CameraState` (after this frame's input + panel-button
/// adjustments) to the camera's actual `Transform`/`Projection`,
/// clamping `center` to roughly the farm's extent first. Clamping is
/// recomputed from the *current* window size every frame (not cached),
/// which is what makes resize-survival work: the visible half-extent
/// shrinks/grows with the window, and `center` gets re-clamped against
/// that automatically rather than needing a dedicated resize-event
/// listener.
pub fn camera_apply_system(
    windows: Query<&Window>,
    mut camera_q: Query<(&mut Transform, &mut Projection), With<Camera2d>>,
    mut state: ResMut<CameraState>,
    config: Res<FarmConfig>,
) {
    let Ok(window) = windows.single() else {
        return;
    };
    let Ok((mut transform, mut projection)) = camera_q.single_mut() else {
        return;
    };

    let world_size = farm_world_size(&config);
    let viewport_px = Vec2::new(window.width(), window.height());
    let fit = fit_scale(world_size, viewport_px);
    let scale = fit / state.zoom_factor;

    // Half the world-space area actually visible at this scale.
    let half_view = viewport_px * scale * 0.5;

    // If the visible area is already >= the farm's extent on an axis
    // (typical at zoom_factor == MIN_ZOOM_FACTOR, or a very wide/short
    // window), there's no room to pan on that axis - clamp to dead
    // center instead of an inverted [min, max] range.
    let clamp_axis = |value: f32, half: f32, world: f32| -> f32 {
        if half * 2.0 >= world {
            world / 2.0
        } else {
            value.clamp(half, world - half)
        }
    };
    state.center.x = clamp_axis(state.center.x, half_view.x, world_size.x);
    state.center.y = clamp_axis(state.center.y, half_view.y, world_size.y);

    transform.translation.x = state.center.x;
    transform.translation.y = state.center.y;

    if let Projection::Orthographic(ortho) = &mut *projection {
        ortho.scale = scale;
    }
}

fn tile_world_pos(coord: IVec2) -> Vec2 {
    Vec2::new(coord.x as f32 * TILE_STRIDE, coord.y as f32 * TILE_STRIDE)
}

/// Marker for the soil-base child sprite of a tile.
#[derive(Component)]
pub struct SoilLayer;

/// Marker for the tilled-furrow-overlay child sprite of a tile.
#[derive(Component)]
pub struct TilledOverlayLayer;

/// Marker for the crop-art child sprite of a tile.
#[derive(Component)]
pub struct CropLayer;

/// Points from a tile entity to its three visual-layer children, so
/// `sync_tile_visuals` can update each without a separate parent/child
/// lookup pass per layer. Set once in `spawn_tile_visuals`.
#[derive(Component)]
pub struct TileVisualLayers {
    pub soil: Entity,
    pub tilled: Entity,
    pub crop: Entity,
}

/// Runs once per tile entity, the frame it's first seen (`Added<Tile>`).
/// Spawns the three sprite-layer children (soil / tilled overlay / crop
/// art), stacked via z-offset in that order so crop art always draws on
/// top of soil, and the tilled overlay sits between the two.
pub fn spawn_tile_visuals(
    mut commands: Commands,
    new_tiles: Query<(Entity, &Tile), Added<Tile>>,
    soil_sprites: Res<SoilSpriteRoster>,
) {
    for (entity, tile) in &new_tiles {
        let pos = tile_world_pos(tile.coord);
        commands.entity(entity).insert(Transform::from_translation(pos.extend(0.0)));

        let soil_entity = commands
            .spawn((
                SoilLayer,
                Sprite {
                    custom_size: Some(Vec2::splat(TILE_SIZE)),
                    ..default()
                },
                Transform::from_translation(Vec3::new(0.0, 0.0, 0.0)),
            ))
            .id();

        let tilled_entity = commands
            .spawn((
                TilledOverlayLayer,
                Sprite {
                    image: soil_sprites.tilled_overlay.clone(),
                    custom_size: Some(Vec2::splat(TILE_SIZE)),
                    ..default()
                },
                Transform::from_translation(Vec3::new(0.0, 0.0, 1.0)),
                Visibility::Hidden,
            ))
            .id();

        let crop_entity = commands
            .spawn((
                CropLayer,
                Sprite {
                    custom_size: Some(Vec2::splat(TILE_SIZE)),
                    ..default()
                },
                Transform::from_translation(Vec3::new(0.0, 0.0, 2.0)),
                Visibility::Hidden,
            ))
            .id();

        commands.entity(entity).add_children(&[soil_entity, tilled_entity, crop_entity]);
        commands.entity(entity).insert(TileVisualLayers {
            soil: soil_entity,
            tilled: tilled_entity,
            crop: crop_entity,
        });
    }
}

/// Refreshes every tile's three visual layers every frame - brute-force
/// (recomputes and reassigns even when nothing changed) rather than
/// change-detected, same tradeoff the old flat-color version made:
/// simplicity over the added complexity of tracking exactly which field
/// changed, at a tile count where this hasn't been a measured problem.
pub fn sync_tile_visuals(
    tiles: Query<(&SoilState, Option<&PlantedCrop>, &TileVisualLayers)>,
    mut sprites: Query<&mut Sprite>,
    mut visibilities: Query<&mut Visibility>,
    soil_sprites: Res<SoilSpriteRoster>,
    crop_sprites: Res<CropSpriteRoster>,
    crop_defs: Res<Assets<CropDef>>,
) {
    for (soil, planted, layers) in &tiles {
        // --- Soil base: texture by SoilType, tinted by moisture/fertility ---
        if let Ok(mut sprite) = sprites.get_mut(layers.soil) {
            if let Some(handle) = soil_sprites.by_type.get(&soil.soil_type) {
                if sprite.image != *handle {
                    sprite.image = handle.clone();
                }
            }
            sprite.color = soil_tint(soil.fertility, soil.moisture);
        }

        // --- Tilled overlay: visibility only, image is set once at spawn ---
        if let Ok(mut vis) = visibilities.get_mut(layers.tilled) {
            *vis = if soil.tilled { Visibility::Visible } else { Visibility::Hidden };
        }

        // --- Crop art: dead / current-stage-name / hidden ---
        let mut crop_visual: Option<Handle<Image>> = None;
        if let Some(crop) = planted {
            if let Some(def) = crop_defs.get(&crop.def) {
                let stage_key = if crop.dead {
                    Some("dead".to_string())
                } else {
                    def.growth_stages.get(crop.current_stage).map(|s| s.name.clone())
                };
                if let Some(key) = stage_key {
                    crop_visual = crop_sprites.get(&def.id, &key).cloned();
                }
            }
        }

        if let Ok(mut sprite) = sprites.get_mut(layers.crop) {
            if let Some(handle) = &crop_visual {
                if sprite.image != *handle {
                    sprite.image = handle.clone();
                }
            }
        }
        if let Ok(mut vis) = visibilities.get_mut(layers.crop) {
            *vis = if crop_visual.is_some() { Visibility::Visible } else { Visibility::Hidden };
        }
    }
}

/// Click-to-select (plain left click, no modifier). Skips the click if
/// egui wants pointer input that frame, same as before.
///
/// Also skips if Ctrl/Cmd is held, since that combination is claimed by
/// `camera_input_system` for drag-panning - without this, a
/// modifier+click-without-moving would both pan (by zero) and select a
/// tile, a confusing double-purpose for the same click. A plain,
/// un-modified left click still selects a tile exactly as before.
pub fn tile_click_system(
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut selected: ResMut<SelectedTile>,
    tile_map: Res<TileMap>,
    mut contexts: EguiContexts,
) {
    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    if contexts.ctx_mut().wants_pointer_input() {
        return;
    }
    let modifier_held = keys.pressed(KeyCode::ControlLeft)
        || keys.pressed(KeyCode::ControlRight)
        || keys.pressed(KeyCode::SuperLeft)
        || keys.pressed(KeyCode::SuperRight);
    if modifier_held {
        return;
    }

    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    let Ok((camera, camera_transform)) = camera_q.single() else {
        return;
    };
    let Ok(world_pos) = camera.viewport_to_world_2d(camera_transform, cursor) else {
        return;
    };

    // Tiles are rendered center-anchored at `coord * TILE_STRIDE` (see
    // `tile_world_pos` / `spawn_tile_visuals`), not corner-anchored at
    // `coord * TILE_STRIDE` with the tile spanning forward from there.
    // `.floor()` would hit-test the latter (a grid of corner-origin
    // cells), which is a half-tile off from what's actually drawn -
    // clicking near a tile's left/bottom edge would silently select its
    // lower-left neighbor instead. Rounding to the nearest tile center
    // matches the render grid instead.
    let coord = IVec2::new(
        (world_pos.x / TILE_STRIDE).round() as i32,
        (world_pos.y / TILE_STRIDE).round() as i32,
    );

    if tile_map.tiles.contains_key(&coord) {
        selected.0 = Some(coord);
    }
}
