//! Crops (design doc §2.4, §2.6, §2.7, §3.1-§3.3; requirements §4.3,
//! §4.6a): crop assets, the daily growth tick, and the action queue that
//! plants/waters/fertilizes/harvests/tills/clears tiles under equipment
//! throughput limits. Extends `farm::degrade_soil`'s planted-tile branch,
//! which Phase 3 left as a documented gap since `PlantedCrop`/`CropDef`
//! didn't exist yet.

pub mod crop_def;
pub mod equipment;
pub mod growth;
pub mod queue;

pub use crop_def::{quality_grade, yield_amount, CropDef, CropDefLoader, GrowthStage, QualityGrade, QualityThresholds};
pub use equipment::Equipment;
pub use growth::PlantedCrop;
pub use queue::{ActionKind, ActionQueue, NextActionId, PendingAction};

use bevy::prelude::*;

pub struct CropsPlugin;

impl Plugin for CropsPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<CropDef>()
            .init_asset_loader::<CropDefLoader>()
            .init_resource::<Equipment>()
            .add_plugins(queue::CropsQueuePlugin)
            .add_plugins(growth::CropsGrowthPlugin);
    }
}
