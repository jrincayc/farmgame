//! Farm tiles & soil (design doc §2.3, §2.3a; requirements §4.2).
//!
//! Each tile is a Bevy **entity**, not a struct in an array — the core
//! ECS choice so that crop and (later) building components can attach
//! to individual tiles independently, queried normally, rather than
//! needing a separate parallel lookup structure for "what's on this
//! tile."

use std::collections::HashMap;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::climate::{Region, WeatherToday};
use crate::crops::{CropDef, PlantedCrop};
use crate::sim_order::DayAdvanceOrder;
use crate::soil::{self, SoilRoster, MAX_DAILY_RAIN_MM};
use crate::time::{Calendar, DayAdvanced};

/// Farm dimensions, read at world-gen time rather than compiled in
/// (design doc §2.3, requirements §4.2/§9), so a future player-facing
/// size picker just means writing into this resource before world-gen
/// runs instead of after — tile-spawning code itself doesn't change.
///
/// **Phase 7 addition:** `Serialize`/`Deserialize` so a save file can
/// carry the farm's actual size — loading a save must rebuild the grid
/// at *that* size, not whatever `FarmConfig::default()` happens to be
/// for the currently-running instance.
#[derive(Resource, Debug, Clone, Serialize, Deserialize)]
pub struct FarmConfig {
    pub width: u32,
    pub height: u32,
    /// Seed for the soil layout noise (design doc §6.7). Defaulted on
    /// load so saves written before it existed still parse.
    #[serde(default = "default_soil_seed")]
    pub seed: u64,
}

fn default_soil_seed() -> u64 {
    0xFA2A_5EED
}

impl FarmConfig {
    pub const MIN_DIMENSION: u32 = 32;
    pub const MAX_DIMENSION: u32 = 200; // caps grid at 40,000 tiles

    /// MVP default: comfortably in "thousands of tiles" territory
    /// (requirements §5) without yet exercising the upper cap.
    pub fn default_mvp() -> Self {
        Self {
            width: 64,
            height: 64,
            seed: default_soil_seed(),
        }
    }
}

impl Default for FarmConfig {
    fn default() -> Self {
        Self::default_mvp()
    }
}

#[derive(Component, Debug, Clone, Copy)]
pub struct Tile {
    pub coord: IVec2,
}

/// Per-tile soil state. Saves use `save::SavedTile` rather than
/// serializing this directly, since the soil is stored there by string id.
#[derive(Component, Debug, Clone)]
pub struct SoilState {
    /// 0.0-1.0.
    pub fertility: f32,
    /// Water depth in the root zone, in mm (design doc §6.2), between
    /// the soil's wilting point and saturation.
    pub water_mm: f32,
    /// Index into `SoilRoster`.
    pub soil: u8,
    /// **Added Phase 4.** Whether the tile has been freshly tilled and
    /// is ready to plant. Set by `ActionKind::Till`; consumed (cleared)
    /// by a successful `ActionKind::Plant` — a field needs re-tilling
    /// between crops, not just once ever, which gives Till a real,
    /// recurring throughput role (requirements §4.6a) rather than a
    /// one-time formality.
    pub tilled: bool,
}

/// Baseline fertility fallow tiles drift toward (design doc §2.3a's
/// "e.g. 0.6" example). Also the starting fertility for freshly-spawned
/// tiles, since a not-yet-worked field settling at its own equilibrium
/// is the natural starting point.
const FALLOW_FERTILITY_BASELINE: f32 = 0.6;

/// Maps a farm coordinate to its tile entity. A plain `HashMap` is
/// explicitly fine at "a few thousand tiles" scale (design doc §5) —
/// this isn't a "need spatial partitioning" threshold, just a lookup
/// so tile-targeted actions (Phase 4's action queue, later UI clicks)
/// don't need to scan every tile entity to find the one under the
/// cursor.
#[derive(Resource, Debug, Default)]
pub struct TileMap {
    pub tiles: HashMap<IVec2, Entity>,
}

pub struct FarmPlugin;

impl Plugin for FarmPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FarmConfig>()
            .init_resource::<TileMap>()
            .insert_resource(SoilRoster::load_from_dir(soil::SOILS_DIR))
            .add_systems(Startup, spawn_tiles)
            .add_systems(Update, degrade_soil.in_set(DayAdvanceOrder::Farm));
    }
}

