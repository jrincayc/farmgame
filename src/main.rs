//! Farm sim entry point.
//!
//! Phase 1 scope: just the time/calendar skeleton (design doc §1, §2.1,
//! §2.1a). No rendering of the farm itself yet — this exists to prove
//! the plugin wiring and `DayAdvanced` event chain work before climate,
//! farm, and crops build on top of it in later phases.
//!
//! Run with `cargo run`. With no UI yet, `TimeControl` defaults to
//! Paused, so nothing will happen on its own — press keys per the log
//! line printed at startup to drive it manually for now.

use bevy::prelude::*;

mod time;

use time::{Calendar, DayAdvanced, StepDayRequested, TimeControl, TimeMode, TimePlugin, TimerSpeed};

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Farm Sim (dev)".to_string(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(TimePlugin)
        .add_systems(Startup, print_dev_controls)
        .add_systems(Update, (dev_keyboard_controls, log_day_advanced))
        .run();
}

fn print_dev_controls() {
    info!("=== Farm Sim dev build (Phase 1: time skeleton) ===");
    info!("No UI yet - temporary keyboard controls:");
    info!("  Space : request a single day Step");
    info!("  1 / 2 / 3 : switch to Timer mode at Slow / Normal / Fast");
    info!("  P : switch to Paused");
}

/// Placeholder input handling until the real UI (Phase 6) exists. Not
/// part of the time module itself — this is glue for manual testing
/// only, so it lives in main.rs rather than time/mod.rs.
fn dev_keyboard_controls(
    keys: Res<ButtonInput<KeyCode>>,
    mut control: ResMut<TimeControl>,
    mut step_requests: EventWriter<StepDayRequested>,
) {
    if keys.just_pressed(KeyCode::Space) {
        step_requests.write(StepDayRequested);
    }
    if keys.just_pressed(KeyCode::Digit1) {
        control.mode = TimeMode::Timer {
            speed: TimerSpeed::Slow,
            elapsed_ms: 0.0,
        };
        info!("Timer mode: Slow");
    }
    if keys.just_pressed(KeyCode::Digit2) {
        control.mode = TimeMode::Timer {
            speed: TimerSpeed::Normal,
            elapsed_ms: 0.0,
        };
        info!("Timer mode: Normal");
    }
    if keys.just_pressed(KeyCode::Digit3) {
        control.mode = TimeMode::Timer {
            speed: TimerSpeed::Fast,
            elapsed_ms: 0.0,
        };
        info!("Timer mode: Fast");
    }
    if keys.just_pressed(KeyCode::KeyP) {
        control.mode = TimeMode::Paused;
        info!("Paused");
    }
}

fn log_day_advanced(mut events: EventReader<DayAdvanced>, calendar: Res<Calendar>) {
    for ev in events.read() {
        info!(
            "DayAdvanced -> day {} (year_length {})",
            ev.new_day, calendar.year_length
        );
    }
}
