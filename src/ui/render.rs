//! Placeholder tile rendering + click selection (Phase 6). Deliberately
//! NOT the real art pipeline (see `ui/mod.rs`'s module doc and the
//! design discussion that preceded this phase) - flat colored `Sprite`
//! squares with a text-letter label, just enough to make tile-targeted
//! UI (inspector, action queue) meaningful before real growth-stage art
//! and layered soil overlays exist as their own later phase.
//!
//! **Fixed camera, no pan/zoom** (explicit scope decision - pan/zoom
//! was deferred to the future rendering phase alongside real sprites,
//! since building camera controls against placeholder squares felt like
//! throwaway work).

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

/// Points from a tile entity to its child label entity, so
/// `sync_tile_visuals` can update the label's `Text2d` without a
/// separate lookup pass. Set once in `spawn_tile_visuals`.
#[derive(Component)]
pub struct TileLabel(pub Entity);

fn tile_world_pos(coord: IVec2) -> Vec2 {
    Vec2::new(coord.x as f32 * TILE_STRIDE, coord.y as f32 * TILE_STRIDE)
}

/// Fixed camera framed to the default farm size (design decision above)
/// - not resize- or farm-size-reactive beyond what's computed once here
/// at Startup. `FarmConfig` is read (not hardcoded) so a non-default
/// farm size at least frames reasonably, even without real pan/zoom.
///
/// **Compile-risk note:** `OrthographicProjection::default_2d()` and the
/// `Projection::Orthographic(..)` wrapping are matched from memory
/// against the Bevy 0.16 camera API - if this doesn't compile, the
/// fallback is spawning bare `Camera2d` and adjusting `scale` via a
/// separate `Query<&mut Projection>` in a follow-up system instead of
/// setting it inline at spawn time.
pub fn setup_camera(mut commands: Commands, config: Res<FarmConfig>) {
    let width = config
        .width
        .clamp(FarmConfig::MIN_DIMENSION, FarmConfig::MAX_DIMENSION) as f32;
    let height = config
        .height
        .clamp(FarmConfig::MIN_DIMENSION, FarmConfig::MAX_DIMENSION) as f32;
    let world_width = width * TILE_STRIDE;
    let world_height = height * TILE_STRIDE;

    // Target pixel dimensions are placeholders matching a typical
    // default window size, not read from the actual `Window` - a
    // resize-reactive camera is exactly the kind of polish the future
    // rendering phase should own, not this one.
    let scale = (world_width / 900.0_f32).max(world_height / 700.0_f32).max(1.0);

    commands.spawn((
        Camera2d,
        Transform::from_xyz(world_width / 2.0, world_height / 2.0, 0.0),
        Projection::Orthographic(OrthographicProjection {
            scale,
            ..OrthographicProjection::default_2d()
        }),
    ));
}

/// Runs once per tile entity, the frame it's first seen (`Added<Tile>`)
/// - fires once after `farm::spawn_tiles` (Startup) populates the world,
/// without this module needing a direct cross-plugin Startup-ordering
/// dependency on that private function.
///
/// **Compile-risk note:** `Text2d`/`TextFont`/`TextColor` field/method
/// shapes are matched from memory against Bevy 0.16's 2D text API - if
/// `Text2d::new(..)` or `text.0 = ..` (in `sync_tile_visuals` below)
/// don't compile, check whether `Text2d` wraps a `String` as a public
/// tuple field vs. exposing a setter method instead.
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
                // Small positive Z so the label draws above its parent
                // tile's sprite rather than z-fighting with it.
                Transform::from_translation(Vec3::new(0.0, 0.0, 1.0)),
            ))
            .id();
        commands.entity(entity).add_child(label_entity);
        commands.entity(entity).insert(TileLabel(label_entity));
    }
}

/// Refreshes every tile's color + label every frame. **Deliberately
/// brute-force rather than change-detected:** a tile's visual depends on
/// both `SoilState` (changes daily via `degrade_soil`) and the
/// *presence/absence* of `PlantedCrop` (changes on Plant/Harvest/Clear,
/// not `Changed<SoilState>`), so correctly change-detecting both without
/// missing an update would need `RemovedComponents<PlantedCrop>`
/// handling too. At MVP farm scale (a few thousand tiles, design doc
/// §5) recomputing every tile's target color/label each frame and only
/// writing when it actually differs is cheap enough not to be worth
/// that complexity - a simplicity-over-perf tradeoff to revisit if the
/// future rendering phase's real sprites make per-frame iteration
/// meaningfully more expensive than this placeholder.
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
            // Design doc §2.4: death must be visible, not silent - a
            // distinct color independent of soil type is the cheapest
            // way to satisfy that with flat squares.
            return Color::srgb(0.25, 0.05, 0.05);
        }
    }

    let base = match soil.soil_type {
        SoilType::Sandy => Color::srgb(0.82, 0.71, 0.45),
        SoilType::Loamy => Color::srgb(0.42, 0.28, 0.16),
        SoilType::Clay => Color::srgb(0.55, 0.35, 0.30),
    };

    if soil.tilled {
        // Slightly darker to suggest turned earth - a placeholder cue
        // standing in for real furrow art (see the reference sheet
        // discussed before this phase), not meant to be the final look.
        let srgba = base.to_srgba();
        Color::srgb(srgba.red * 0.75, srgba.green * 0.75, srgba.blue * 0.75)
    } else {
        base
    }
}

/// Text label per tile: crop initial if planted (matches the C/G/T/W
/// scheme requested for Phase 6), `X` if the crop died, `.` for tilled
/// bare soil, blank otherwise. Intentionally not soil-type letters for
/// bare/fallow tiles - soil type is already color-coded (`tile_color`
/// above), so a redundant letter there would be visual noise.
fn tile_letter(soil: &SoilState, planted: Option<&PlantedCrop>, crop_defs: &Assets<CropDef>) -> &'static str {
    if let Some(crop) = planted {
        if crop.dead {
            return "X";
        }
        return match crop_defs.get(&crop.def) {
            Some(def) => crop_initial(&def.id),
            None => "?", // asset still streaming in
        };
    }

    if soil.tilled {
        "."
    } else {
        ""
    }
}

/// Maps the starter roster's `CropDef::id` to its display letter. Not
/// data-driven (unlike everything else about crops) since this is
/// throwaway placeholder art, not a real asset reference - the future
/// rendering phase's data-driven sprite lookup replaces this outright
/// rather than extending it.
fn crop_initial(crop_id: &str) -> &'static str {
    match crop_id {
        "tomato" => "T",
        "wheat" => "W",
        "carrot" => "C",
        "green_bean" => "G",
        _ => "?",
    }
}

/// Click-to-select (left mouse button). Skips the click if egui itself
/// wants pointer input that frame (e.g. the player clicked a panel
/// button), so clicking through a side panel onto the grid behind it
/// doesn't also reselect a tile.
///
/// **Compile-risk note:** `Camera::viewport_to_world_2d` and
/// `Window::cursor_position` signatures are matched from memory against
/// Bevy 0.16 - both are long-stable APIs so this is lower risk than the
/// camera/text notes above, but flagging per this batch's convention.
pub fn tile_click_system(
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
    mouse: Res<ButtonInput<MouseButton>>,
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