/// Deterministic but varied starting soil layout — see
/// `starting_soil_type` below for the actual assignment. Shared by
/// `spawn_tile_grid` for both the Startup spawn and (Phase 7)
/// `save::load_game`'s rebuild, so a freshly-loaded farm gets sensible
/// placeholder soil for any tile the save doesn't have (shouldn't
/// happen in practice, since load rebuilds at the saved dimensions
/// exactly, but keeps the function total rather than assuming the
/// caller always overwrites every tile).
///
/// **Phase 7 factor-out:** this used to be the body of `spawn_tiles`
/// directly. Pulled into its own `pub` function so `save::load_game`
/// can rebuild the grid at a saved farm size without duplicating the
/// dimension-clamping/soil-assignment logic — the whole point being
/// that grid construction only exists in one place, called from two
/// contexts (Startup, and a save load).
pub fn spawn_tile_grid(commands: &mut Commands, config: &FarmConfig, tile_map: &mut TileMap, soils: &SoilRoster) {
    let width = config
        .width
        .clamp(FarmConfig::MIN_DIMENSION, FarmConfig::MAX_DIMENSION);
    let height = config
        .height
        .clamp(FarmConfig::MIN_DIMENSION, FarmConfig::MAX_DIMENSION);

    let soil_map = soil::generate_soil_map(config.seed, width, height, soils);

    for y in 0..height {
        for x in 0..width {
            let coord = IVec2::new(x as i32, y as i32);
            let soil_index = soil_map[(y * width + x) as usize];
            let entity = commands
                .spawn((
                    Tile { coord },
                    SoilState {
                        fertility: FALLOW_FERTILITY_BASELINE,
                        water_mm: soils.get(soil_index as usize).field_capacity_mm(),
                        soil: soil_index,
                        tilled: false,
                    },
                ))
                .id();
            tile_map.tiles.insert(coord, entity);
        }
    }
}

/// Despawns every existing tile entity — `.despawn()` on Bevy 0.16
/// despawns descendants too (the `ui::render` label child included), so
/// this doesn't need a separate `despawn_recursive` call. **Compile-risk
/// note**, same spirit as other flagged spots in this codebase: this is
/// matched against Bevy 0.16's documented default-recursive-despawn
/// behavior from memory, not confirmed via `cargo check` here — if a
/// tile's label sprite survives a load, this is the first place to
/// check.
///
/// Clears `TileMap` too, since every entity in it is about to be
/// invalid. Used by `save::load_game` (Phase 7) before rebuilding the
/// grid at the save's `FarmConfig` size, which may differ from
/// whatever's currently running.
pub fn despawn_all_tiles(commands: &mut Commands, tile_map: &mut TileMap, tiles: &Query<Entity, With<Tile>>) {
    for entity in tiles.iter() {
        commands.entity(entity).despawn();
    }
    tile_map.tiles.clear();
}

/// Spawns one entity per tile across `FarmConfig`'s bounds (clamped to
/// its own min/max, in case something inserts an out-of-range config
/// before this runs), each with a `Tile` (coordinate) and `SoilState`
/// component, and records it in `TileMap` for coordinate lookup.
fn spawn_tiles(
    mut commands: Commands,
    config: Res<FarmConfig>,
    mut tile_map: ResMut<TileMap>,
    soils: Res<SoilRoster>,
) {
    spawn_tile_grid(&mut commands, &config, &mut tile_map, &soils);

    info!(
        "Spawned {} x {} = {} farm tiles",
        config.width.clamp(FarmConfig::MIN_DIMENSION, FarmConfig::MAX_DIMENSION),
        config.height.clamp(FarmConfig::MIN_DIMENSION, FarmConfig::MAX_DIMENSION),
        tile_map.tiles.len()
    );
}

