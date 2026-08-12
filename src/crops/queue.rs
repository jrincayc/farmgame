//! Action queue & resolution (design doc §2.7, §3.1, §3.1a; requirements
//! §4.6a). This is what makes `Paused` mode meaningful: the player
//! queues plant/water/fertilize/harvest/till/clear actions on any tile
//! while paused, and `DayAdvanced` resolves as much of the queue as that
//! day's equipment capacity allows, FIFO per action kind.

use std::collections::{HashMap, VecDeque};

use bevy::prelude::*;

use crate::farm::SoilState;
use crate::sim_order::DayAdvanceOrder;
use crate::time::{Calendar, DayAdvanced};

use super::crop_def::{quality_grade, yield_amount, CropDef};
use super::equipment::Equipment;
use super::growth::PlantedCrop;
use crate::economy::{Inventory, Wallet};

/// Modern farm equipment categories (requirements §4.6a) - one bucket of
/// daily throughput per kind, tracked in `Equipment::daily_capacity`.
///
/// **Additions beyond the design doc's original `Till, Plant, Water,
/// Fertilize, Harvest` list:** `Clear`, added so dead crops (design doc
/// §2.4: "stays until explicitly cleared by the player") have an action
/// to resolve through, on its own capacity bucket rather than overloading
/// `Till`'s - clearing debris and tilling a field are different
/// equipment in practice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionKind {
    Till,
    Plant,
    Water,
    Fertilize,
    Harvest,
    Clear,
}

pub struct PendingAction {
    /// Stable handle for future reorder/cancel UI (requirements §9,
    /// design doc §2.7's resolved open question) - not used for
    /// anything yet since MVP's queue UI (Phase 6) is FIFO-display-only,
    /// but the data supports it from day one.
    pub id: u64,
    pub tile: Entity,
    pub kind: ActionKind,
    /// Only relevant for `ActionKind::Plant`.
    pub crop_to_plant: Option<Handle<CropDef>>,
}

#[derive(Resource, Default)]
pub struct ActionQueue {
    pub pending: VecDeque<PendingAction>,
}

#[derive(Resource, Default)]
pub struct NextActionId(pub u64);

/// Amount `SoilState.moisture`/`fertility` are bumped by a single Water
/// or Fertilize action, clamped to [0,1] afterward. §5-style
/// placeholders - real numbers are a playtesting question, same as
/// equipment capacities.
const WATER_ACTION_AMOUNT: f32 = 0.3;
const FERTILIZE_ACTION_AMOUNT: f32 = 0.2;

pub struct CropsQueuePlugin;

impl Plugin for CropsQueuePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ActionQueue>()
            .init_resource::<NextActionId>()
            .add_systems(Update, process_action_queue.in_set(DayAdvanceOrder::CropsQueue));
    }
}

/// Reacts to `DayAdvanced`, ordered after `farm::degrade_soil` and
/// before `crops::tick_growth` (design doc §1.1's chain) so a queued
/// watering/fertilizing lands before growth reads soil state for the
/// day, and a queued planting exists before growth tries to tick it.
///
/// Processes once per `DayAdvanced` event rather than once per frame,
/// so a Timer-mode catch-up spanning several days (see `time` module)
/// gives equipment a fresh daily capacity budget for each of those days
/// rather than treating the whole catch-up as a single day's work.
fn process_action_queue(
    mut events: EventReader<DayAdvanced>,
    mut queue: ResMut<ActionQueue>,
    equipment: Res<Equipment>,
    mut tiles: Query<(&mut SoilState, Option<&PlantedCrop>)>,
    mut wallet: ResMut<Wallet>,
    mut inventory: ResMut<Inventory>,
    crop_defs: Res<Assets<CropDef>>,
    calendar: Res<Calendar>,
    mut commands: Commands,
) {
    let day_count = events.read().count();
    if day_count == 0 {
        return;
    }

    for _ in 0..day_count {
        let mut done_today: HashMap<ActionKind, u32> = HashMap::new();
        let mut still_pending = VecDeque::with_capacity(queue.pending.len());

        while let Some(action) = queue.pending.pop_front() {
            let cap = equipment.daily_capacity.get(&action.kind).copied().unwrap_or(0);
            let done = done_today.entry(action.kind).or_insert(0);

            if *done >= cap {
                // Equipment capacity exhausted for this kind today -
                // carries over to the next DayAdvanced (requirements
                // §4.6a), still in its original queue position.
                still_pending.push_back(action);
                continue;
            }

            resolve_action(
                &action,
                &mut tiles,
                &mut wallet,
                &mut inventory,
                &crop_defs,
                calendar.current_day,
                &mut commands,
            );
            *done += 1;
            // Whether resolve_action actually applied the action or
            // dropped it as invalid (funds/tile-state changed since
            // queued - design doc §3.1a), it's consumed from the queue
            // and counted against today's capacity either way. This
            // matches §3.1a's own pseudocode: the `retain` closure
            // removes an item once capacity allows processing it,
            // regardless of whether the underlying action turned out to
            // still be valid.
        }

        queue.pending = still_pending;
    }
}

