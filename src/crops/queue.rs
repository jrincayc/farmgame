//! Action queue & resolution (design doc §2.7, §3.1, §3.1a; requirements
//! §4.6a). This is what makes `Paused` mode meaningful: the player
//! queues plant/water/fertilize/harvest/till/clear actions on any tile
//! while paused, and `DayAdvanced` resolves as much of the queue as that
//! day's equipment capacity allows, FIFO per action kind.

use std::collections::{HashMap, HashSet, VecDeque};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::farm::{FarmConfig, SoilState, Tile, TileMap};
use crate::field::{tractor_order, FieldId, FieldIndex, FieldRegistry};
use crate::messages::{ClearMessageKey, GameMessage, MessageKind, Severity};
use crate::sim_order::DayAdvanceOrder;
use crate::soil::SoilRoster;
use crate::time::{Calendar, DayAdvanced};

use super::crop_def::{quality_grade, yield_amount, CropDef};
use super::equipment::Equipment;
use super::growth::{is_mature, PlantedCrop};
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
///
/// **Phase 7 addition:** `Serialize`/`Deserialize` - both
/// `save::SavedAction` (mirrors `PendingAction`) and
/// `save::SavedEquipment` (mirrors `Equipment::daily_capacity`, a
/// `HashMap<ActionKind, u32>`) need this enum to round-trip through
/// JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ActionKind {
    Till,
    Plant,
    Water,
    Fertilize,
    Harvest,
    Clear,
}

/// What a queued action works on. Resolved when the action is processed,
/// not when it is queued (design doc §6.6), so tiles added to a field in
/// the meantime are included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileOrField {
    Tile(Entity),
    Field(FieldId),
}

pub struct PendingAction {
    /// Stable handle for future reorder/cancel UI (requirements §9,
    /// design doc §2.7's resolved open question). **Phase 7:** also what
    /// lets a loaded save's queue preserve its original ordering/ids
    /// rather than renumbering on load.
    pub id: u64,
    pub target: TileOrField,
    pub kind: ActionKind,
    /// Only relevant for `ActionKind::Plant`.
    pub crop_to_plant: Option<Handle<CropDef>>,
    /// Depth of water applied by an `ActionKind::Water` action, in mm
    /// (design doc §6.6); ignored for every other kind.
    pub water_depth_mm: f32,
    /// Tiles this action has already visited (applied or skipped). Every
    /// kind completes when every tile currently in the target is visited,
    /// which also works for Water/Fertilize, whose effect leaves nothing
    /// in tile state saying "already done".
    pub visited: HashSet<IVec2>,
    pub applied: u32,
    pub skipped: u32,
    /// Whether a "waiting for funds" message is currently standing for
    /// this action. Not saved; it is re-raised on the next blocked day.
    pub waiting_funds: bool,
}

impl PendingAction {
    pub fn new(
        id: u64,
        target: TileOrField,
        kind: ActionKind,
        crop_to_plant: Option<Handle<CropDef>>,
        water_depth_mm: f32,
    ) -> Self {
        Self {
            id,
            target,
            kind,
            crop_to_plant,
            water_depth_mm,
            visited: HashSet::new(),
            applied: 0,
            skipped: 0,
            waiting_funds: false,
        }
    }
}

#[derive(Resource, Default)]
pub struct ActionQueue {
    pub pending: VecDeque<PendingAction>,
}

#[derive(Resource, Default)]
pub struct NextActionId(pub u64);

/// Amount `SoilState.fertility` is bumped by a single Fertilize action,
/// clamped to [0,1] afterward. §5-style placeholder - a playtesting
/// question, same as equipment capacities.
const FERTILIZE_ACTION_AMOUNT: f32 = 0.2;

pub struct CropsQueuePlugin;

