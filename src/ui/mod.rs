//! UI (design doc §1's `ui/` module note: "reads from the others but
//! doesn't own simulation state"; requirements §4.0's light/dark theme
//! requirement; §4.6a's FIFO-only queue display) - Phase 6, plus a
//! camera pan/zoom addition (see `render.rs`'s module doc), a save/load
//! panel (Phase 7), and now real sprite-based tile rendering (see
//! `sprites.rs`'s module doc) replacing the flat-colored-square
//! placeholder from Phase 6.
//!
//! Three submodules:
//! - `sprites`: soil-texture and crop-growth-stage-art asset loading,
//!   plus the moisture/fertility tint function. New this batch.
//! - `render`: the tile grid (now sprite-layered, see its module doc),
//!   click-to-select, and camera pan/zoom.
//! - `panels`: the egui panels themselves (time control, tile inspector,
//!   action queue, economy/sell, camera zoom buttons, save/load).
//!
//! **Phase 8 addition (unchanged this batch):** the save/load window
//! used to be permanently open. It's now gated behind
//! `SaveLoadWindowOpen`, toggled by a top-bar button, and egui's own
//! `Window::open(&mut bool)` wires the window's built-in close button
//! (×) straight back to that same resource.

pub mod panels;
pub mod render;
pub mod sprites;

use bevy::prelude::*;
use bevy_egui::{EguiContextPass, EguiPlugin};
use serde::{Deserialize, Serialize};

pub use render::CameraState;
pub use sprites::CropSpriteRoster;

/// Currently-clicked tile, read by every tile-targeted panel (inspector,
/// action queue). `None` until the player clicks a tile for the first
/// time. Lives here rather than in `render` since panels (not just
/// rendering) need to read it.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct SelectedTile(pub Option<IVec2>);

/// Light/dark toggle (requirements §4.0: both first-class, "Default
/// theme TBD, likely Light").
///
/// **Phase 7:** now actually persisted, per the memory note this was
/// "already structured as a plain resource for easy persistence" -
/// `Serialize`/`Deserialize` added, and `save::SaveGame` carries it
/// directly.
#[derive(Resource, Debug, Clone, Copy, Serialize, Deserialize)]
pub struct UiTheme {
    pub dark: bool,
}

impl Default for UiTheme {
    fn default() -> Self {
        Self { dark: false } // Light default, per requirements §4.0.
    }
}

/// Whether the floating Save/Load window is currently shown. Phase 8
/// addition - previously the window was always open with no toggle;
/// this makes it dismissible, off by default, and reopenable from a top
/// bar button (`panels::top_bar`). Deliberately not persisted across
/// saves - whether the window happens to be open isn't game state.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct SaveLoadWindowOpen(pub bool);

/// Whether tiles are drawn as flat soil-type colours (design doc §6.7)
/// instead of textured/tinted soil, so the player can see the soil layout
/// at a glance. Not persisted.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct ShowSoilOverlay(pub bool);

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(EguiPlugin {
            enable_multipass_for_primary_context: true,
        })
            .init_resource::<SelectedTile>()
            .init_resource::<UiTheme>()
            .init_resource::<SaveLoadWindowOpen>()
            .init_resource::<ShowSoilOverlay>()
            .init_resource::<render::PanelInsets>()
            .init_resource::<CropSpriteRoster>()
            .add_systems(Startup, (render::setup_camera, sprites::load_soil_sprites))
            .add_systems(
                Update,
                (
                    sprites::load_crop_sprites,
                    render::camera_input_system,
                    render::camera_apply_system,
                    render::spawn_tile_visuals,
                    render::sync_tile_visuals,
                    render::tile_click_system,
                )
                    .chain(),
            )
            .add_systems(
                EguiContextPass,
                (
                    panels::top_bar,
                    panels::tile_inspector_panel,
                    panels::action_queue_panel,
                    panels::economy_panel,
                    // Save/load panel: gated by SaveLoadWindowOpen
                    // (Phase 8) rather than always-open. Kept last for
                    // the same readability reason as before.
                    panels::save_load_panel,
                    render::record_panel_insets,
                )
                    .chain(),
            );
    }
}
