//! Placeholder tile rendering + click selection + camera pan/zoom.
//!
//! **Pan/zoom note (overrides an earlier deferral):** the design doc
//! (§3.5) and requirements doc (§10) originally deferred camera pan/zoom
//! to a future rendering/art-pipeline phase, on the reasoning that
//! building camera controls against placeholder squares would be
//! throwaway work. In practice, a fixed camera on a farm of thousands of
//! tiles made playtesting painful (easy to miss a tile entirely), so
//! pan/zoom is being built now, against the placeholder rendering.
//! Camera control logic operates on `Camera`/`Transform`/`Projection`,
//! not on what's drawn, so it should mostly survive the later swap to
//! real sprites intact - this isn't expected to be thrown away the way
//! e.g. tile-visual code might be.
//!
//! Everything else in this module (flat colored `Sprite` squares, a
//! `Text2d` letter label) is unchanged and still NOT the real art
//! pipeline - see `ui/mod.rs`'s module doc.

use bevy::input::mouse::MouseWheel;
use bevy::prelude::*;
use bevy_egui::EguiContexts;

use crate::crops::{CropDef, PlantedCrop};
use crate::farm::{FarmConfig, SoilState, SoilType, Tile, TileMap};

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

/// Points from a tile entity to its child label entity, so
/// `sync_tile_visuals` can update the label's `Text2d` without a
/// separate lookup pass. Set once in `spawn_tile_visuals`.
#[derive(Component)]
pub struct TileLabel(pub Entity);

/// Runs once per tile entity, the frame it's first seen (`Added<Tile>`).
pub fn spawn_tile_visuals(mut commands: Commands, new_tiles: Query<(Entity, &Tile), Added<Tile>>) {
    for (entity, tile) in &new_tiles {
        let pos = tile_world_pos(tile.coord);
        commands.entity(entity).insert((
            Sprite {
                custom_size: Some(Vec2::splat(TILE_SIZE)),
                ..default()
            },
            Transform::from_translation(pos.extend(0.0)),
        ));

        let label_entity = commands
            .spawn((
                Text2d::new(""),
                TextFont {
                    font_size: 8.0,
                    ..default()
                },
                TextColor(Color::BLACK),
                Transform::from_translation(Vec3::new(0.0, 0.0, 1.0)),
            ))
            .id();
        commands.entity(entity).add_child(label_entity);
        commands.entity(entity).insert(TileLabel(label_entity));
    }
}

/// Refreshes every tile's color + label every frame. Unchanged from
/// before this batch - see the original doc comment for why this is
/// brute-force rather than change-detected.
pub fn sync_tile_visuals(
    mut tiles: Query<(&SoilState, Option<&PlantedCrop>, &TileLabel, &mut Sprite)>,
    mut texts: Query<&mut Text2d>,
    crop_defs: Res<Assets<CropDef>>,
) {
    for (soil, planted, label, mut sprite) in &mut tiles {
        let color = tile_color(soil, planted);
        if sprite.color != color {
            sprite.color = color;
        }

        let letter = tile_letter(soil, planted, &crop_defs);
        if let Ok(mut text) = texts.get_mut(label.0) {
            if text.0 != letter {
                text.0 = letter.to_string();
            }
        }
    }
}

fn tile_color(soil: &SoilState, planted: Option<&PlantedCrop>) -> Color {
    if let Some(crop) = planted {
        if crop.dead {
            return Color::srgb(0.25, 0.05, 0.05);
        }
    }

    let base = match soil.soil_type {
        SoilType::Sandy => Color::srgb(0.82, 0.71, 0.45),
        SoilType::Loamy => Color::srgb(0.42, 0.28, 0.16),
        SoilType::Clay => Color::srgb(0.55, 0.35, 0.30),
    };

    if soil.tilled {
        let srgba = base.to_srgba();
        Color::srgb(srgba.red * 0.75, srgba.green * 0.75, srgba.blue * 0.75)
    } else {
        base
    }
}

fn tile_letter(soil: &SoilState, planted: Option<&PlantedCrop>, crop_defs: &Assets<CropDef>) -> String {
    if let Some(crop) = planted {
        if crop.dead {
            return "X".to_string();
        }
        return match crop_defs.get(&crop.def) {
            Some(def) => crop_initial(&def.id),
            None => "?".to_string(),
        };
    }

    if soil.tilled {
        ".".to_string()
    } else {
        String::new()
    }
}

fn crop_initial(crop_id: &str) -> String {
    crop_id
        .chars()
        .next()
        .map(|c| c.to_ascii_uppercase().to_string())
        .unwrap_or_else(|| "?".to_string())
}

/// Click-to-select (plain left click, no modifier). Skips the click if
/// egui wants pointer input that frame, same as before.
///
/// **New in this batch:** also skips if Ctrl/Cmd is held, since that
/// combination is now claimed by `camera_input_system` for drag-panning
/// - without this, a modifier+click-without-moving would both pan
/// (by zero) and select a tile, which is a confusing double-purpose for
/// the same click. A plain, un-modified left click still selects a
/// tile exactly as before.
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

    let coord = IVec2::new(
        (world_pos.x / TILE_STRIDE).floor() as i32,
        (world_pos.y / TILE_STRIDE).floor() as i32,
    );

    if tile_map.tiles.contains_key(&coord) {
        selected.0 = Some(coord);
    }
}
