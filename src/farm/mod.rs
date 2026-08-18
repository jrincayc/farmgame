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

use crate::climate::WeatherToday;
use crate::crops::{CropDef, PlantedCrop};
use crate::sim_order::DayAdvanceOrder;
use crate::time::DayAdvanced;

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

/// **Phase 7 addition:** `Serialize`/`Deserialize` so a per-tile save
/// entry (`save::SavedTile`) can embed this directly rather than
/// needing its own mirror struct — every field here is a plain value
/// (no `Entity`/`Handle`), unlike `PlantedCrop`, which does need a
/// mirror (`save::SavedCrop`) because of its `Handle<CropDef>`.
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct SoilState {
    /// 0.0-1.0.
    pub fertility: f32,
    /// 0.0-1.0.
    pub moisture: f32,
    pub soil_type: SoilType,
    /// **Added Phase 4.** Whether the tile has been freshly tilled and
    /// is ready to plant. Set by `ActionKind::Till`; consumed (cleared)
    /// by a successful `ActionKind::Plant` — a field needs re-tilling
    /// between crops, not just once ever, which gives Till a real,
    /// recurring throughput role (requirements §4.6a) rather than a
    /// one-time formality. Not part of the original Phase 3 design; the
    /// design doc's §2.3 `SoilState` struct is updated to match.
    pub tilled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
pub fn spawn_tile_grid(commands: &mut Commands, config: &FarmConfig, tile_map: &mut TileMap) {
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
fn spawn_tiles(mut commands: Commands, config: Res<FarmConfig>, mut tile_map: ResMut<TileMap>) {
    spawn_tile_grid(&mut commands, &config, &mut tile_map);

    info!(
        "Spawned {} x {} = {} farm tiles",
        config.width.clamp(FarmConfig::MIN_DIMENSION, FarmConfig::MAX_DIMENSION),
        config.height.clamp(FarmConfig::MIN_DIMENSION, FarmConfig::MAX_DIMENSION),
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
/// **Phase 4: the planted-tile fertility branch is now implemented,**
/// closing the gap Phase 3 explicitly left open (its scope note said
/// every tile was treated as fallow since `PlantedCrop`/`CropDef`
/// didn't exist yet). While a tile has a `PlantedCrop`, fertility now
/// changes by that crop's `fertility_impact_per_day` instead of
/// drifting toward the fallow baseline.
///
/// **Sign-convention bug caught while wiring this up:** the design
/// doc's own field comment on `fertility_impact_per_day` says "positive
/// drains soil, negative (legumes etc.) replenishes it" — but §2.3a's
/// `degrade_soil` pseudocode *added* `fertility_impact_per_day`
/// directly to `soil.fertility` with no negation. Adding a positive
/// number increases fertility, which is the opposite of "draining."
/// Following that pseudocode literally would have made every heavy
/// feeder (tomato, wheat, carrot — all positive values) replenish the
/// soil, and the one legume in the starter roster (negative value,
/// meant to demonstrate rotation via nitrogen-fixing) would have
/// drained it — backwards on both counts, and it would have quietly
/// undermined the entire "crop rotation is emergent from these values"
/// pitch (requirements §4.2). Fixed here by negating: `delta =
/// -fertility_impact_per_day`. The design doc's pseudocode is corrected
/// to match, with a note in place, same pattern as the earlier
/// flat-delta fallow-drift bug.
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
    crop_defs: Res<Assets<CropDef>>,
    mut tiles: Query<(&mut SoilState, Option<&PlantedCrop>)>,
) {
    let day_count = events.read().count();
    if day_count == 0 {
        return;
    }

    for (mut soil, planted) in &mut tiles {
        for _ in 0..day_count {
            let fertility_delta = match planted {
                Some(crop) => crop_defs
                    .get(&crop.def)
                    .map(|def| -def.fertility_impact_per_day)
                    .unwrap_or(0.0),
                None => {
                    let regen_rate = soil.soil_type.fallow_regen_rate();
                    (FALLOW_FERTILITY_BASELINE - soil.fertility) * regen_rate
                }
            };
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
        });
        app.update();

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
    fn spawned_tiles_start_untilled() {
        let mut app = test_app(FarmConfig {
            width: 1,
            height: 1,
        });
        app.update();

        assert!(all_soil(&mut app).iter().all(|s| !s.tilled));
    }

    #[test]
    fn fallow_fertility_drifts_up_toward_baseline_from_below() {
        let mut app = test_app(FarmConfig {
            width: 1,
            height: 1,
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