/// Reacts to `DayAdvanced` (design doc §1.1: farm runs after climate,
/// since soil water reacts to the day's already-rolled rainfall). Drifts
/// fallow-tile fertility toward `FALLOW_FERTILITY_BASELINE` and runs the
/// daily water balance (design doc §6.2): rain, drainage, evaporation,
/// transpiration, then a floor at the wilting point.
///
/// While a tile has a `PlantedCrop`, fertility changes by that crop's
/// `fertility_impact_per_day` (negated: positive drains) instead of
/// drifting toward the fallow baseline.
///
/// Each `DayAdvanced` event is processed with its own day's temperature
/// (as `tick_growth` does), so a multi-day catch-up gets each day's own
/// evapotranspiration. Rainfall is the one `WeatherToday` roll that
/// survives (see `climate`).
fn degrade_soil(
    mut events: EventReader<DayAdvanced>,
    weather: Res<WeatherToday>,
    region: Res<Region>,
    calendar: Res<Calendar>,
    soils: Res<SoilRoster>,
    crop_defs: Res<Assets<CropDef>>,
    mut tiles: Query<(&mut SoilState, Option<&PlantedCrop>)>,
) {
    let temps: Vec<f32> = events
        .read()
        .map(|e| {
            let year_length = calendar.year_length.max(1);
            region.base_temp_curve.temp_on(e.new_day % year_length, year_length)
        })
        .collect();
    if temps.is_empty() {
        return;
    }
    let rain_mm = weather.rainfall * MAX_DAILY_RAIN_MM;

    for (mut soil, planted) in &mut tiles {
        let soil_def = soils.get(soil.soil as usize);
        let crop = planted.and_then(|p| crop_defs.get(&p.def).map(|def| (p, def)));

        for &temp in &temps {
            let fertility_delta = match crop {
                Some((_, def)) => -def.fertility_impact_per_day,
                None if planted.is_some() => 0.0,
                None => (FALLOW_FERTILITY_BASELINE - soil.fertility) * soil_def.fallow_regen_rate,
            };
            soil.fertility = (soil.fertility + fertility_delta).clamp(0.0, 1.0);

            // Dead crops transpire nothing.
            let living = crop.filter(|(p, _)| !p.dead).map(|(p, def)| (def, p.current_stage));

            let mut water = soil_def.add_water(soil.water_mm, rain_mm);
            water = soil_def.drain(water);
            water -= soil::daily_et_mm(soil_def, living, temp);
            soil.water_mm = water.max(soil_def.wilting_mm());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::climate::ClimatePlugin;
    use crate::crops::CropsPlugin;
    use crate::economy::EconomyPlugin;
    use crate::time::{StepDayRequested, TimePlugin};
    use std::collections::HashSet;

    fn test_app(config: FarmConfig) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(bevy::asset::AssetPlugin::default());
        app.add_plugins(TimePlugin);
        app.add_plugins(ClimatePlugin);
        app.add_plugins(CropsPlugin);
        app.add_plugins(EconomyPlugin);
        app.add_plugins(FarmPlugin);
        crate::sim_order::configure(&mut app);
        app.insert_resource(config);
        app
    }

    fn all_soil(app: &mut App) -> Vec<SoilState> {
        let mut query = app.world_mut().query::<&SoilState>();
        query.iter(app.world()).cloned().collect()
    }

    #[test]
    fn spawns_configured_number_of_tiles() {
        let mut app = test_app(FarmConfig {
            width: 40,
            height: 35,
            ..default()
        });
        app.update();

        assert_eq!(app.world().resource::<TileMap>().tiles.len(), 40 * 35);
    }

    #[test]
    fn out_of_range_config_is_clamped_not_rejected() {
        let mut app = test_app(FarmConfig {
            width: 1,
            height: FarmConfig::MAX_DIMENSION + 500,
            ..default()
        });
        app.update();

        let tile_map = app.world().resource::<TileMap>();
        assert_eq!(
            tile_map.tiles.len() as u32,
            FarmConfig::MIN_DIMENSION * FarmConfig::MAX_DIMENSION
        );
    }

    #[test]
    fn spawned_tiles_start_untilled() {
        let mut app = test_app(FarmConfig {
            width: 1,
            height: 1,
            ..default()
        });
        app.update();

        assert!(all_soil(&mut app).iter().all(|s| !s.tilled));
    }

    #[test]
    fn fallow_fertility_drifts_up_toward_baseline_from_below() {
        let mut app = test_app(FarmConfig {
            width: 1,
            height: 1,
            ..default()
        });
        app.update();

        {
            let mut query = app.world_mut().query::<&mut SoilState>();
            for mut soil in query.iter_mut(app.world_mut()) {
                soil.fertility = 0.1;
            }
        }

        app.world_mut().send_event(StepDayRequested);
        app.update();

        let soil = &all_soil(&mut app)[0];
        assert!(
            soil.fertility > 0.1 && soil.fertility < FALLOW_FERTILITY_BASELINE,
            "expected fertility to move toward baseline from below, got {}",
            soil.fertility
        );
    }

    #[test]
    fn fallow_fertility_drifts_down_toward_baseline_from_above() {
        let mut app = test_app(FarmConfig {
            width: 1,
            height: 1,
            ..default()
        });
        app.update();

        {
            let mut query = app.world_mut().query::<&mut SoilState>();
            for mut soil in query.iter_mut(app.world_mut()) {
                soil.fertility = 0.95;
            }
        }

        app.world_mut().send_event(StepDayRequested);
        app.update();

        let soil = &all_soil(&mut app)[0];
        assert!(
            soil.fertility < 0.95 && soil.fertility > FALLOW_FERTILITY_BASELINE,
            "expected fertility to move toward baseline from above, got {}",
            soil.fertility
        );
    }

    #[test]
    fn fertility_settles_near_baseline_rather_than_maxing_out() {
        let mut app = test_app(FarmConfig {
            width: 1,
            height: 1,
            ..default()
        });
        app.update();

        {
            let mut query = app.world_mut().query::<&mut SoilState>();
            for mut soil in query.iter_mut(app.world_mut()) {
                soil.fertility = 0.05;
            }
        }

        for _ in 0..400 {
            app.world_mut().send_event(StepDayRequested);
            app.update();
        }

        let soil = &all_soil(&mut app)[0];
        assert!(
            (soil.fertility - FALLOW_FERTILITY_BASELINE).abs() < 0.01,
            "expected fertility to settle near {}, got {}",
            FALLOW_FERTILITY_BASELINE,
            soil.fertility
        );
    }

    #[test]
    fn water_stays_between_wilting_point_and_saturation() {
        let mut app = test_app(FarmConfig::default());
        app.update();

        for _ in 0..100 {
            app.world_mut().send_event(StepDayRequested);
            app.update();
            let soils = app.world().resource::<SoilRoster>().clone();
            for soil in all_soil(&mut app) {
                let def = soils.get(soil.soil as usize);
                assert!(
                    soil.water_mm >= def.wilting_mm() - 1e-3 && soil.water_mm <= def.saturation_mm() + 1e-3,
                    "water {} escaped [{}, {}] for {}",
                    soil.water_mm,
                    def.wilting_mm(),
                    def.saturation_mm(),
                    def.id
                );
            }
        }
    }

    #[test]
    fn starting_soil_types_are_varied_across_a_small_farm() {
        let mut app = test_app(FarmConfig {
            width: 33,
            height: 33,
            ..default()
        });
        app.update();

        let types: HashSet<u8> = all_soil(&mut app).iter().map(|s| s.soil).collect();
        assert!(types.len() > 1, "expected varied starting soil types, got {types:?}");
    }

    /// Rain on saturated soil adds nothing: the excess is runoff, and the
    /// day's drainage/evaporation can only take water away.
    #[test]
    fn rain_on_saturated_soil_adds_nothing() {
        let mut app = test_app(FarmConfig::default());
        app.update();
        let soils = app.world().resource::<SoilRoster>().clone();
        {
            let mut query = app.world_mut().query::<&mut SoilState>();
            for mut soil in query.iter_mut(app.world_mut()) {
                soil.water_mm = soils.get(soil.soil as usize).saturation_mm();
            }
        }

        app.world_mut().send_event(StepDayRequested);
        app.update();

        for soil in all_soil(&mut app) {
            assert!(soil.water_mm <= soils.get(soil.soil as usize).saturation_mm() + 1e-3);
        }
    }

    #[test]
    fn dead_crops_use_no_water() {
        use crate::crops::{CropDef, GrowthStage, QualityThresholds};

        let mut app = test_app(FarmConfig::default());
        app.update();

        let handle = app.world_mut().resource_mut::<Assets<CropDef>>().add(CropDef {
            id: "test".into(),
            name: "test".into(),
            growth_stages: vec![GrowthStage {
                name: "growing".into(),
                duration_days: 10,
                water_use_factor: 1.0,
            }],
            min_temp: -50.0,
            max_temp: 50.0,
            ideal_temp_range: (-50.0, 50.0),
            frost_tolerant: true,
            min_available_water: 0.0,
            ideal_available_water: 0.0,
            min_air_porosity: 0.0,
            ideal_air_porosity: 0.0,
            daily_water_use_mm: 10.0,
            fertility_requirement: 0.0,
            base_yield: 1,
            sell_price: 1,
            seed_cost: 1,
            quality_thresholds: QualityThresholds {
                premium_max_stress: 1.0,
                good_max_stress: 2.0,
                lethal_stress: 100.0,
            },
            optimal_harvest_window_days: 1,
            overripe_stress_per_day: 0.0,
            fertility_impact_per_day: 0.0,
        });

        let (alive, dead) = {
            let tile_map = app.world().resource::<TileMap>();
            (tile_map.tiles[&IVec2::new(0, 0)], tile_map.tiles[&IVec2::new(1, 0)])
        };
        for (entity, is_dead) in [(alive, false), (dead, true)] {
            let mut soil = app.world_mut().get_mut::<SoilState>(entity).unwrap();
            soil.soil = 1;
            soil.water_mm = 120.0;
            app.world_mut().entity_mut(entity).insert(PlantedCrop {
                def: handle.clone(),
                planted_day: 0,
                current_stage: 0,
                days_in_stage: 0,
                cumulative_stress: 0.0,
                worst_single_day_stress: 0.0,
                dead: is_dead,
            });
        }

        app.world_mut().send_event(StepDayRequested);
        app.update();

        let water = |app: &App, e: Entity| app.world().get::<SoilState>(e).unwrap().water_mm;
        let (a, d) = (water(&app, alive), water(&app, dead));
        assert!(d > a, "dead crop should keep more water than a living one (alive {a}, dead {d})");
    }
}
