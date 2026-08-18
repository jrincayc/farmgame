//! Time & calendar (design doc §2.1, §2.1a; requirements §4.1).
//!
//! This module's only job is deciding *when* a day advances and
//! broadcasting that via the `DayAdvanced` event. It owns no simulation
//! state beyond the day counter itself — soil, crops, weather, and
//! economy all live in their own plugins and react to `DayAdvanced`
//! rather than running on a real-time tick (design doc §1.1).

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// Absolute day count since game start, plus the length of a "year" for
/// deriving season-like labels elsewhere. Intentionally dumb: it just
/// counts (design doc §2.1).
///
/// **Phase 7 addition:** `Serialize`/`Deserialize` so `save::SaveGame`
/// can persist it directly, no separate `SavedCalendar` mirror needed —
/// unlike `PlantedCrop`/`ActionQueue`, nothing here references an
/// `Entity` or asset `Handle`, so a straight derive is safe.
#[derive(Resource, Debug, Clone, Serialize, Deserialize)]
pub struct Calendar {
    pub current_day: u32,
    pub year_length: u32,
}

impl Default for Calendar {
    fn default() -> Self {
        Self {
            current_day: 0,
            // 28 days/season * 4 seasons, per design doc §2.1 example.
            // A placeholder like the other numbers flagged in design doc §5.
            year_length: 112,
        }
    }
}

/// Fired once whenever the simulation should advance by exactly one day.
///
/// `new_day` is the day about to become "current". Downstream listeners
/// (climate, farm, crops, economy — added in later phases) should read
/// `new_day` from this event rather than `Res<Calendar>`, since
/// `Calendar` itself isn't updated until `apply_day_increment` runs last
/// in the chain (design doc §1.1's diagram).
#[derive(Event, Debug, Clone, Copy)]
pub struct DayAdvanced {
    pub new_day: u32,
}

/// Fire this (e.g. from a UI button handler) to request a single manual
/// day-step. Bypasses `TimeControl.mode` entirely — Step always fires
/// `DayAdvanced` once, regardless of Paused/Timer (design doc §2.1a).
#[derive(Event, Debug, Clone, Copy, Default)]
pub struct StepDayRequested;

#[derive(Resource, Debug, Clone)]
pub struct TimeControl {
    pub mode: TimeMode,
}

impl Default for TimeControl {
    fn default() -> Self {
        Self {
            mode: TimeMode::Paused,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TimeMode {
    Paused,
    Timer { speed: TimerSpeed, elapsed_ms: f32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerSpeed {
    Slow,
    Normal,
    Fast,
}

impl TimerSpeed {
    /// Real-time milliseconds per game day at this speed. Placeholder
    /// values (design doc §2.1a) — a playtesting question, not an
    /// architecture one (design doc §5).
    pub fn duration_ms(self) -> f32 {
        match self {
            TimerSpeed::Slow => 60_000.0,
            TimerSpeed::Normal => 20_000.0,
            TimerSpeed::Fast => 5_000.0,
        }
    }
}

/// Ordering labels for this module's own systems. Later plugins
/// (climate, farm, crops, economy) will order their `DayAdvanced`
/// listeners relative to these — specifically, after `Trigger` and
/// before `Apply` — to match the design doc §1.1 chain, where
/// `time::increment_day` runs last.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimeSystems {
    /// Systems that may fire `DayAdvanced` this frame (Timer countdown,
    /// manual Step requests).
    Trigger,
    /// Runs after every `DayAdvanced` listener; commits the new day to
    /// `Calendar` (design doc §1.1: `time::increment_day` is last).
    Apply,
}

pub struct TimePlugin;

impl Plugin for TimePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Calendar>()
            .init_resource::<TimeControl>()
            .add_event::<DayAdvanced>()
            .add_event::<StepDayRequested>()
            .configure_sets(Update, (TimeSystems::Trigger, TimeSystems::Apply).chain())
            .add_systems(
                Update,
                (tick_timer_mode, handle_step_requests).in_set(TimeSystems::Trigger),
            )
            .add_systems(Update, apply_day_increment.in_set(TimeSystems::Apply));
    }
}

/// Timer mode: accumulate real elapsed time and fire `DayAdvanced` once
/// the current `TimerSpeed`'s threshold is reached, then reset the
/// accumulator. Does nothing while Paused (design doc §2.1a).
///
/// Uses a `while` loop (not `if`) so that a single very-long frame (a
/// stutter, or the window regaining focus after being backgrounded)
/// correctly fires multiple `DayAdvanced` events to catch up, each with
/// its own sequential `new_day`, rather than silently dropping days or
/// reporting the same `new_day` twice.
fn tick_timer_mode(
    time: Res<Time>,
    mut control: ResMut<TimeControl>,
    calendar: Res<Calendar>,
    mut day_advanced: EventWriter<DayAdvanced>,
) {
    let TimeMode::Timer { speed, elapsed_ms } = &mut control.mode else {
        return;
    };

    *elapsed_ms += time.delta_secs() * 1000.0;
    let mut next_day = calendar.current_day;
    while *elapsed_ms >= speed.duration_ms() {
        *elapsed_ms -= speed.duration_ms();
        next_day += 1;
        day_advanced.write(DayAdvanced { new_day: next_day });
    }
}

/// Step / manual advance: fires immediately on request, independent of
/// `TimeControl.mode` (design doc §2.1a).
///
/// If more than one `StepDayRequested` lands in the same frame, each
/// still advances by exactly one day (sequential `new_day` values)
/// rather than collapsing into a single advance.
fn handle_step_requests(
    mut requests: EventReader<StepDayRequested>,
    calendar: Res<Calendar>,
    mut day_advanced: EventWriter<DayAdvanced>,
) {
    let mut next_day = calendar.current_day;
    for _ in requests.read() {
        next_day += 1;
        day_advanced.write(DayAdvanced { new_day: next_day });
    }
}

/// Last step in the `DayAdvanced` chain (design doc §1.1): commits the
/// new day to `Calendar`. If multiple `DayAdvanced` events landed this
/// frame (see `tick_timer_mode`/`handle_step_requests`), their `new_day`
/// values are already sequential, so simply taking the last one applied
/// lands on the correct final day.
fn apply_day_increment(mut events: EventReader<DayAdvanced>, mut calendar: ResMut<Calendar>) {
    for ev in events.read() {
        calendar.current_day = ev.new_day;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(TimePlugin);
        app
    }

    #[test]
    fn step_request_advances_calendar_by_one() {
        let mut app = test_app();
        app.update(); // let Time initialize

        app.world_mut().send_event(StepDayRequested);
        app.update();

        assert_eq!(app.world().resource::<Calendar>().current_day, 1);
    }

    #[test]
    fn paused_mode_never_advances_on_its_own() {
        let mut app = test_app();
        for _ in 0..5 {
            app.update();
        }
        assert_eq!(app.world().resource::<Calendar>().current_day, 0);
    }

    #[test]
    fn multiple_step_requests_in_one_frame_each_advance_one_day() {
        let mut app = test_app();
        app.update();

        app.world_mut().send_event(StepDayRequested);
        app.world_mut().send_event(StepDayRequested);
        app.update();

        // Two requests queued before the same Update pass should each
        // count as a full day advance, not collapse into one.
        assert_eq!(app.world().resource::<Calendar>().current_day, 2);
    }
}
