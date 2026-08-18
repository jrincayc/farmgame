//! Region climate & daily weather (design doc §2.2, §3.4; requirements
//! §4.3a, §4.4).
//!
//! Crop-to-season suitability is meant to be *derived*, not declared: a
//! crop states a temperature tolerance, the region states a temperature
//! curve across the year, and viability falls out of comparing the two
//! (requirements §4.3a). This module owns that curve plus the one other
//! per-day weather roll (rainfall, frost) — nothing here special-cases
//! any particular crop or "season" label.

use std::collections::HashSet;

use bevy::prelude::*;
use rand::Rng;

use crate::sim_order::DayAdvanceOrder;
use crate::time::{Calendar, DayAdvanced};

/// A region's temperature-over-the-year model. `Sine` is the only MVP
/// variant (requirements §4.3a: "could start as a simple sine-like
/// curve with a min/max"); a `Preset { name }`-style variant for
/// "tropical"/"cold" presets is a natural later addition that wouldn't
/// change how callers use `temp_on`.
#[derive(Debug, Clone, Copy)]
pub enum TempCurve {
    Sine {
        min: f32,
        max: f32,
        /// Shifts the wave along the year axis, in days. With
        /// `phase_offset: 0.0`, day 0 lands at the curve's midpoint
        /// rising toward `max`; adjust to move the coldest/warmest day
        /// to wherever "day 0" is meant to represent.
        phase_offset: f32,
    },
}

impl TempCurve {
    /// Temperature on a given day-of-year (`0..year_length`), independent
    /// of any absolute day count. Pure function specifically so a future
    /// `TileClimateModifier` (design doc §2.3, greenhouse stretch goal)
    /// can wrap it — `effective_temp = region.temp_on(day) + modifier.offset`
    /// — without this function or `Region` needing to know modifiers exist.
    pub fn temp_on(&self, day_of_year: u32, year_length: u32) -> f32 {
        match self {
            TempCurve::Sine { min, max, phase_offset } => {
                let year_length = (year_length as f32).max(1.0);
                let fraction = (day_of_year as f32 + phase_offset) / year_length;
                let wave = (fraction * std::f32::consts::TAU).sin();
                let mid = (min + max) / 2.0;
                let amplitude = (max - min) / 2.0;
                mid + amplitude * wave
            }
        }
    }
}

/// One region/save's climate (design doc §2.2). A single global resource
/// for MVP (one climate, per requirements §6 scope) — `temp_on` itself
/// doesn't assume singleton-ness, so multi-region later means changing
/// *how* `Region` is looked up (per-farm entity vs. global resource),
/// not the climate math itself.
///
/// **Not persisted (Phase 7).** `Region` isn't part of `save::SaveGame`
/// - it's a single fixed climate for MVP (requirements §6), not
/// per-save-file data, so a save/load round-trip leaves it untouched.
/// Worth revisiting if/when multi-region ever lands.
#[derive(Resource, Debug, Clone)]
pub struct Region {
    pub base_temp_curve: TempCurve,
    /// Day-of-year values (`0..year_length`) where frost is *possible*.
    /// Membership doesn't guarantee frost on that day — see
    /// `advance_day`'s frost roll below — it marks the window frost can
    /// occur in at all, which is what `frost_tolerant` crops (design doc
    /// §2.4) care about avoiding.
    pub frost_days: HashSet<u32>,
}

impl Default for Region {
    fn default() -> Self {
        let year_length = Calendar::default().year_length;
        let frost_window = year_length / 4;

        Self {
            base_temp_curve: TempCurve::Sine {
                min: -5.0,
                max: 32.0,
                phase_offset: 0.0,
            },
            frost_days: (0..frost_window).collect(),
        }
    }
}

/// This day's rolled weather (design doc §3.4). Deliberately thin: MVP
/// weather is "roll a bounded random value, check the frost calendar" —
/// no separate weather "system" beyond this resource that other systems
/// (soil moisture, crop stress) read.
#[derive(Resource, Debug, Clone, Default)]
pub struct WeatherToday {
    /// 0.0–1.0, added to soil moisture that day (design doc §3.4). Zero
    /// on days it doesn't rain at all — see `RAIN_CHANCE_PER_DAY` below;
    /// this isn't a uniform daily roll, since real precipitation is
    /// intermittent rather than "a little every day."
    pub rainfall: f32,
    pub is_frost: bool,
}

