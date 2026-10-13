//! Crop growth tick (design doc §3.2; requirements §4.3): the one
//! generic system every planted crop runs through on `DayAdvanced`,
//! regardless of which `CropDef` it references. No `if crop.id ==
//! "tomato"` branches - the whole point of the data-driven design.

use bevy::prelude::*;

use crate::climate::{Region, WeatherToday};
use crate::farm::SoilState;
use crate::soil::SoilRoster;
use crate::sim_order::DayAdvanceOrder;
use crate::time::{Calendar, DayAdvanced};

use super::crop_def::CropDef;

#[derive(Component, Debug, Clone)]
pub struct PlantedCrop {
    pub def: Handle<CropDef>,
    pub planted_day: u32,
    pub current_stage: usize,
    pub days_in_stage: u32,
    /// Sums a bit each day outside ideal conditions; drives YIELD
    /// (design doc §2.4).
    pub cumulative_stress: f32,
    /// Max single-day deviation seen; drives QUALITY (design doc §2.4).
    pub worst_single_day_stress: f32,
    /// Crop hit `lethal_stress`. Stays on the tile as a visible dead
    /// state until the player clears it (design doc §2.4) - not
    /// despawned here.
    pub dead: bool,
}

/// **Phase 8:** the stress constants and comparison logic that used to
/// live here (`FERTILITY_DEFICIT_STRESS`, `MOISTURE_OUTSIDE_IDEAL_STRESS`,
/// `TEMP_OUTSIDE_IDEAL_STRESS`, `MOISTURE_STRESS_SCALE`, `FROST_STRESS`,
/// and the inline `temp_stress`/`frost_stress`/`moisture_stress`/
/// `fertility_stress` calculation) have moved to
/// `crop_def::compute_daily_stress`, so the tile inspector can call the
/// exact same function for its "why is this crop stressed" display
/// instead of re-deriving its own copy (your DRY request). See that
/// function's doc comment for the frost-was-dead-data history this
/// module used to carry — same fix, now in one place.

/// Days past `optimal_harvest_window_days` a crop has sat at its final,
/// completed growth stage - `0` if it isn't at the final stage yet, or
/// is but is still within the window. Split out (Phase 8) so both
/// `tick_growth` (feeds it into `compute_daily_stress`) and the tile
/// inspector (displays it) call one function instead of two copies of
/// the same "is this crop overripe" check.
pub fn overripe_days(crop: &PlantedCrop, def: &CropDef) -> u32 {
    let is_at_final_stage = !def.growth_stages.is_empty()
        && crop.current_stage == def.growth_stages.len() - 1
        && crop.days_in_stage >= def.growth_stages[crop.current_stage].duration_days;
    if is_at_final_stage {
        crop.days_in_stage.saturating_sub(
            def.growth_stages[crop.current_stage].duration_days + def.optimal_harvest_window_days,
        )
    } else {
        0
    }
}

/// Whether the crop has completed its final growth stage and can be
/// harvested. **Phase 8:** factored out of `queue::resolve_action`'s
/// inline `Harvest` check so the tile inspector can show "ready to
/// harvest" without a second copy of the same condition.
pub fn is_mature(crop: &PlantedCrop, def: &CropDef) -> bool {
    !def.growth_stages.is_empty()
        && crop.current_stage == def.growth_stages.len() - 1
        && crop.days_in_stage >= def.growth_stages[crop.current_stage].duration_days
}

/// Days remaining until `is_mature` would return true, walking the
/// remaining growth stages from `crop.current_stage`/`days_in_stage`.
/// `None` only for a degenerate `CropDef` with no growth stages at all.
/// **Phase 8**, for the tile inspector's harvest-readiness display.
pub fn days_until_mature(crop: &PlantedCrop, def: &CropDef) -> Option<u32> {
    if def.growth_stages.is_empty() {
        return None;
    }
    if is_mature(crop, def) {
        return Some(0);
    }
    let mut remaining = 0u32;
    for (i, stage) in def.growth_stages.iter().enumerate() {
        if i < crop.current_stage {
            continue;
        }
        if i == crop.current_stage {
            remaining += stage.duration_days.saturating_sub(crop.days_in_stage);
        } else {
            remaining += stage.duration_days;
        }
    }
    Some(remaining)
}

pub struct CropsGrowthPlugin;

impl Plugin for CropsGrowthPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, tick_growth.in_set(DayAdvanceOrder::CropsGrowth));
    }
}

/// Reacts to `DayAdvanced`, ordered after `process_action_queue`
/// (design doc §1.1: queued waterings/fertilizing/plantings should land
/// before growth reads soil state for the day). Advances every
/// non-dead `PlantedCrop`'s stress accumulators and growth stage.
///
/// If multiple `DayAdvanced` events land in the same frame (Timer-mode
/// catch-up after a stutter - see `time` module), this processes each
/// one in order per crop, using that event's own `new_day` for the
/// temperature lookup rather than `Res<Calendar>` (which isn't
/// committed to the new day until `time::apply_day_increment` runs
/// last). It reuses the same `WeatherToday` roll for every event in the
/// batch, inheriting the same "only the final day's roll survives"
/// limitation already documented in `climate` and `farm` - not a new
/// inconsistency introduced here.
fn tick_growth(
    mut events: EventReader<DayAdvanced>,
    region: Res<Region>,
    calendar: Res<Calendar>,
    weather: Res<WeatherToday>,
    soils: Res<SoilRoster>,
    crop_defs: Res<Assets<CropDef>>,
    mut crops: Query<(&mut PlantedCrop, &SoilState)>,
) {
    let advanced_days: Vec<u32> = events.read().map(|e| e.new_day).collect();
    if advanced_days.is_empty() {
        return;
    }

    for (mut crop, soil) in &mut crops {
        if crop.dead {
            continue;
        }

        let Some(def) = crop_defs.get(&crop.def) else {
            // Asset not loaded yet (e.g. still streaming in right after
            // planting) - skip this crop for this batch rather than
            // panicking; it'll pick up growth once the handle resolves.
            continue;
        };

        let soil_def = soils.get(soil.soil as usize);
        let available = soil_def.available(soil.water_mm);
        let air = soil_def.air(soil.water_mm);

        for &day in &advanced_days {
            if crop.dead {
                break;
            }

            let year_length = calendar.year_length.max(1);
            let doy = day % year_length;
            let temp = region.base_temp_curve.temp_on(doy, year_length);

            // Phase 8: the stress math itself now lives in
            // crop_def::compute_daily_stress, shared with the tile
            // inspector's display. overripe_days is computed here (it
            // needs crop's current stage/days-in-stage) and passed in.
            let overripe = overripe_days(&crop, def);
            let breakdown = super::crop_def::compute_daily_stress(
                def,
                temp,
                weather.is_frost,
                available,
                air,
                soil.fertility,
                overripe,
            );
            let today_stress = breakdown.total;

            crop.cumulative_stress += today_stress;
            crop.worst_single_day_stress = crop.worst_single_day_stress.max(today_stress);

            if crop.worst_single_day_stress >= def.quality_thresholds.lethal_stress {
                crop.dead = true;
                break;
            }

            // Guard against indexing past the last stage (design doc
            // §3.2's own noted correction): once current_stage is the
            // final index, the crop just accumulates days_in_stage,
            // which is exactly what the overripening check above reads.
            crop.days_in_stage += 1;
            if crop.current_stage + 1 < def.growth_stages.len() {
                let stage = &def.growth_stages[crop.current_stage];
                if crop.days_in_stage >= stage.duration_days {
                    crop.current_stage += 1;
                    crop.days_in_stage = 0;
                }
            }
        }
    }
}
