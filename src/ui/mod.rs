//! UI (design doc §1's `ui/` module note: "reads from the others but
//! doesn't own simulation state"; requirements §4.0's light/dark theme
//! requirement; §4.6a's FIFO-only queue display) - **Phase 6.**
//!
//! Two halves, split across submodules:
//! - `render`: a placeholder top-down tile grid (flat colored squares +
//!   a text-letter label per tile) and click-to-select. This is
//!   explicitly *not* the real art pipeline - per the design discussion
//!   that preceded this phase, real sprite-based rendering (growth-stage
//!   art, layered soil overlays) is its own later phase. This phase only
//!   needed *something* clickable so tile-targeted UI wasn't meaningless.
//! - `panels`: the egui panels themselves (time control, tile inspector,
//!   action queue, economy/sell).
//!
//! **Highest compile-risk file in this batch:** `bevy_egui` changed its
//! multi-pass API right around 0.34, and I don't have a compiler in the
//! loop. I checked the actual `v0.34.1` tag's own examples on GitHub
//! (not a later version's docs, which use a renamed
//! `EguiPrimaryContextPass`) to confirm this file uses `EguiContextPass`
//! and a non-fallible `contexts.ctx_mut()` - but this wasn't run against
//! the crate itself. If `ui/panels.rs` or this file fails to compile,
//! start here and in `render.rs`'s `Text2d`/`OrthographicProjection`
//! usage - both noted inline where the risk is highest.

pub mod panels;
pub mod render;

use bevy::prelude::*;
use bevy_egui::{EguiContextPass, EguiPlugin};

/// Currently-clicked tile, read by every tile-targeted panel (inspector,
/// action queue). `None` until the player clicks a tile for the first
/// time. Lives here rather than in `render` since panels (not just
/// rendering) need to read it.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct SelectedTile(pub Option<IVec2>);

/// Light/dark toggle (requirements §4.0: both first-class, "Default
/// theme TBD, likely Light"). Not yet persisted across runs - Phase 7
/// (save/load) can read this resource once it exists without this
/// module needing to change.
#[derive(Resource, Debug, Clone, Copy)]
pub struct UiTheme {
    pub dark: bool,
}

impl Default for UiTheme {
    fn default() -> Self {
        Self { dark: false } // Light default, per requirements §4.0.
    }
}

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(EguiPlugin { enable_multipass_for_primary_context: true })
            .init_resource::<SelectedTile>()
            .init_resource::<UiTheme>()
            .add_systems(Startup, render::setup_camera)
            .add_systems(
                Update,
                (render::spawn_tile_visuals, render::sync_tile_visuals, render::tile_click_system).chain(),
            )
            // Ordered so panel layout is sensible (top bar first claims
            // the top strip, then side panels, then the bottom economy
            // bar) - order here doesn't affect correctness, just how
            // egui carves up remaining screen space between panels.
            .add_systems(
                EguiContextPass,
                (
                    panels::top_bar,
                    panels::tile_inspector_panel,
                    panels::action_queue_panel,
                    panels::economy_panel,
                )
                    .chain(),
            );
    }
}
