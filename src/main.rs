//! Farm sim entry point.
//!
//! Phase 6 scope: time (Phase 1) + climate/weather (Phase 2) + farm
//! tiles/soil (Phase 3) + crops (Phase 4) + economy sell flow (Phase 5)
//! + a real UI (Phase 6: egui panels, placeholder tile rendering,
//! click-to-select). Dev keyboard controls are kept alongside the new
//! UI for now — useful for fast manual testing without clicking through
//! panels — but the UI is the primary way to play from this phase on.

use bevy::prelude::*;

mod climate;
mod crops;
mod economy;
mod farm;
mod field;
mod save;
mod sim_order;
mod soil;
mod time;
mod ui;

use climate::{ClimatePlugin, Region, WeatherToday};
use crops::{ActionKind, ActionQueue, CropDef, CropsPlugin, NextActionId, PendingAction, PlantedCrop, TileOrField};
use economy::{EconomyPlugin, Inventory, SellRequested, Wallet};
use farm::{FarmPlugin, SoilState, TileMap};
use time::{Calendar, DayAdvanced, StepDayRequested, TimeControl, TimeMode, TimePlugin, TimerSpeed};
use ui::UiPlugin;

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
    .add_plugins(field::FieldPlugin)
    .add_plugins(CropsPlugin)
    .add_plugins(EconomyPlugin)
    .add_plugins(save::SavePlugin)
    .add_plugins(UiPlugin)
    .add_systems(Startup, print_dev_controls)
    .add_systems(Update, dev_keyboard_controls)
    .add_systems(Update, dev_crop_controls)
    .add_systems(Update, dev_economy_controls)
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
    )
    // Runs after crops::tick_growth so wallet/inventory/crop counts
    // reflect the day's queue resolution and growth tick, not what was
    // true before either ran.
    .add_systems(
        Update,
        log_crops_summary.after(sim_order::DayAdvanceOrder::CropsGrowth),
    );

    sim_order::configure(&mut app);

    app.run();
}

