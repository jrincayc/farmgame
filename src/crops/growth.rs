//! Crop growth tick (design doc §3.2; requirements §4.3): the one
//! generic system every planted crop runs through on `DayAdvanced`,
//! regardless of which `CropDef` it references. No `if crop.id ==
//! "tomato"` branches - the whole point of the data-driven design.

use bevy::prelude::*;

use crate::climate::{Region, WeatherToday};
use crate::farm::SoilState;
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

/// Stress added when soil fertility is below `fertility_requirement`.
/// Deliberately mild ("outside ideal but still viable"), matching the
/// existing outside-ideal-temperature-range stress - a nutrient-poor
/// tile handicaps a crop, it doesn't doom it outright the way frost or
/// a moisture extreme can.
const FERTILITY_DEFICIT_STRESS: f32 = 0.25;

/// Same treatment for moisture outside the ideal (but still viable)
/// range.
const MOISTURE_OUTSIDE_IDEAL_STRESS: f32 = 0.25;
const TEMP_OUTSIDE_IDEAL_STRESS: f32 = 0.25;

/// Converts a moisture deficit/excess (on `SoilState.moisture`'s 0.0-1.0
/// scale) into stress units comparable to the temperature stress scale
/// (which is in degrees C, so raw values run much larger - e.g. a
/// region routinely swings across 30+ degrees, but moisture only spans
/// 0..1). Without a scale factor, moisture stress would always be
/// negligible next to temperature stress. §5-style placeholder - tune
/// once quality_thresholds are being playtested against real crop runs.
const MOISTURE_STRESS_SCALE: f32 = 10.0;

/// **Design-doc gap I'm filling here, not just implementing:** §3.2's
/// original `tick_growth` pseudocode computed `temp_stress` from
/// `Region::temp_on` but never referenced `WeatherToday.is_frost` at
/// all, despite `frost_tolerant` existing specifically to interact with
/// frost. A frost event is a sharp, discrete overnight cold snap layered
/// on top of the smooth seasonal curve - a day can sit comfortably
/// within a crop's viable temp range on the curve and still frost that
/// night. Without this, `frost_tolerant` was dead data. Non-frost-
/// tolerant crops now take a flat stress hit on a frost day, regardless
/// of where the day's curve temperature sits. §5-style placeholder value.
const FROST_STRESS: f32 = 6.0;

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

        for &day in &advanced_days {
            if crop.dead {
                break;
            }

            let year_length = calendar.year_length.max(1);
            let doy = day % year_length;
            let temp = region.base_temp_curve.temp_on(doy, year_length);

            let temp_stress = if temp < def.min_temp || temp > def.max_temp {
                (def.min_temp - temp).max(temp - def.max_temp)
            } else if !(def.ideal_temp_range.0..=def.ideal_temp_range.1).contains(&temp) {
                TEMP_OUTSIDE_IDEAL_STRESS
            } else {
                0.0
            };

            let frost_stress = if weather.is_frost && !def.frost_tolerant {
                FROST_STRESS
            } else {
                0.0
            };

            let moisture = soil.moisture;
            let moisture_stress = if moisture < def.min_moisture || moisture > def.max_moisture {
                (def.min_moisture - moisture).max(moisture - def.max_moisture) * MOISTURE_STRESS_SCALE
            } else if !(def.ideal_moisture_range.0..=def.ideal_moisture_range.1).contains(&moisture) {
                MOISTURE_OUTSIDE_IDEAL_STRESS
            } else {
                0.0
            };

            let fertility_stress = if soil.fertility < def.fertility_requirement {
                FERTILITY_DEFICIT_STRESS
            } else {
                0.0
            };

            // Overripening: only once a crop has completed its final
            // stage and sat there past optimal_harvest_window_days.
            // Feeds the same accumulators as every other stress source -
            // "left to rot" isn't a separate mechanic (design doc §3.2).
            let is_at_final_stage = !def.growth_stages.is_empty()
                && crop.current_stage == def.growth_stages.len() - 1
                && crop.days_in_stage >= def.growth_stages[crop.current_stage].duration_days;
            let overripe_days = if is_at_final_stage {
                crop.days_in_stage.saturating_sub(
                    def.growth_stages[crop.current_stage].duration_days
                        + def.optimal_harvest_window_days,
                )
            } else {
                0
            };

            let today_stress = temp_stress
                + frost_stress
                + moisture_stress
                + fertility_stress
                + overripe_days as f32 * def.overripe_stress_per_day;

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
