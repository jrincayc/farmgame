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