/// Applies a single action's real-world effect, re-validating
/// preconditions at resolution time rather than trusting whatever was
/// true when the action was queued (design doc §3.1: "re-validate at
/// resolution time - funds, tile state may have changed since queued").
/// Invalid actions are logged and dropped, not requeued - see the
/// capacity-exhaustion path above for the one case that *does* requeue.
fn resolve_action(
    action: &PendingAction,
    tiles: &mut Query<(&mut SoilState, Option<&PlantedCrop>)>,
    wallet: &mut Wallet,
    inventory: &mut Inventory,
    crop_defs: &Assets<CropDef>,
    current_day: u32,
    commands: &mut Commands,
) {
    let Ok((mut soil, planted)) = tiles.get_mut(action.tile) else {
        warn!("{:?} action targets a missing tile entity, dropping", action.kind);
        return;
    };

    match action.kind {
        ActionKind::Till => {
            soil.tilled = true;
        }

        ActionKind::Water => {
            soil.moisture = (soil.moisture + WATER_ACTION_AMOUNT).clamp(0.0, 1.0);
        }

        ActionKind::Fertilize => {
            soil.fertility = (soil.fertility + FERTILIZE_ACTION_AMOUNT).clamp(0.0, 1.0);
        }

        ActionKind::Plant => {
            let Some(handle) = &action.crop_to_plant else {
                warn!("Plant action queued with no crop specified, dropping");
                return;
            };
            if planted.is_some() {
                warn!("tile already has a crop, dropping queued Plant action");
                return;
            }
            // Precondition added in Phase 4: a tile must be freshly
            // tilled before it can be planted, and planting consumes
            // that tilled state - a field needs re-tilling between
            // crops, not just once ever. This gives Till a real,
            // recurring throughput role rather than being a one-time
            // formality.
            if !soil.tilled {
                warn!("tile not tilled, dropping queued Plant action");
                return;
            }
            let Some(def) = crop_defs.get(handle) else {
                warn!("crop def asset not loaded yet, dropping queued Plant action");
                return;
            };
            if wallet.money < def.seed_cost {
                warn!("insufficient funds for seed, dropping queued Plant action");
                return;
            }

            wallet.money -= def.seed_cost;
            soil.tilled = false;
            commands.entity(action.tile).insert(PlantedCrop {
                def: handle.clone(),
                planted_day: current_day,
                current_stage: 0,
                days_in_stage: 0,
                cumulative_stress: 0.0,
                worst_single_day_stress: 0.0,
                dead: false,
            });
        }

        ActionKind::Harvest => {
            let Some(crop) = planted else {
                warn!("no crop to harvest, dropping queued Harvest action");
                return;
            };
            if crop.dead {
                warn!("crop is dead (use Clear, not Harvest), dropping queued Harvest action");
                return;
            }
            let Some(def) = crop_defs.get(&crop.def) else {
                warn!("crop def asset not loaded yet, dropping queued Harvest action");
                return;
            };
            let is_mature = !def.growth_stages.is_empty()
                && crop.current_stage == def.growth_stages.len() - 1
                && crop.days_in_stage >= def.growth_stages[crop.current_stage].duration_days;
            if !is_mature {
                warn!("crop not yet mature, dropping queued Harvest action");
                return;
            }

            let quality = quality_grade(crop.worst_single_day_stress, &def.quality_thresholds);
            let amount = yield_amount(crop.cumulative_stress, soil.fertility, soil.moisture, def.base_yield);
            inventory.add(def.id.clone(), amount, quality);
            commands.entity(action.tile).remove::<PlantedCrop>();
        }

        ActionKind::Clear => {
            let Some(crop) = planted else {
                warn!("no crop to clear, dropping queued Clear action");
                return;
            };
            if !crop.dead {
                warn!("crop is not dead (use Harvest, not Clear), dropping queued Clear action");
                return;
            }
            commands.entity(action.tile).remove::<PlantedCrop>();
        }
    }
}
