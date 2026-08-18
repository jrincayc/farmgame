//! Economy (design doc §2.5; requirements §4.5).
//!
//! **Phase 4** stubbed `Wallet`/`Inventory` just far enough for the
//! action queue to deduct `seed_cost` on Plant and store harvested goods
//! on Harvest. **Phase 5** adds the other half: `Market`/`sale_value`
//! (`market.rs`) and the instant sell flow (`sell.rs`) that turns
//! `Inventory` lots into `Wallet` money.

pub mod market;
pub mod sell;

#[allow(unused_imports)]
pub use market::{sale_value, Market};
pub use sell::{SellPlugin, SellRequested};

use std::collections::HashMap;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::crops::QualityGrade;

/// §5-style placeholder starting balance - enough to plant a handful of
/// crops for manual testing before any selling flow exists.
const STARTING_MONEY: u32 = 1000;

/// **Phase 7 addition:** `Serialize`/`Deserialize` so `save::SaveGame`
/// can persist this directly - a plain `u32`, nothing entity/handle-like
/// to work around.
#[derive(Resource, Debug, Clone, Serialize, Deserialize)]
pub struct Wallet {
    pub money: u32,
}

impl Default for Wallet {
    fn default() -> Self {
        Self { money: STARTING_MONEY }
    }
}

/// One harvested batch, quality-graded at harvest time (design doc §3.3)
/// and never re-graded afterward. Lots are kept separate per harvest
/// rather than merged into one running total per crop, even for the
/// same crop id - Phase 5's sell flow (`sell.rs`) prices per lot before
/// summing, which is easier against distinct lots than a merged pile
/// that's lost which portion was which grade.
///
/// **Phase 7 addition:** `Serialize`/`Deserialize` - embedded in
/// `Inventory`, which `save::SaveGame` persists directly. Requires
/// `QualityGrade` to derive the same (done in `crops::crop_def`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct HarvestedLot {
    pub quantity: u32,
    pub quality: QualityGrade,
}

/// **Phase 7 addition:** `Serialize`/`Deserialize` - a plain
/// `HashMap<String, Vec<HarvestedLot>>`, no entity/handle references, so
/// this persists directly in `save::SaveGame` with no separate mirror
/// struct needed (unlike `ActionQueue`/`PlantedCrop`).
#[derive(Resource, Debug, Clone, Default, Serialize, Deserialize)]
pub struct Inventory {
    /// Keyed by `CropDef::id`, not a `Handle<CropDef>` - inventory
    /// should outlive any particular asset handle and survive
    /// save/load (Phase 7) as a plain string, matching how `SavedCrop`
    /// already does it (design doc §2.8).
    pub harvested: HashMap<String, Vec<HarvestedLot>>,
}

impl Inventory {
    pub fn add(&mut self, crop_id: String, quantity: u32, quality: QualityGrade) {
        if quantity == 0 {
            return;
        }
        self.harvested
            .entry(crop_id)
            .or_default()
            .push(HarvestedLot { quantity, quality });
    }
}

pub struct EconomyPlugin;

impl Plugin for EconomyPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Wallet>()
            .init_resource::<Inventory>()
            .add_plugins(SellPlugin);
    }
}
