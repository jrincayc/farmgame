//! Farm tiles & soil (design doc §2.3, §2.3a; requirements §4.2).
//!
//! Each tile is a Bevy **entity**, not a struct in an array — the core
//! ECS choice so that crop and (later) building components can attach
//! to individual tiles independently, queried normally, rather than
//! needing a separate parallel lookup structure for "what's on this
//! tile."

use std::collections::HashMap;

use bevy::prelude::*;

use crate::climate::WeatherToday;
use crate::sim_order::DayAdvanceOrder;
use crate::time::DayAdvanced;

/// Farm dimensions, read at world-gen time rather than compiled in
/// (design doc §2.3, requirements §4.2/§9), so a future player-facing
/// size picker just means writing into this resource before world-gen
/// runs instead of after — tile-spawning code itself doesn't change.
#[derive(Resource, Debug, Clone)]
pub struct FarmConfig {
    pub width: u32,
    pub height: u32,
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

#[derive(Component, Debug, Clone)]
pub struct SoilState {
    /// 0.0-1.0.
    pub fertility: f32,
    /// 0.0-1.0.
    pub moisture: f32,
    pub soil_type: SoilType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SoilType {
    Sandy,
    Loamy,
    Clay,
}

impl SoilType {
    /// Fraction of the gap to `FALLOW_FERTILITY_BASELINE` closed per day
    /// while a tile sits empty (design doc §2.3a). Placeholder like the
    /// other tunables flagged in design doc §5 — Loamy regenerates
    /// fastest, Sandy slowest, per the design doc's own example.
    pub fn fallow_regen_rate(self) -> f32 {
        match self {
            SoilType::Loamy => 0.05,
            SoilType::Clay => 0.03,
            SoilType::Sandy => 0.015,
        }
    }
}

/// Baseline fertility fallow tiles drift toward (design doc §2.3a's
/// "e.g. 0.6" example). Also the starting fertility for freshly-spawned
/// tiles, since a not-yet-worked field settling at its own equilibrium
/// is the natural starting point.
const FALLOW_FERTILITY_BASELINE: f32 = 0.6;

/// Fraction of a day's rolled rainfall that actually reaches soil
/// moisture (the rest is runoff/etc.), and how much moisture
/// evaporates on a dry day regardless of rainfall. Both placeholders
/// (design doc §5) — real numbers are a playtesting question, same as
/// the fertility regen rates above.
const RAINFALL_TO_MOISTURE: f32 = 0.8;
const DAILY_EVAPORATION: f32 = 0.03;

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
            .add_systems(Startup, spawn_tiles)
            .add_systems(Update, degrade_soil.in_set(DayAdvanceOrder::Farm));
    }
}

/// Spawns one entity per tile across `FarmConfig`'s bounds (clamped to
/// its own min/max, in case something inserts an out-of-range config
/// before this runs), each with a `Tile` (coordinate) and `SoilState`
/// component, and records it in `TileMap` for coordinate lookup.
fn spawn_tiles(mut commands: Commands, config: Res<FarmConfig>, mut tile_map: ResMut<TileMap>) {
    let width = config
        .width
        .clamp(FarmConfig::MIN_DIMENSION, FarmConfig::MAX_DIMENSION);
    let height = config
        .height
        .clamp(FarmConfig::MIN_DIMENSION, FarmConfig::MAX_DIMENSION);

    for y in 0..height {
        for x in 0..width {
            let coord = IVec2::new(x as i32, y as i32);
            let entity = commands
                .spawn((
                    Tile { coord },
                    SoilState {
                        fertility: FALLOW_FERTILITY_BASELINE,
                        moisture: 0.4,
                        soil_type: starting_soil_type(coord),
                    },
                ))
                .id();
            tile_map.tiles.insert(coord, entity);
        }
    }

    info!(
        "Spawned {} x {} = {} farm tiles",
        width,
        height,
        tile_map.tiles.len()
    );
}

/// MVP starting soil layout: deterministic but varied, so the farm
/// isn't mechanically uniform even before any player action (tilling,
/// crop rotation) has happened. A coordinate-hash split rather than a
/// real noise function — good enough until there's an actual reason
/// (e.g. a soil-quality minimap, or hand-authored regions) to want
/// something spatially smoother/clustered.
fn starting_soil_type(coord: IVec2) -> SoilType {
    match (coord.x.wrapping_add(coord.y.wrapping_mul(3))).rem_euclid(3) {
        0 => SoilType::Sandy,
        1 => SoilType::Loamy,
        _ => SoilType::Clay,
    }
}

