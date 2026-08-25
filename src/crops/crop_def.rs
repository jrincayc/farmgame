//! Crop definitions (design doc §2.4; requirements §4.3): the data-driven
//! asset type crops are loaded from, plus the pure grading functions that
//! read it. Nothing here is per-crop special-cased — a new crop is a new
//! JSON file, not new code.

use bevy::asset::io::Reader;
use bevy::asset::{AssetLoader, LoadContext};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::fmt;

/// A crop's static definition, loaded from `assets/crops/*.json` via
/// `CropDefLoader`. See the field-by-field schema note below for the
/// **Phase 4 schema change** from a single `water_requirement` scalar to
/// a min/max/ideal range mirroring the temperature fields.
#[derive(Asset, TypePath, Debug, Clone, Deserialize)]
pub struct CropDef {
    pub id: String,
    pub name: String,
    pub growth_stages: Vec<GrowthStage>,

    pub min_temp: f32,
    pub max_temp: f32,
    pub ideal_temp_range: (f32, f32),
    pub frost_tolerant: bool,

    /// **Schema change (Phase 4):** the requirements doc's original
    /// `water_requirement: f32` schema only modeled under-watering. Per
    /// design feedback, over-watering is a real failure mode too (root
    /// rot, waterlogging) — so moisture now mirrors the temperature
    /// fields exactly: an absolute viable range (`min_moisture`/
    /// `max_moisture`) and a narrower `ideal_moisture_range` for
    /// full-quality growth. Values are on `SoilState.moisture`'s 0.0-1.0
    /// scale, not an absolute water quantity.
    pub min_moisture: f32,
    pub max_moisture: f32,
    pub ideal_moisture_range: (f32, f32),

    pub fertility_requirement: f32,

    pub base_yield: u32,
    pub sell_price: u32,
    pub seed_cost: u32,

    pub quality_thresholds: QualityThresholds,
    pub optimal_harvest_window_days: u32,
    pub overripe_stress_per_day: f32,

    /// Positive drains soil fertility per day planted (heavy feeders);
    /// negative replenishes it (legumes, cover crops). See design doc
    /// §2.3a and the **sign-convention fix** noted in `farm::degrade_soil`
    /// — the original pseudocode added this value directly to fertility,
    /// which would make a positive ("draining") value *increase*
    /// fertility, backwards from this doc comment's own stated intent.
    pub fertility_impact_per_day: f32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QualityThresholds {
    pub premium_max_stress: f32,
    pub good_max_stress: f32,
    pub lethal_stress: f32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GrowthStage {
    pub name: String,
    pub duration_days: u32,
}

/// Determines produce quality from the worst single day of stress a
/// harvested crop experienced (design doc §2.4 note on yield vs.
/// quality). Multiplies `sell_price` at sale time (requirements §4.5) —
/// the multiplier itself lives on `QualityGrade` since it's intrinsic to
/// the grade, not something each caller should redefine.
///
/// **Phase 7 addition:** `Serialize`/`Deserialize` — `economy::Inventory`
/// (persisted directly in `save::SaveGame`) stores `HarvestedLot`s that
/// embed this enum, so it needs to round-trip through JSON like every
/// other saved field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QualityGrade {
    Poor,
    Good,
    Premium,
}

impl QualityGrade {
    pub fn multiplier(self) -> f32 {
        match self {
            QualityGrade::Poor => 0.5,
            QualityGrade::Good => 1.0,
            QualityGrade::Premium => 1.5,
        }
    }
}

impl fmt::Display for QualityGrade {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}",
               match self {
                   QualityGrade::Poor => "Poor",
                   QualityGrade::Good => "Good",
                   QualityGrade::Premium => "Premium",
               }
        )
    }
}

pub fn quality_grade(worst_single_day_stress: f32, thresholds: &QualityThresholds) -> QualityGrade {
    if worst_single_day_stress <= thresholds.premium_max_stress {
        QualityGrade::Premium
    } else if worst_single_day_stress <= thresholds.good_max_stress {
        QualityGrade::Good
    } else {
        QualityGrade::Poor
    }
}

/// Cumulative-stress-and-soil-quality yield model (requirements §4.3's
/// "yield tracks cumulative average conditions" design note). Two
/// independent factors, both clamped to [0,1] before multiplying:
/// - `stress_factor`: chronically stressed crops yield less, tapering to
///   zero at `YIELD_STRESS_DIVISOR` total accumulated stress.
/// - `soil_factor`: the tile's fertility/moisture *at harvest time*
///   contributes, but is floored at `MIN_SOIL_YIELD_FACTOR` rather than
///   being able to zero out yield entirely on its own — soil quality
///   should matter, but a crop that otherwise grew perfectly on
///   mediocre-but-viable soil shouldn't harvest nothing.
///
/// Both constants are §5-style placeholders (design doc's own term for
/// numbers that are a playtesting question, not an architecture one).
const YIELD_STRESS_DIVISOR: f32 = 200.0;
const MIN_SOIL_YIELD_FACTOR: f32 = 0.3;

