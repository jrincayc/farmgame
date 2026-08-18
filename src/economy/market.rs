//! Market pricing (design doc §2.5; requirements §4.5) - **Phase 5.**
//!
//! MVP pricing is static-per-crop but formula-shaped:
//! `sale_value = sell_price * quality_multiplier * time_factor`, with
//! `time_factor` pinned to `1.0` for every crop. The point of keeping it
//! as a real term (rather than just `sell_price * quality_multiplier`)
//! is that the stretch-goal futures market / variable pricing
//! (requirements §7) becomes "make something else write non-1.0 values
//! into `Market::price_modifiers` over time," not a rewrite of this
//! function or its caller.

use std::collections::HashMap;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::crops::QualityGrade;

/// Per-crop time-based price multiplier, keyed by `CropDef::id`. Absent
/// entries default to `1.0` (see `Market::time_factor`) rather than
/// requiring every crop id to be pre-populated here - keeps this
/// correct even if a new crop JSON is added without a matching entry.
///
/// **Phase 7 addition:** `Serialize`/`Deserialize` so `save::SaveGame`
/// can persist it directly - always empty for MVP today (no stretch
/// pricing system writes into it yet), but included now so a save file
/// doesn't need a schema migration once that stretch goal lands.
#[derive(Resource, Debug, Clone, Default, Serialize, Deserialize)]
pub struct Market {
    pub price_modifiers: HashMap<String, f32>,
}

impl Market {
    /// `1.0` for MVP, for every crop, regardless of whether an entry
    /// exists - the stretch-goal variable-pricing system is expected to
    /// insert real entries here later; until then this is a pure
    /// pass-through.
    pub fn time_factor(&self, crop_id: &str) -> f32 {
        self.price_modifiers.get(crop_id).copied().unwrap_or(1.0)
    }
}

/// Design doc §2.5's `sale_value` formula, applied to a single unit.
/// Callers multiply by quantity themselves (see `sell.rs`) - kept a pure
/// per-unit function rather than baking quantity in, so it stays usable
/// for e.g. a future "preview price per unit" UI element without a
/// quantity=1 hack.
pub fn sale_value(base_sell_price: u32, quality: QualityGrade, time_factor: f32) -> f32 {
    base_sell_price as f32 * quality.multiplier() * time_factor
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sale_value_at_good_quality_and_default_time_factor_equals_base_price() {
        let value = sale_value(10, QualityGrade::Good, 1.0);
        assert_eq!(value, 10.0);
    }

    #[test]
    fn sale_value_scales_with_quality_multiplier() {
        assert_eq!(sale_value(10, QualityGrade::Poor, 1.0), 5.0);
        assert_eq!(sale_value(10, QualityGrade::Good, 1.0), 10.0);
        assert_eq!(sale_value(10, QualityGrade::Premium, 1.0), 15.0);
    }

    #[test]
    fn sale_value_scales_with_time_factor() {
        let value = sale_value(10, QualityGrade::Good, 1.5);
        assert_eq!(value, 15.0);
    }

    #[test]
    fn market_time_factor_defaults_to_one_for_unknown_crop() {
        let market = Market::default();
        assert_eq!(market.time_factor("tomato"), 1.0);
    }

    #[test]
    fn market_time_factor_reads_configured_modifier() {
        let mut market = Market::default();
        market.price_modifiers.insert("tomato".to_string(), 1.2);
        assert_eq!(market.time_factor("tomato"), 1.2);
        assert_eq!(market.time_factor("wheat"), 1.0);
    }
}