impl Plugin for CropsQueuePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ActionQueue>()
            .add_event::<GameMessage>()
            .add_event::<ClearMessageKey>()
            .init_resource::<FieldRegistry>()
            .init_resource::<FieldIndex>()
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
    soils: Res<SoilRoster>,
    mut tiles: Query<(&Tile, &mut SoilState, Option<&PlantedCrop>)>,
    mut wallet: ResMut<Wallet>,
    mut inventory: ResMut<Inventory>,
    crop_defs: Res<Assets<CropDef>>,
    calendar: Res<Calendar>,
    mut commands: Commands,
    (registry, index, tile_map, config): (Res<FieldRegistry>, Res<FieldIndex>, Res<TileMap>, Res<FarmConfig>),
    mut messages: EventWriter<GameMessage>,
    mut clears: EventWriter<ClearMessageKey>,
) {
    let day_count = events.read().count();
    if day_count == 0 {
        return;
    }

    for _ in 0..day_count {
        let mut done_today: HashMap<ActionKind, u32> = HashMap::new();
        let mut still_pending = VecDeque::with_capacity(queue.pending.len());

        while let Some(mut action) = queue.pending.pop_front() {
            let cap = equipment.daily_capacity.get(&action.kind).copied().unwrap_or(0);
            let done = done_today.entry(action.kind).or_insert(0);

            let funds_key = format!("funds:{}", action.id);
            let day = calendar.current_day + 1;
            let describe = |target: TileOrField| match target {
                TileOrField::Tile(_) => "a tile".to_string(),
                TileOrField::Field(id) => registry
                    .get(id)
                    .map_or_else(|| "a deleted field".to_string(), |f| f.name.clone()),
            };
            let drop_message = |reason: String| GameMessage {
                day,
                kind: MessageKind::ActionDropped,
                severity: Severity::Warning,
                text: reason,
                tile: None,
                field: None,
                key: None,
            };

            // The target's tiles right now, in the order they're worked.
            let coords: Vec<IVec2> = match action.target {
                TileOrField::Tile(entity) => match tiles.get(entity) {
                    Ok((tile, _, _)) => vec![tile.coord],
                    Err(_) => {
                        messages.write(drop_message(format!(
                            "{:?} #{} dropped: its tile no longer exists",
                            action.kind, action.id
                        )));
                        clears.write(ClearMessageKey(funds_key));
                        continue;
                    }
                },
                TileOrField::Field(id) => {
                    if registry.get(id).is_none() {
                        messages.write(drop_message(format!(
                            "{:?} #{} dropped: its field was deleted ({} tiles done)",
                            action.kind, action.id, action.applied
                        )));
                        clears.write(ClearMessageKey(funds_key));
                        continue;
                    }
                    tractor_order(index.tiles_of(id), &config)
                }
            };

            let mut out_of_funds = false;
            for &coord in &coords {
                if action.visited.contains(&coord) {
                    continue;
                }
                if *done >= cap {
                    break;
                }
                // A Plant waits (rather than skipping) on an untilled tile
                // that an earlier, still-pending Till will reach.
                if action.kind == ActionKind::Plant {
                    if let Some(&entity) = tile_map.tiles.get(&coord) {
                        let untilled = tiles.get(entity).is_ok_and(|(_, soil, _)| !soil.tilled);
                        if untilled && till_pending_for(&still_pending, entity, coord, &index) {
                            continue;
                        }
                    }
                }
                let outcome = match tile_map.tiles.get(&coord) {
                    Some(&entity) => resolve_action(
                        &action,
                        entity,
                        &mut tiles,
                        &soils,
                        &mut wallet,
                        &mut inventory,
                        &crop_defs,
                        calendar.current_day,
                        &mut commands,
                    ),
                    None => Outcome::Skipped,
                };
                match outcome {
                    Outcome::Applied => {
                        *done += 1;
                        action.applied += 1;
                    }
                    // Skipped tiles are reported, not an error, and use no
                    // equipment capacity.
                    Outcome::Skipped => action.skipped += 1,
                    // Every remaining tile costs the same, so stop for today.
                    Outcome::OutOfFunds => {
                        out_of_funds = true;
                        break;
                    }
                }
                action.visited.insert(coord);
            }

            let finished = coords.iter().all(|c| action.visited.contains(c));

            // Standing "waiting for funds" message while blocked; cleared
            // once the action can proceed or ends.
            if out_of_funds {
                let cost = action
                    .crop_to_plant
                    .as_ref()
                    .and_then(|h| crop_defs.get(h))
                    .map_or(0, |d| d.seed_cost);
                let field = match action.target {
                    TileOrField::Field(id) => Some(id),
                    _ => None,
                };
                messages.write(GameMessage {
                    day,
                    kind: MessageKind::WaitingForFunds,
                    severity: Severity::Warning,
                    text: format!(
                        "Planting on {} is waiting for funds: seeds cost {}, you have {}",
                        describe(action.target),
                        cost,
                        wallet.money
                    ),
                    tile: coords.iter().copied().find(|c| !action.visited.contains(c)),
                    field,
                    key: Some(funds_key),
                });
                action.waiting_funds = true;
            } else if action.waiting_funds {
                clears.write(ClearMessageKey(funds_key));
                action.waiting_funds = false;
            }

            if finished {
                if let TileOrField::Field(id) = action.target {
                    messages.write(GameMessage {
                        day,
                        kind: MessageKind::ActionDone,
                        severity: if action.skipped > 0 { Severity::Warning } else { Severity::Info },
                        text: format!(
                            "{:?} on {} finished: {} applied, {} skipped",
                            action.kind,
                            describe(action.target),
                            action.applied,
                            action.skipped
                        ),
                        tile: coords.first().copied(),
                        field: Some(id),
                        key: None,
                    });
                }
            } else {
                still_pending.push_back(action);
            }
        }

        queue.pending = still_pending;
    }
}