pub fn yield_amount(cumulative_stress: f32, fertility: f32, moisture: f32, base_yield: u32) -> u32 {
    let stress_factor = (1.0 - cumulative_stress / YIELD_STRESS_DIVISOR).clamp(0.0, 1.0);
    let soil_factor = ((fertility + moisture) / 2.0).clamp(MIN_SOIL_YIELD_FACTOR, 1.0);
    ((base_yield as f32) * stress_factor * soil_factor).round() as u32
}

/// Where a value sits relative to a crop's viable/ideal range - shared
/// by `compute_daily_stress` below and, purely for display, the tile
/// inspector (`ui::panels::tile_inspector_panel`). **Phase 8:** this is
/// the single place that decides "too cold" vs "a bit cold" vs "ideal"
/// for both temperature and moisture, so the growth tick and the UI
/// can't drift apart on what counts as stress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeStatus {
    /// Below the absolute viable minimum - severe, single-day stress.
    BelowMin,
    /// Above the absolute viable maximum - severe, single-day stress.
    AboveMax,
    /// Inside the viable range but below the narrower ideal range.
    BelowIdeal,
    /// Inside the viable range but above the narrower ideal range.
    AboveIdeal,
    /// Inside the ideal range.
    Ideal,
}

pub fn range_status(value: f32, min: f32, max: f32, ideal: (f32, f32)) -> RangeStatus {
    if value < min {
        RangeStatus::BelowMin
    } else if value > max {
        RangeStatus::AboveMax
    } else if value < ideal.0 {
        RangeStatus::BelowIdeal
    } else if value > ideal.1 {
        RangeStatus::AboveIdeal
    } else {
        RangeStatus::Ideal
    }
}

/// Stress added when a value sits outside its ideal (but still viable)
/// range - temperature and moisture both use this same mild penalty
/// (design doc §3.2's original constants, now shared instead of
/// duplicated per-field).
const OUTSIDE_IDEAL_STRESS: f32 = 0.25;
/// Stress added when soil fertility is below `fertility_requirement`.
const FERTILITY_DEFICIT_STRESS: f32 = 0.25;
/// Flat stress hit on a frost day for a non-frost-tolerant crop.
const FROST_STRESS: f32 = 6.0;
/// Converts a moisture deficit/excess (0.0-1.0 scale) into stress units
/// comparable to temperature stress (degrees C, much larger swings).
const MOISTURE_STRESS_SCALE: f32 = 10.0;

/// One day's stress, broken down by source. `tick_growth` sums
/// `.total` into `cumulative_stress`/`worst_single_day_stress`; the
/// tile inspector (Phase 8 polish) displays the individual fields so
/// the player can tell *why* a crop is stressed - "too hot" vs "too
/// dry" vs "starved soil" - rather than only seeing the two running
/// totals with no way to tell which direction would help.
#[derive(Debug, Clone, Copy)]
pub struct StressBreakdown {
    pub temp_status: RangeStatus,
    pub temp_stress: f32,
    pub frost_hit: bool,
    pub frost_stress: f32,
    pub moisture_status: RangeStatus,
    pub moisture_stress: f32,
    pub fertility_ok: bool,
    pub fertility_stress: f32,
    pub overripe_days: u32,
    pub overripe_stress: f32,
    pub total: f32,
}

