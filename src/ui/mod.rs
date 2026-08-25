//! UI (design doc §1's `ui/` module note: "reads from the others but
//! doesn't own simulation state"; requirements §4.0's light/dark theme
//! requirement; §4.6a's FIFO-only queue display) - **Phase 6**, plus a
//! camera pan/zoom addition (see `render.rs`'s module doc) and (Phase 7)
//! a save/load panel (see `panels::save_load_panel`).
//!
//! Two halves, split across submodules:
//! - `render`: a placeholder top-down tile grid (flat colored squares +
//!   a text-letter label per tile), click-to-select, and camera
//!   pan/zoom. Still explicitly *not* the real art pipeline.
//! - `panels`: the egui panels themselves (time control, tile inspector,
//!   action queue, economy/sell, camera zoom buttons, save/load).
//!
//! **Phase 8 addition:** the save/load window used to be permanently
//! open (a floating egui window with no way to dismiss it - annoying
//! since it sat on screen the whole session). It's now gated behind
//! `SaveLoadWindowOpen`, toggled by a top-bar button, and egui's own
//! `Window::open(&mut bool)` wires the window's built-in close button
//! (×) straight back to that same resource - no separate close-button
//! plumbing needed.

pub mod panels;
pub mod render;

use bevy::prelude::*;
use bevy_egui::{EguiContextPass, EguiPlugin};
use serde::{Deserialize, Serialize};

pub use render::CameraState;

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

/// Whether the floating Save/Load window is currently shown. **Phase 8
/// addition** - previously the window was always open with no toggle;
/// this makes it dismissible, off by default, and reopenable from a top
/// bar button (`panels::top_bar`). Deliberately not persisted across
/// saves - whether the window happens to be open isn't game state.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct SaveLoadWindowOpen(pub bool);

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(EguiPlugin {
            enable_multipass_for_primary_context: true,
        })
            .init_resource::<SelectedTile>()
            .init_resource::<UiTheme>()
            .init_resource::<SaveLoadWindowOpen>()
            .add_systems(Startup, render::setup_camera)
            .add_systems(
                Update,
                (
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
                    // Save/load panel: now gated by SaveLoadWindowOpen
                    // (Phase 8) rather than always-open (Phase 7). Kept
                    // last for the same readability reason as before.
                    panels::save_load_panel,
                )
                    .chain(),
            );
    }
}
