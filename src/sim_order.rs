//! Cross-module ordering for systems that react to `DayAdvanced` within
//! the same frame it fires (design doc §1.1's diagram: climate → farm →
//! crops (queue, growth, failure) → economy → `time::increment_day`).
//!
//! Each phase's plugin places its own `DayAdvanced`-reactive systems
//! into the matching `DayAdvanceOrder` variant below; `configure` — called
//! once from `main.rs` — chains those sets so the sequence is pinned
//! explicitly rather than left to system-registration order, per the
//! design doc's stated intent. Variants for modules that don't exist yet
//! are commented out as a forward reference; uncomment and wire them up
//! as each phase lands.

use bevy::prelude::*;

use crate::time::TimeSystems;

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DayAdvanceOrder {
    /// climate::advance_day (Phase 2)
    Climate,
    /// farm::degrade_soil (Phase 3)
    Farm,
    // /// crops::process_action_queue (Phase 4)
    // CropsQueue,
    // /// crops::tick_growth (Phase 4)
    // CropsGrowth,
    // /// crops::check_failure (Phase 4)
    // CropsFailure,
    // /// economy::update_prices (Phase 5)
    // Economy,
}

/// Wires the `DayAdvanceOrder` sets between `TimeSystems::Trigger` (which
/// may fire `DayAdvanced` this frame) and `TimeSystems::Apply` (which
/// commits the new day to `Calendar` last), so every `DayAdvanced`
/// listener sees the event the same frame it's fired and runs in the
/// order the design doc specifies.
pub fn configure(app: &mut App) {
    app.configure_sets(
        Update,
        (
            TimeSystems::Trigger,
            DayAdvanceOrder::Climate,
            DayAdvanceOrder::Farm,
            TimeSystems::Apply,
        )
            .chain(),
    );
}