/// The one function that computes a day's stress - called by both
/// `growth::tick_growth` (which feeds `.total` into the accumulators)
/// and `ui::panels::tile_inspector_panel` (which only reads the
/// breakdown for display). Per the DRY requirement this was written to
/// satisfy: there is exactly one copy of "what counts as stress" in the
/// codebase now, not one in `growth.rs` and a second, display-only copy
/// in `panels.rs`.
///
/// `overripe_days` is passed in rather than computed here, since it
/// depends on `PlantedCrop`'s stage/days-in-stage state, which this
/// module (crop *definitions* only) deliberately doesn't reference -
/// see `growth::overripe_days` for that half.
pub fn compute_daily_stress(
    def: &CropDef,
    temp: f32,
    is_frost: bool,
    moisture: f32,
    fertility: f32,
    overripe_days: u32,
) -> StressBreakdown {
    let temp_status = range_status(temp, def.min_temp, def.max_temp, def.ideal_temp_range);
    let temp_stress = match temp_status {
        RangeStatus::BelowMin => def.min_temp - temp,
        RangeStatus::AboveMax => temp - def.max_temp,
        RangeStatus::BelowIdeal | RangeStatus::AboveIdeal => OUTSIDE_IDEAL_STRESS,
        RangeStatus::Ideal => 0.0,
    };

    let frost_hit = is_frost && !def.frost_tolerant;
    let frost_stress = if frost_hit { FROST_STRESS } else { 0.0 };

    let moisture_status = range_status(moisture, def.min_moisture, def.max_moisture, def.ideal_moisture_range);
    let moisture_stress = match moisture_status {
        RangeStatus::BelowMin => (def.min_moisture - moisture) * MOISTURE_STRESS_SCALE,
        RangeStatus::AboveMax => (moisture - def.max_moisture) * MOISTURE_STRESS_SCALE,
        RangeStatus::BelowIdeal | RangeStatus::AboveIdeal => OUTSIDE_IDEAL_STRESS,
        RangeStatus::Ideal => 0.0,
    };

    let fertility_ok = fertility >= def.fertility_requirement;
    let fertility_stress = if fertility_ok { 0.0 } else { FERTILITY_DEFICIT_STRESS };

    let overripe_stress = overripe_days as f32 * def.overripe_stress_per_day;

    let total = temp_stress + frost_stress + moisture_stress + fertility_stress + overripe_stress;

    StressBreakdown {
        temp_status,
        temp_stress,
        frost_hit,
        frost_stress,
        moisture_status,
        moisture_stress,
        fertility_ok,
        fertility_stress,
        overripe_days,
        overripe_stress,
        total,
    }
}

/// Error type for `CropDefLoader`. Hand-rolled rather than pulling in
/// `thiserror` as a new direct dependency for one small loader — this
/// satisfies `AssetLoader::Error`'s `Into<Box<dyn Error + Send + Sync>>`
/// bound via std's blanket `From<E: Error> for Box<dyn Error + ..>` impl.
#[derive(Debug)]
pub struct CropDefLoaderError(String);

impl std::fmt::Display for CropDefLoaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "failed to load crop def: {}", self.0)
    }
}

impl std::error::Error for CropDefLoaderError {}

impl From<std::io::Error> for CropDefLoaderError {
    fn from(e: std::io::Error) -> Self {
        CropDefLoaderError(e.to_string())
    }
}

impl From<serde_json::Error> for CropDefLoaderError {
    fn from(e: serde_json::Error) -> Self {
        CropDefLoaderError(e.to_string())
    }
}

/// Loads `CropDef` from `assets/crops/*.json` (requirements §3: JSON for
/// crop data, via a custom `AssetLoader<CropDef>` backed by
/// `serde_json`, exactly as the tech-stack section specifies).
#[derive(Default)]
pub struct CropDefLoader;

impl AssetLoader for CropDefLoader {
    type Asset = CropDef;
    type Settings = ();
    type Error = CropDefLoaderError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &Self::Settings,
        _load_context: &mut LoadContext<'_>,
    ) -> Result<Self::Asset, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let def: CropDef = serde_json::from_slice(&bytes)?;
        Ok(def)
    }

    fn extensions(&self) -> &[&str] {
        &["json"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thresholds() -> QualityThresholds {
        QualityThresholds {
            premium_max_stress: 0.5,
            good_max_stress: 2.0,
            lethal_stress: 8.0,
        }
    }

    #[test]
    fn quality_grade_boundaries() {
        let t = thresholds();
        assert_eq!(quality_grade(0.5, &t), QualityGrade::Premium);
        assert_eq!(quality_grade(0.51, &t), QualityGrade::Good);
        assert_eq!(quality_grade(2.0, &t), QualityGrade::Good);
        assert_eq!(quality_grade(2.01, &t), QualityGrade::Poor);
    }

    #[test]
    fn yield_amount_at_zero_stress_and_full_soil_equals_base() {
        let amount = yield_amount(0.0, 1.0, 1.0, 12);
        assert_eq!(amount, 12);
    }

    #[test]
    fn yield_amount_never_goes_below_soil_floor() {
        let amount = yield_amount(0.0, 0.0, 0.0, 100);
        assert!(amount > 0, "expected soil floor to keep some yield, got {amount}");
    }

    #[test]
    fn yield_amount_is_clamped_not_negative_at_extreme_stress() {
        let amount = yield_amount(1000.0, 1.0, 1.0, 50);
        assert_eq!(amount, 0);
    }
}
