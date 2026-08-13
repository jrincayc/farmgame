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

// Some of these re-exports (GrowthStage, QualityThresholds,
// quality_grade, yield_amount) aren't yet called by name outside
// crops:: itself - `cargo check` correctly flags that as unused in a
// binary crate with no external consumers. Kept anyway (rather than
// dropped) as intentional public API surface for this module - e.g.
// `ui::panels` or a future save/load module may construct/inspect
// these directly. Silenced explicitly rather than left as an unaddressed
// warning.
#[allow(unused_imports)]
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