/// Whether an earlier pending Till (already moved to `earlier`, so it is
/// ahead of the action being processed in the queue) will still till this
/// tile.
fn till_pending_for(earlier: &VecDeque<PendingAction>, tile: Entity, coord: IVec2, index: &FieldIndex) -> bool {
    earlier.iter().any(|a| {
        a.kind == ActionKind::Till
            && !a.visited.contains(&coord)
            && match a.target {
                TileOrField::Tile(e) => e == tile,
                TileOrField::Field(id) => index.tiles.get(&id).is_some_and(|s| s.contains(&(coord.x, coord.y))),
            }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Applied,
    /// The action doesn't apply to this tile right now.
    Skipped,
    /// Can't afford it yet: the tile stays unvisited and the action keeps
    /// waiting (for money from a harvest, say) instead of giving up on
    /// the remaining tiles.
    OutOfFunds,
}

/// Applies a single action's real-world effect to one tile,
/// re-validating preconditions at resolution time rather than trusting
/// whatever was true when the action was queued (design doc §3.1).
fn resolve_action(
    action: &PendingAction,
    tile: Entity,
    tiles: &mut Query<(&Tile, &mut SoilState, Option<&PlantedCrop>)>,
    soils: &SoilRoster,
    wallet: &mut Wallet,
    inventory: &mut Inventory,
    crop_defs: &Assets<CropDef>,
    current_day: u32,
    commands: &mut Commands,
) -> Outcome {
    let Ok((_, mut soil, planted)) = tiles.get_mut(tile) else {
        return Outcome::Skipped;
    };

    match action.kind {
        ActionKind::Till => {
            soil.tilled = true;
        }

        ActionKind::Water => {
            // Applied like rain: capped at saturation, the excess is
            // lost. An over-application shows up as waterlogging the
            // same day and drains on the next one.
            soil.water_mm = soils.get(soil.soil as usize).add_water(soil.water_mm, action.water_depth_mm);
        }

        ActionKind::Fertilize => {
            soil.fertility = (soil.fertility + FERTILIZE_ACTION_AMOUNT).clamp(0.0, 1.0);
        }

        ActionKind::Plant => {
            let Some(handle) = &action.crop_to_plant else {
                return Outcome::Skipped;
            };
            if planted.is_some() || !soil.tilled {
                return Outcome::Skipped;
            }
            let Some(def) = crop_defs.get(handle) else {
                return Outcome::Skipped;
            };
            if wallet.money < def.seed_cost {
                return Outcome::OutOfFunds;
            }

            wallet.money -= def.seed_cost;
            soil.tilled = false;
            commands.entity(tile).insert(PlantedCrop {
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
                return Outcome::Skipped;
            };
            if crop.dead {
                return Outcome::Skipped;
            }
            let Some(def) = crop_defs.get(&crop.def) else {
                return Outcome::Skipped;
            };
            if !is_mature(crop, def) {
                return Outcome::Skipped;
            }

            let quality = quality_grade(crop.worst_single_day_stress, &def.quality_thresholds);
            let amount = yield_amount(
                crop.cumulative_stress,
                soil.fertility,
                soils.get(soil.soil as usize).available(soil.water_mm),
                def.base_yield,
            );
            inventory.add(def.id.clone(), amount, quality);
            commands.entity(tile).remove::<PlantedCrop>();
        }

        ActionKind::Clear => {
            let Some(crop) = planted else {
                return Outcome::Skipped;
            };
            if !crop.dead {
                return Outcome::Skipped;
            }
            commands.entity(tile).remove::<PlantedCrop>();
        }
    }
    Outcome::Applied
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::climate::ClimatePlugin;
    use crate::crops::CropsPlugin;
    use crate::economy::EconomyPlugin;
    use crate::farm::FarmPlugin;
    use crate::field::{DeleteFieldRequested, FieldPlugin, PaintFieldRequested, TileField};
    use crate::time::{StepDayRequested, TimePlugin};

    fn test_app() -> (App, FieldId) {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(bevy::asset::AssetPlugin::default());
        app.add_plugins(TimePlugin);
        app.add_plugins(ClimatePlugin);
        app.add_plugins(CropsPlugin);
        app.add_plugins(EconomyPlugin);
        app.add_plugins(FarmPlugin);
        app.add_plugins(FieldPlugin);
        app.add_plugins(crate::messages::MessagesPlugin);
        crate::sim_order::configure(&mut app);
        app.insert_resource(FarmConfig { width: 32, height: 32, ..default() });
        app.update();
        let id = app.world_mut().resource_mut::<FieldRegistry>().create("A".into());
        (app, id)
    }

    fn add_test_crop(app: &mut App, seed_cost: u32) -> Handle<CropDef> {
        use crate::crops::{GrowthStage, QualityThresholds};
        app.world_mut().resource_mut::<Assets<CropDef>>().add(CropDef {
            id: "t".into(),
            name: "t".into(),
            growth_stages: vec![GrowthStage { name: "g".into(), duration_days: 5, water_use_factor: 1.0 }],
            min_temp: -50.0,
            max_temp: 50.0,
            ideal_temp_range: (-50.0, 50.0),
            frost_tolerant: true,
            min_available_water: 0.0,
            ideal_available_water: 0.0,
            min_air_porosity: 0.0,
            ideal_air_porosity: 0.0,
            daily_water_use_mm: 1.0,
            fertility_requirement: 0.0,
            base_yield: 1,
            sell_price: 1,
            seed_cost,
            quality_thresholds: QualityThresholds {
                premium_max_stress: 1.0,
                good_max_stress: 2.0,
                lethal_stress: 1000.0,
            },
            optimal_harvest_window_days: 1,
            overripe_stress_per_day: 0.0,
            fertility_impact_per_day: 0.0,
        })
    }

    fn paint(app: &mut App, a: (i32, i32), b: (i32, i32), field: Option<FieldId>) {
        app.world_mut().send_event(PaintFieldRequested {
            a: IVec2::new(a.0, a.1),
            b: IVec2::new(b.0, b.1),
            field,
        });
        app.update();
    }

    fn queue_action(app: &mut App, target: TileOrField, kind: ActionKind, depth: f32) {
        app.world_mut()
            .resource_mut::<ActionQueue>()
            .pending
            .push_back(PendingAction::new(0, target, kind, None, depth));
    }

    fn step_day(app: &mut App) {
        app.world_mut().send_event(StepDayRequested);
        app.update();
    }

    fn soil_at(app: &App, x: i32, y: i32) -> SoilState {
        let e = app.world().resource::<TileMap>().tiles[&IVec2::new(x, y)];
        app.world().get::<SoilState>(e).unwrap().clone()
    }

    #[test]
    fn painting_reassigns_tiles_and_deleting_unassigns_them() {
        let (mut app, a) = test_app();
        let b = app.world_mut().resource_mut::<FieldRegistry>().create("B".into());
        paint(&mut app, (0, 0), (3, 3), Some(a));
        assert_eq!(app.world().resource::<FieldIndex>().len_of(a), 16);

        paint(&mut app, (0, 0), (1, 1), Some(b));
        let index = app.world().resource::<FieldIndex>();
        assert_eq!((index.len_of(a), index.len_of(b)), (12, 4));

        paint(&mut app, (0, 0), (0, 0), None);
        assert_eq!(app.world().resource::<FieldIndex>().len_of(b), 3);

        app.world_mut().send_event(DeleteFieldRequested(a));
        app.update();
        let e = app.world().resource::<TileMap>().tiles[&IVec2::new(3, 3)];
        assert_eq!(app.world().get::<TileField>(e).unwrap().0, None);
        assert_eq!(app.world().resource::<FieldIndex>().len_of(a), 0);
        assert!(app.world().resource::<FieldRegistry>().get(a).is_none());
    }

    #[test]
    fn field_action_works_tiles_in_tractor_order_across_days() {
        let (mut app, a) = test_app();
        paint(&mut app, (0, 0), (4, 4), Some(a)); // 25 tiles, rows y=4 (top of field) .. y=0
        app.world_mut().resource_mut::<Equipment>().daily_capacity.insert(ActionKind::Till, 10);
        queue_action(&mut app, TileOrField::Field(a), ActionKind::Till, 0.0);

        step_day(&mut app);
        // 10 tiles: the field's two top-most rows.
        assert!(soil_at(&app, 0, 4).tilled && soil_at(&app, 4, 3).tilled);
        assert!(!soil_at(&app, 0, 2).tilled);
        {
            let queue = app.world().resource::<ActionQueue>();
            assert_eq!(queue.pending.len(), 1);
            assert_eq!(queue.pending[0].applied, 10);
        }

        step_day(&mut app);
        step_day(&mut app);
        assert!(app.world().resource::<ActionQueue>().pending.is_empty());
        assert!(soil_at(&app, 4, 0).tilled);
    }

    #[test]
    fn tiles_added_to_a_field_mid_job_are_included() {
        let (mut app, a) = test_app();
        paint(&mut app, (0, 0), (4, 1), Some(a)); // 10 tiles
        app.world_mut().resource_mut::<Equipment>().daily_capacity.insert(ActionKind::Till, 5);
        queue_action(&mut app, TileOrField::Field(a), ActionKind::Till, 0.0);
        step_day(&mut app);

        paint(&mut app, (0, 5), (1, 5), Some(a)); // 2 more, above the field
        for _ in 0..4 {
            step_day(&mut app);
        }
        assert!(app.world().resource::<ActionQueue>().pending.is_empty());
        assert!(soil_at(&app, 1, 5).tilled);
    }

    #[test]
    fn amount_adding_actions_complete_after_visiting_each_tile_once() {
        let (mut app, a) = test_app();
        paint(&mut app, (0, 0), (2, 2), Some(a));
        let before = soil_at(&app, 1, 1).water_mm;
        let sat = {
            let soils = app.world().resource::<SoilRoster>();
            soils.get(soil_at(&app, 1, 1).soil as usize).saturation_mm()
        };
        queue_action(&mut app, TileOrField::Field(a), ActionKind::Water, 1.0);
        step_day(&mut app);
        assert!(app.world().resource::<ActionQueue>().pending.is_empty());
        // Not a tile outside the field.
        let outside = soil_at(&app, 10, 10);
        assert!(outside.water_mm <= sat);
        assert!(soil_at(&app, 1, 1).water_mm != before || before >= sat);
    }

    #[test]
    fn inapplicable_tiles_are_skipped_and_use_no_capacity() {
        let (mut app, a) = test_app();
        paint(&mut app, (0, 0), (2, 2), Some(a));
        app.world_mut().resource_mut::<Equipment>().daily_capacity.insert(ActionKind::Harvest, 1);
        // Nothing is planted: every tile is skipped in one day despite capacity 1.
        queue_action(&mut app, TileOrField::Field(a), ActionKind::Harvest, 0.0);
        step_day(&mut app);
        assert!(app.world().resource::<ActionQueue>().pending.is_empty());
    }

    #[test]
    fn deleted_field_drops_the_action_with_a_notice() {
        let (mut app, a) = test_app();
        paint(&mut app, (0, 0), (2, 2), Some(a));
        queue_action(&mut app, TileOrField::Field(a), ActionKind::Till, 0.0);
        app.world_mut().send_event(DeleteFieldRequested(a));
        app.update();
        step_day(&mut app);
        assert!(app.world().resource::<ActionQueue>().pending.is_empty());
        let log = app.world().resource::<crate::messages::MessageLog>();
        assert_eq!(log.entries.len(), 1);
        assert_eq!(log.entries[0].kind, crate::messages::MessageKind::ActionDropped);
    }

    #[test]
    fn empty_field_completes_immediately() {
        let (mut app, a) = test_app();
        queue_action(&mut app, TileOrField::Field(a), ActionKind::Till, 0.0);
        step_day(&mut app);
        assert!(app.world().resource::<ActionQueue>().pending.is_empty());
    }

    #[test]
    fn plant_waits_for_an_earlier_till_that_spans_several_days() {
        let (mut app, a) = test_app();
        let handle = add_test_crop(&mut app, 1);
        paint(&mut app, (0, 0), (4, 4), Some(a)); // 25 tiles
        app.world_mut().resource_mut::<Equipment>().daily_capacity.insert(ActionKind::Till, 10);
        queue_action(&mut app, TileOrField::Field(a), ActionKind::Till, 0.0);
        app.world_mut().resource_mut::<ActionQueue>().pending.push_back(PendingAction::new(
            1,
            TileOrField::Field(a),
            ActionKind::Plant,
            Some(handle),
            0.0,
        ));

        for _ in 0..3 {
            step_day(&mut app);
        }
        assert!(app.world().resource::<ActionQueue>().pending.is_empty());
        let planted = app.world_mut().query::<&PlantedCrop>().iter(app.world()).count();
        assert_eq!(planted, 25);
    }

    #[test]
    fn plant_waits_for_funds_instead_of_skipping_tiles() {
        let (mut app, a) = test_app();
        let handle = add_test_crop(&mut app, 15);
        paint(&mut app, (0, 0), (2, 0), Some(a)); // 3 tiles
        queue_action(&mut app, TileOrField::Field(a), ActionKind::Till, 0.0);
        app.world_mut().resource_mut::<ActionQueue>().pending.push_back(PendingAction::new(
            1,
            TileOrField::Field(a),
            ActionKind::Plant,
            Some(handle),
            0.0,
        ));
        app.world_mut().resource_mut::<Wallet>().money = 20; // one seed

        step_day(&mut app);
        {
            let queue = app.world().resource::<ActionQueue>();
            assert_eq!(queue.pending.len(), 1);
            assert_eq!((queue.pending[0].applied, queue.pending[0].skipped), (1, 0));
        }

        app.world_mut().resource_mut::<Wallet>().money = 100;
        step_day(&mut app);
        assert!(app.world().resource::<ActionQueue>().pending.is_empty());
        let planted = app.world_mut().query::<&PlantedCrop>().iter(app.world()).count();
        assert_eq!(planted, 3);
    }

    #[test]
    fn waiting_for_funds_is_one_standing_message_cleared_when_resolved() {
        use crate::messages::{MessageKind, MessageLog};
        let (mut app, a) = test_app();
        let handle = add_test_crop(&mut app, 15);
        paint(&mut app, (0, 0), (2, 0), Some(a));
        queue_action(&mut app, TileOrField::Field(a), ActionKind::Till, 0.0);
        app.world_mut().resource_mut::<ActionQueue>().pending.push_back(PendingAction::new(
            1,
            TileOrField::Field(a),
            ActionKind::Plant,
            Some(handle),
            0.0,
        ));
        app.world_mut().resource_mut::<Wallet>().money = 0;
        for _ in 0..3 {
            step_day(&mut app);
        }
        let waiting = |app: &App| {
            app.world()
                .resource::<MessageLog>()
                .entries
                .iter()
                .filter(|m| m.kind == MessageKind::WaitingForFunds)
                .count()
        };
        assert_eq!(waiting(&app), 1);

        app.world_mut().resource_mut::<Wallet>().money = 100;
        step_day(&mut app);
        assert_eq!(waiting(&app), 0);
    }

    #[test]
    fn a_field_maturing_is_one_message_and_can_auto_pause() {
        use crate::messages::{GameOptions, MessageKind, MessageLog};
        use crate::time::{TimeControl, TimeMode, TimerSpeed};
        let (mut app, a) = test_app();
        let handle = add_test_crop(&mut app, 1);
        paint(&mut app, (0, 0), (2, 0), Some(a));
        for x in 0..3 {
            let e = app.world().resource::<TileMap>().tiles[&IVec2::new(x, 0)];
            app.world_mut().entity_mut(e).insert(PlantedCrop {
                def: handle.clone(),
                planted_day: 0,
                current_stage: 0,
                days_in_stage: 4, // one day short of the 5-day stage
                cumulative_stress: 0.0,
                worst_single_day_stress: 0.0,
                dead: false,
            });
        }
        app.world_mut().resource_mut::<GameOptions>().auto_pause_on_ready = true;
        app.world_mut().resource_mut::<TimeControl>().mode = TimeMode::Timer {
            speed: TimerSpeed::Normal,
            elapsed_ms: 0.0,
        };
        step_day(&mut app);

        let log = app.world().resource::<MessageLog>();
        let ready: Vec<_> = log.entries.iter().filter(|m| m.kind == MessageKind::ReadyToHarvest).collect();
        assert_eq!(ready.len(), 1);
        assert!(ready[0].text.contains("3 t ready to harvest"), "{}", ready[0].text);
        assert!(matches!(app.world().resource::<TimeControl>().mode, TimeMode::Paused));
    }
}