/// Reacts to `DayAdvanced` (design doc §1.1: farm runs after climate,
/// since moisture reacts to the day's already-rolled rainfall). Drifts
/// fallow-tile fertility toward `FALLOW_FERTILITY_BASELINE` and applies
/// the day's rainfall/evaporation to moisture.
///
/// **Design doc correction:** §2.3a's prose says fallow fertility
/// "drifts back toward a baseline," but its code sample applied a flat
/// `soil.fertility + regen_rate` delta every day — which doesn't drift
/// toward anything, it just climbs until clamped at 1.0. This
/// implements the prose's actual intent: `delta = (baseline -
/// fertility) * rate`, so fertility approaches `FALLOW_FERTILITY_
/// BASELINE` and levels off there rather than eventually maxing out.
///
/// **Phase 3 scope note:** this only implements the fallow-tile half
/// of §2.3a's `degrade_soil` — the "while a tile has a `PlantedCrop`,
/// fertility drains/replenishes by `fertility_impact_per_day`" half
/// needs `PlantedCrop`/`CropDef`, which don't exist until Phase 4. This
/// system gains that match arm then; until then every tile is
/// effectively treated as fallow, which is accurate since nothing can
/// be planted yet.
///
/// If more than one `DayAdvanced` fires in the same frame (the timer's
/// stutter-catchup case — see time module), this applies once per
/// event, reusing whatever `WeatherToday` holds at the time this system
/// runs (climate's own multi-event handling has the same "only the
/// final day's roll survives" limitation, so this doesn't introduce a
/// new inconsistency, just inherits an existing, documented one).
fn degrade_soil(
    mut events: EventReader<DayAdvanced>,
    weather: Res<WeatherToday>,
    mut tiles: Query<&mut SoilState>,
) {
    let day_count = events.read().count();
    if day_count == 0 {
        return;
    }

    for mut soil in &mut tiles {
        for _ in 0..day_count {
            let regen_rate = soil.soil_type.fallow_regen_rate();
            let fertility_delta = (FALLOW_FERTILITY_BASELINE - soil.fertility) * regen_rate;
            soil.fertility = (soil.fertility + fertility_delta).clamp(0.0, 1.0);

            let moisture_delta = weather.rainfall * RAINFALL_TO_MOISTURE - DAILY_EVAPORATION;
            soil.moisture = (soil.moisture + moisture_delta).clamp(0.0, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::climate::ClimatePlugin;
    use crate::time::{StepDayRequested, TimePlugin};
    use std::collections::HashSet;

    fn test_app(config: FarmConfig) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(TimePlugin);
        app.add_plugins(ClimatePlugin);
        app.add_plugins(FarmPlugin);
        crate::sim_order::configure(&mut app);
        // Overwrites the FarmConfig that FarmPlugin::build already
        // inserted via init_resource - safe because Startup (where
        // spawn_tiles reads it) hasn't run yet; that happens on the
        // first app.update() below.
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
        });
        app.update(); // runs Startup

        assert_eq!(app.world().resource::<TileMap>().tiles.len(), 40 * 35);
    }

    #[test]
    fn out_of_range_config_is_clamped_not_rejected() {
        let mut app = test_app(FarmConfig {
            width: 1,
            height: FarmConfig::MAX_DIMENSION + 500,
        });
        app.update();

        let tile_map = app.world().resource::<TileMap>();
        assert_eq!(
            tile_map.tiles.len() as u32,
            FarmConfig::MIN_DIMENSION * FarmConfig::MAX_DIMENSION
        );
    }

    #[test]
    fn fallow_fertility_drifts_up_toward_baseline_from_below() {
        // width/height of 1 still clamps up to MIN_DIMENSION (32x32) in
        // spawn_tiles - fine here since every tile gets set uniformly
        // below and moves in the same direction regardless of its
        // SoilType's regen rate; we just read back one representative
        // tile.
        let mut app = test_app(FarmConfig {
            width: 1,
            height: 1,
        });
        app.update(); // spawn

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
        // Regression guard for the design-doc pseudocode bug described
        // above: a flat additive delta would eventually clamp fertility
        // at 1.0 no matter the starting point. The corrected
        // baseline-drift formula should instead converge near
        // FALLOW_FERTILITY_BASELINE and stay there. Starts well below
        // baseline (spawn_tiles already starts tiles *at* baseline,
        // which would make this test vacuous - both formulas leave an
        // already-at-baseline value alone).
        //
        // Note: FarmConfig{1,1} still clamps up to 32x32 (MIN_DIMENSION)
        // and mixes all three SoilTypes, each with a different regen
        // rate. 400 days comfortably covers even Sandy's slowest rate
        // (0.015/day: convergence to within 0.01 of baseline needs
        // ~265 days from a 0.55 gap), so every tile - not just
        // whichever one `all_soil(..)[0]` happens to be - should have
        // converged by the time we check.
        let mut app = test_app(FarmConfig {
            width: 1,
            height: 1,
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
    fn moisture_stays_within_bounds_over_many_days() {
        let mut app = test_app(FarmConfig {
            width: 1,
            height: 1,
        });
        app.update();

        for _ in 0..100 {
            app.world_mut().send_event(StepDayRequested);
            app.update();
        }

        let soil = &all_soil(&mut app)[0];
        assert!(
            (0.0..=1.0).contains(&soil.moisture),
            "moisture escaped [0,1]: {}",
            soil.moisture
        );
    }

    #[test]
    fn starting_soil_types_are_varied_across_a_small_farm() {
        let mut app = test_app(FarmConfig {
            width: 33,
            height: 33,
        });
        app.update();

        let types: HashSet<SoilType> = all_soil(&mut app).iter().map(|s| s.soil_type).collect();
        assert!(
            types.len() > 1,
            "expected varied starting soil types across a small farm, got {types:?}"
        );
    }
}
