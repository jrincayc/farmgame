//! Farm sim entry point.
//!
//! Phase 2 scope: time/calendar skeleton (Phase 1) plus region climate
//! and daily weather (design doc §2.2, §3.4). Still no rendering of the
//! farm itself — this exists to prove the `DayAdvanced` chain now
//! reaches climate in the right order before farm/crops build on top of
//! it in later phases.
//!
//! Run with `cargo run`. With no UI yet, `TimeControl` defaults to
//! Paused, so nothing will happen on its own — press keys per the log
//! line printed at startup to drive it manually for now.

use bevy::prelude::*;

mod climate;
mod sim_order;
mod time;

use climate::{ClimatePlugin, Region, WeatherToday};
use time::{Calendar, DayAdvanced, StepDayRequested, TimeControl, TimeMode, TimePlugin, TimerSpeed};

fn main() {
    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "Farm Sim (dev)".to_string(),
            ..default()
        }),
        ..default()
    }))
    .add_plugins(TimePlugin)
    .add_plugins(ClimatePlugin)
    .add_systems(Startup, print_dev_controls)
    .add_systems(Update, dev_keyboard_controls)
    // Runs after climate's DayAdvanced listener so the weather/temp it
    // prints reflects the day just rolled, not the previous day's.
    .add_systems(
        Update,
        log_day_advanced.after(sim_order::DayAdvanceOrder::Climate),
    );

    sim_order::configure(&mut app);

    app.run();
}

fn print_dev_controls() {
    info!("=== Farm Sim dev build (Phase 2: climate + weather) ===");
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

fn log_day_advanced(
    mut events: EventReader<DayAdvanced>,
    calendar: Res<Calendar>,
    region: Res<Region>,
    weather: Res<WeatherToday>,
) {
    for ev in events.read() {
        let day_of_year = ev.new_day % calendar.year_length.max(1);
        let temp = region.base_temp_curve.temp_on(day_of_year, calendar.year_length);
        info!(
            "DayAdvanced -> day {} (year_length {}) | temp {:.1}C | rainfall {:.2} | frost {}",
            ev.new_day, calendar.year_length, temp, weather.rainfall, weather.is_frost
        );
    }
}
