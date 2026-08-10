//! Farm sim entry point.
//!
//! Phase 3 scope: time (Phase 1) + climate/weather (Phase 2) + farm
//! tiles and soil (Phase 3, design doc §2.3, §2.3a). Still no rendering
//! of the farm itself — this exists to prove the `DayAdvanced` chain
//! now reaches farm in the right order (after climate, so soil moisture
//! reacts to the day's already-rolled rainfall) before crops build on
//! top of it in Phase 4.
//!
//! Run with `cargo run`. With no UI yet, `TimeControl` defaults to
//! Paused, so nothing will happen on its own — press keys per the log
//! line printed at startup to drive it manually for now.

use bevy::prelude::*;

mod climate;
mod farm;
mod sim_order;
mod time;

use climate::{ClimatePlugin, Region, WeatherToday};
use farm::{FarmPlugin, SoilState};
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
    .add_plugins(FarmPlugin)
    .add_systems(Startup, print_dev_controls)
    .add_systems(Update, dev_keyboard_controls)
    // Runs after climate's DayAdvanced listener so the weather/temp it
    // prints reflects the day just rolled, not the previous day's.
    .add_systems(
        Update,
        log_day_advanced.after(sim_order::DayAdvanceOrder::Climate),
    )
    // Runs after farm's DayAdvanced listener for the same reason -
    // otherwise the printed soil averages would lag a day behind.
    .add_systems(
        Update,
        log_soil_summary.after(sim_order::DayAdvanceOrder::Farm),
    );

    sim_order::configure(&mut app);

    app.run();
}

fn print_dev_controls() {
    info!("=== Farm Sim dev build (Phase 3: farm tiles + soil) ===");
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

/// Dev-only console summary so Phase 3's soil simulation is observable
/// before any UI exists, matching the pattern used for climate/weather
/// in Phase 2. Prints the farm-wide average fertility/moisture rather
/// than every tile, since even the MVP default (64x64 = 4,096 tiles)
/// would flood the console otherwise.
fn log_soil_summary(mut events: EventReader<DayAdvanced>, tiles: Query<&SoilState>) {
    if events.read().next().is_none() {
        return;
    }

    let count = tiles.iter().count();
    if count == 0 {
        return;
    }

    let (fertility_sum, moisture_sum) = tiles
        .iter()
        .fold((0.0, 0.0), |(f, m), soil| (f + soil.fertility, m + soil.moisture));

    info!(
        "  soil avg -> fertility {:.3} | moisture {:.3} (across {} tiles)",
        fertility_sum / count as f32,
        moisture_sum / count as f32,
        count
    );
}