fn print_dev_controls() {
    info!("=== Farm Sim dev build (Phase 6: UI) ===");
    info!("A real UI now exists: top bar (time/theme), left panel");
    info!("(actions/queue), right panel (tile inspector), bottom bar");
    info!("(wallet/inventory/sell). Click a tile in the grid to select it.");
    info!("Dev keyboard controls still work too, all targeting tile (0,0):");
    info!("  Space : request a single day Step");
    info!("  1 / 2 / 3 : switch to Timer mode at Slow / Normal / Fast");
    info!("  P : switch to Paused");
    info!("  T/G/W/F/H/C : queue Till/Plant(tomato)/Water/Fertilize/Harvest/Clear");
    info!("  M : sell ALL harvested inventory, for every crop id currently held");
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

/// Temporary dev-only glue (same spirit as `dev_keyboard_controls`) so
/// Phase 4's action queue can be exercised end-to-end via `cargo run`
/// before Phase 6's real UI exists. Every key targets tile (0,0) only —
/// enough to prove the queue/equipment/growth/harvest loop works, not a
/// stand-in for real tile selection.
///
/// **Known Dvorak caveat (per existing project notes on `KeyCode`):**
/// these bindings are physical-key-based like the existing P/Space/1-3
/// controls, so they may not land on the QWERTY letters printed above
/// under a Dvorak layout — same accepted, temporary tradeoff already
/// noted for the Phase 1 controls.
fn dev_crop_controls(
    keys: Res<ButtonInput<KeyCode>>,
    tile_map: Res<TileMap>,
    asset_server: Res<AssetServer>,
    mut queue: ResMut<ActionQueue>,
    mut next_id: ResMut<NextActionId>,
) {
    let Some(&tile) = tile_map.tiles.get(&IVec2::new(0, 0)) else {
        return;
    };

    if keys.just_pressed(KeyCode::KeyT) {
        enqueue_action(&mut queue, &mut next_id, tile, ActionKind::Till, None);
        info!("Queued Till on tile (0,0)");
    }
    if keys.just_pressed(KeyCode::KeyG) {
        let handle: Handle<CropDef> = asset_server.load("crops/tomato.json");
        enqueue_action(&mut queue, &mut next_id, tile, ActionKind::Plant, Some(handle));
        info!("Queued Plant (tomato) on tile (0,0)");
    }
    if keys.just_pressed(KeyCode::KeyW) {
        enqueue_action(&mut queue, &mut next_id, tile, ActionKind::Water, None);
        info!("Queued Water on tile (0,0)");
    }
    if keys.just_pressed(KeyCode::KeyF) {
        enqueue_action(&mut queue, &mut next_id, tile, ActionKind::Fertilize, None);
        info!("Queued Fertilize on tile (0,0)");
    }
    if keys.just_pressed(KeyCode::KeyH) {
        enqueue_action(&mut queue, &mut next_id, tile, ActionKind::Harvest, None);
        info!("Queued Harvest on tile (0,0)");
    }
    if keys.just_pressed(KeyCode::KeyC) {
        enqueue_action(&mut queue, &mut next_id, tile, ActionKind::Clear, None);
        info!("Queued Clear on tile (0,0)");
    }
}

fn enqueue_action(
    queue: &mut ActionQueue,
    next_id: &mut NextActionId,
    tile: Entity,
    kind: ActionKind,
    crop_to_plant: Option<Handle<CropDef>>,
) {
    let id = next_id.0;
    next_id.0 += 1;
    // Dev keyboard controls queue a standard 20 mm watering.
    queue.pending.push_back(PendingAction::new(id, TileOrField::Tile(tile), kind, crop_to_plant, 20.0));
}

/// **Phase 5 dev glue** (same spirit/tradeoffs as `dev_crop_controls`):
/// sells every crop id currently present in `Inventory`, all at once.
/// Not tile-targeted (selling has no tile - see `economy::sell`'s
/// module doc) and not capacity-limited - fires one `SellRequested` per
/// distinct crop id held, resolved the same frame.
fn dev_economy_controls(
    keys: Res<ButtonInput<KeyCode>>,
    inventory: Res<Inventory>,
    mut sell_requests: EventWriter<SellRequested>,
) {
    if !keys.just_pressed(KeyCode::KeyM) {
        return;
    }

    if inventory.harvested.is_empty() {
        info!("No harvested inventory to sell");
        return;
    }

    // Collect ids first - Inventory itself is only borrowed immutably
    // here, but this avoids relying on iteration order staying stable
    // while events are being written.
    let crop_ids: Vec<String> = inventory.harvested.keys().cloned().collect();
    for crop_id in crop_ids {
        info!("Requesting sell for '{}'", crop_id);
        sell_requests.write(SellRequested { crop_id });
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
/// in Phase 2. Prints the farm-wide average fertility/water depth rather
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
        .fold((0.0, 0.0), |(f, m), soil| (f + soil.fertility, m + soil.water_mm));

    info!(
        "  soil avg -> fertility {:.3} | water {:.1} mm (across {} tiles)",
        fertility_sum / count as f32,
        moisture_sum / count as f32,
        count
    );
}

/// Dev-only console summary for Phase 4+5: planted/dead crop counts,
/// wallet balance, and harvested-lot count, so the whole
/// plant→grow→harvest→inventory→sell loop is observable via `cargo run`
/// before Phase 6's UI exists.
fn log_crops_summary(
    mut events: EventReader<DayAdvanced>,
    wallet: Res<Wallet>,
    inventory: Res<Inventory>,
    crops: Query<&PlantedCrop>,
) {
    if events.read().next().is_none() {
        return;
    }

    let planted = crops.iter().filter(|c| !c.dead).count();
    let dead = crops.iter().filter(|c| c.dead).count();
    let harvested_lots: usize = inventory.harvested.values().map(|v| v.len()).sum();

    info!(
        "  crops -> planted {} | dead {} | wallet {} | harvested lots {}",
        planted, dead, wallet.money, harvested_lots
    );
}