/// Derives a day-of-year from an absolute day count, the way `Calendar`
/// itself intends (design doc §2.1: "derives them from
/// `current_day % year_length`, it isn't stored redundantly").
fn day_of_year(day: u32, year_length: u32) -> u32 {
    if year_length == 0 {
        0
    } else {
        day % year_length
    }
}

/// Chance that any given day has rain at all.
const RAIN_CHANCE_PER_DAY: f64 = 0.35;

/// Chance that a day inside `Region.frost_days` actually produces frost.
const FROST_CHANCE_ON_RISK_DAY: f64 = 0.4;

pub struct ClimatePlugin;

impl Plugin for ClimatePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Region>()
            .init_resource::<WeatherToday>()
            .add_systems(Update, advance_day.in_set(DayAdvanceOrder::Climate));
    }
}

/// Reacts to `DayAdvanced` (design doc §1.1: climate runs first in the
/// chain, since farm/crops read `WeatherToday` for the same day). Rolls
/// rainfall and checks the frost calendar for the day the event names —
/// `DayAdvanced::new_day`, not `Res<Calendar>`, since `Calendar` isn't
/// committed to the new day until `time::apply_day_increment` runs last.
fn advance_day(
    mut events: EventReader<DayAdvanced>,
    calendar: Res<Calendar>,
    region: Res<Region>,
    mut weather: ResMut<WeatherToday>,
) {
    for ev in events.read() {
        let doy = day_of_year(ev.new_day, calendar.year_length);
        let mut rng = rand::thread_rng();

        weather.rainfall = if rng.gen_bool(RAIN_CHANCE_PER_DAY) {
            rng.gen_range(0.0_f32..=1.0_f32)
        } else {
            0.0
        };
        weather.is_frost =
            region.frost_days.contains(&doy) && rng.gen_bool(FROST_CHANCE_ON_RISK_DAY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_curve_stays_within_min_max() {
        let curve = TempCurve::Sine {
            min: -5.0,
            max: 32.0,
            phase_offset: 0.0,
        };
        for day in 0..112 {
            let t = curve.temp_on(day, 112);
            assert!(
                (-5.0..=32.0).contains(&t),
                "day {day} produced out-of-range temp {t}"
            );
        }
    }

    #[test]
    fn sine_curve_is_periodic_across_year_boundary() {
        let curve = TempCurve::Sine {
            min: -5.0,
            max: 32.0,
            phase_offset: 3.7,
        };
        let year_length = 112;
        let t1 = curve.temp_on(day_of_year(0, year_length), year_length);
        let t2 = curve.temp_on(day_of_year(year_length * 2, year_length), year_length);
        assert_eq!(t1, t2);
    }

    #[test]
    fn day_of_year_wraps_correctly() {
        assert_eq!(day_of_year(0, 112), 0);
        assert_eq!(day_of_year(111, 112), 111);
        assert_eq!(day_of_year(112, 112), 0);
        assert_eq!(day_of_year(113, 112), 1);
        assert_eq!(day_of_year(112 * 3 + 5, 112), 5);
    }

    #[test]
    fn day_of_year_handles_degenerate_year_length() {
        assert_eq!(day_of_year(5, 0), 0);
    }

    #[test]
    fn rain_and_frost_chance_constants_are_valid_probabilities() {
        assert!((0.0..=1.0).contains(&RAIN_CHANCE_PER_DAY));
        assert!((0.0..=1.0).contains(&FROST_CHANCE_ON_RISK_DAY));
    }

    #[test]
    fn default_region_frost_days_are_a_quarter_of_the_year() {
        let region = Region::default();
        let year_length = Calendar::default().year_length;
        assert_eq!(region.frost_days.len() as u32, year_length / 4);
    }
}
