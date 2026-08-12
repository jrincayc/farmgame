//! Equipment (design doc §2.6; requirements §4.6a): fixed, non-purchasable
//! daily throughput per action kind for MVP. Purchasable upgrades that
//! raise these numbers are a stretch goal (requirements §7) - this
//! `HashMap` shape is exactly so a later purchase system can just bump a
//! value in it, no structural change needed.

use std::collections::HashMap;

use bevy::prelude::*;

use super::queue::ActionKind;

#[derive(Resource, Debug, Clone)]
pub struct Equipment {
    pub daily_capacity: HashMap<ActionKind, u32>,
}

impl Default for Equipment {
    fn default() -> Self {
        // §5-style placeholders (design doc's own term): illustrative
        // starting values for modern (~2000s USA) equipment, per
        // requirements §4.6a's examples - a tractor-pulled plough
        // tilling ~80 tiles/day, a combine harvesting ~120/day. Water
        // and Fertilize get sprayer/irrigation-rig-style higher
        // throughput since a pass is less intensive per tile than
        // tilling or harvesting. Clear (dead-crop debris removal) sits
        // between Till and Harvest - real numbers are a playtesting
        // question (requirements §9), same as everything else here.
        let mut daily_capacity = HashMap::new();
        daily_capacity.insert(ActionKind::Till, 80);
        daily_capacity.insert(ActionKind::Plant, 80);
        daily_capacity.insert(ActionKind::Water, 150);
        daily_capacity.insert(ActionKind::Fertilize, 150);
        daily_capacity.insert(ActionKind::Harvest, 120);
        daily_capacity.insert(ActionKind::Clear, 100);
        Self { daily_capacity }
    }
}
