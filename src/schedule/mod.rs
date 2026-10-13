//! Scheduling (design doc §4.7 / §3.11; requirements §4.7): standing
//! per-field sequences of actions that run on their own.
//!
//! A `Schedule` is an ordered list of `Step`s for one field, a cursor and
//! a repeat flag. Each step is an action plus a `Wait` condition. Once per
//! `DayAdvanced`, `fire_schedules` checks every schedule whose current
//! step is waiting and, when the condition holds, pushes an ordinary
//! `PendingAction` onto the `ActionQueue` - so capacity limits, tractor
//! order, funds-waiting and messages all behave exactly as for a manual
//! action. `track_schedules` follows that action by id (via
//! `ActionEnded`) and moves the cursor on when it completes.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::crops::{
    is_mature, ActionEnd, ActionEnded, ActionKind, ActionQueue, CropDef, CropRoster, NextActionId, PendingAction,
    PlantedCrop, TileOrField,
};
use crate::farm::{SoilState, TileMap};
use crate::field::{FieldId, FieldIndex, FieldRegistry};
use crate::messages::{ClearMessageKey, GameMessage, MessageKind, Severity};
use crate::sim_order::DayAdvanceOrder;
use crate::soil::SoilRoster;
use crate::time::{Calendar, DayAdvanced};

/// When a step may start, measured from the moment the previous step
/// finished (or from the schedule's creation, for the first step).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Wait {
    Immediately,
    /// At least one living crop in the field is ready to harvest.
    AnyMature,
    /// The field has living crops and all of them are ready to harvest.
    AllMature,
    /// On this day of the year (1-based).
    DayOfYear(u32),
    /// At least this many days after the previous step finished.
    AfterDays(u32),
    /// The field's average plant-available water (0-1) is below this.
    SoilDry(f32),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub kind: ActionKind,
    /// `CropDef.id`; only used by `ActionKind::Plant`.
    #[serde(default)]
    pub crop_id: Option<String>,
    /// Only used by `ActionKind::Water`.
    #[serde(default)]
    pub water_depth_mm: f32,
    pub wait: Wait,
}

impl Step {
    pub fn new(kind: ActionKind, wait: Wait) -> Self {
        Self { kind, crop_id: None, water_depth_mm: 20.0, wait }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RunState {
    /// The current step's action has not been queued yet.
    Waiting,
    /// The current step's action is in the queue, with this id.
    Running(u64),
    /// Needs the player: the step's action was cancelled or removed, or
    /// can't be built. The text says why.
    Stalled(String),
    /// A non-repeating schedule that has run all its steps.
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub id: u32,
    pub field: FieldId,
    pub steps: Vec<Step>,
    pub repeat: bool,
    pub enabled: bool,
    pub cursor: usize,
    /// Day the current step became the current step.
    pub since_day: u32,
    pub state: RunState,
}

impl Schedule {
    pub fn new(id: u32, field: FieldId, steps: Vec<Step>, today: u32) -> Self {
        Self {
            id,
            field,
            steps,
            repeat: false,
            enabled: true,
            cursor: 0,
            since_day: today,
            state: RunState::Waiting,
        }
    }

    /// Move to the next step (wrapping if repeating). Returns `true` if
    /// the schedule has now run out of steps.
    pub fn advance(&mut self, today: u32) -> bool {
        self.since_day = today;
        self.cursor += 1;
        if self.cursor >= self.steps.len() {
            if self.repeat && !self.steps.is_empty() {
                self.cursor = 0;
                self.state = RunState::Waiting;
                false
            } else {
                self.cursor = self.steps.len();
                self.state = RunState::Done;
                true
            }
        } else {
            self.state = RunState::Waiting;
            false
        }
    }

    pub fn restart(&mut self, today: u32) {
        self.cursor = 0;
        self.since_day = today;
        self.state = RunState::Waiting;
    }

    pub fn current_step(&self) -> Option<&Step> {
        self.steps.get(self.cursor)
    }
}

#[derive(Resource, Debug, Clone, Default, Serialize, Deserialize)]
pub struct Schedules {
    pub items: Vec<Schedule>,
    pub next_id: u32,
}

impl Schedules {
    pub fn create(&mut self, field: FieldId, steps: Vec<Step>, today: u32) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.items.push(Schedule::new(id, field, steps, today));
        id
    }
}

/// What the wait conditions need to know about a field.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FieldStats {
    pub planted: usize,
    pub mature: usize,
    pub mean_available: Option<f32>,
}

/// Whether `wait` is satisfied on `today` (the day being advanced to).
pub fn wait_met(wait: Wait, today: u32, year_length: u32, since_day: u32, stats: &FieldStats) -> bool {
    match wait {
        Wait::Immediately => true,
        Wait::AnyMature => stats.mature > 0,
        Wait::AllMature => stats.planted > 0 && stats.mature == stats.planted,
        Wait::DayOfYear(d) => year_length > 0 && today % year_length + 1 == d,
        Wait::AfterDays(n) => today >= since_day + n,
        Wait::SoilDry(threshold) => stats.mean_available.is_some_and(|a| a < threshold),
    }
}

fn needs_stats(wait: Wait) -> bool {
    matches!(wait, Wait::AnyMature | Wait::AllMature | Wait::SoilDry(_))
}

fn field_stats(
    field: FieldId,
    index: &FieldIndex,
    tile_map: &TileMap,
    tiles: &Query<(&SoilState, Option<&PlantedCrop>)>,
    crop_defs: &Assets<CropDef>,
    soils: &SoilRoster,
) -> FieldStats {
    let mut stats = FieldStats::default();
    let mut water_sum = 0.0;
    let mut count = 0;
    for coord in index.tiles_of(field) {
        let Some(&entity) = tile_map.tiles.get(&coord) else { continue };
        let Ok((soil, crop)) = tiles.get(entity) else { continue };
        water_sum += soils.get(soil.soil as usize).available(soil.water_mm);
        count += 1;
        if let Some(crop) = crop.filter(|c| !c.dead) {
            stats.planted += 1;
            if crop_defs.get(&crop.def).is_some_and(|def| is_mature(crop, def)) {
                stats.mature += 1;
            }
        }
    }
    if count > 0 {
        stats.mean_available = Some(water_sum / count as f32);
    }
    stats
}

pub struct SchedulePlugin;

impl Plugin for SchedulePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Schedules>()
            .add_systems(Update, fire_schedules.in_set(DayAdvanceOrder::Schedule))
            .add_systems(Update, track_schedules.in_set(DayAdvanceOrder::ScheduleTrack));
    }
}

fn stall_key(id: u32) -> String {
    format!("schedule:{id}")
}

/// Queues the current step of every waiting schedule whose condition holds.
fn fire_schedules(
    mut events: EventReader<DayAdvanced>,
    calendar: Res<Calendar>,
    mut schedules: ResMut<Schedules>,
    mut queue: ResMut<ActionQueue>,
    mut next_id: ResMut<NextActionId>,
    registry: Res<FieldRegistry>,
    index: Res<FieldIndex>,
    tile_map: Res<TileMap>,
    tiles: Query<(&SoilState, Option<&PlantedCrop>)>,
    crop_defs: Res<Assets<CropDef>>,
    roster: Res<CropRoster>,
    soils: Res<SoilRoster>,
    mut messages: EventWriter<GameMessage>,
) {
    let days = events.read().count() as u32;
    for i in 0..days {
        let today = calendar.current_day + 1 + i;
        for schedule in schedules.items.iter_mut().filter(|s| s.enabled) {
            if registry.get(schedule.field).is_none() {
                continue;
            }
            // A running step whose action vanished (e.g. dropped while
            // loading a save) would otherwise wait forever.
            if let RunState::Running(id) = schedule.state {
                if !queue.pending.iter().any(|a| a.id == id) {
                    schedule.state = RunState::Stalled("its queued action is gone".into());
                }
                continue;
            }
            if schedule.state != RunState::Waiting {
                continue;
            }
            let Some(step) = schedule.current_step().cloned() else { continue };
            let stats = if needs_stats(step.wait) {
                field_stats(schedule.field, &index, &tile_map, &tiles, &crop_defs, &soils)
            } else {
                FieldStats::default()
            };
            if !wait_met(step.wait, today, calendar.year_length, schedule.since_day, &stats) {
                continue;
            }

            let crop_to_plant = if step.kind == ActionKind::Plant {
                let handle = step.crop_id.as_deref().and_then(|wanted| {
                    roster
                        .entries
                        .iter()
                        .find(|e| crop_defs.get(&e.handle).is_some_and(|d| d.id == wanted))
                        .map(|e| e.handle.clone())
                });
                if handle.is_none() {
                    let reason = match &step.crop_id {
                        Some(id) => format!("crop '{id}' is not available"),
                        None => "no crop chosen for the planting step".to_string(),
                    };
                    messages.write(stalled_message(today, &registry, schedule, &reason));
                    schedule.state = RunState::Stalled(reason);
                    continue;
                }
                handle
            } else {
                None
            };

            let id = next_id.0;
            next_id.0 += 1;
            queue.pending.push_back(PendingAction::new(
                id,
                TileOrField::Field(schedule.field),
                step.kind,
                crop_to_plant,
                step.water_depth_mm,
            ));
            schedule.state = RunState::Running(id);
        }
    }
}

fn stalled_message(day: u32, registry: &FieldRegistry, schedule: &Schedule, reason: &str) -> GameMessage {
    let field = registry.get(schedule.field).map_or("?", |f| f.name.as_str());
    GameMessage {
        day,
        kind: MessageKind::ScheduleStalled,
        severity: Severity::Warning,
        text: format!(
            "Schedule on {field} stopped at step {}: {reason}. Retry or skip it in the Schedules window",
            schedule.cursor + 1
        ),
        tile: None,
        field: Some(schedule.field),
        key: Some(stall_key(schedule.id)),
    }
}

/// Follows queued actions to their end, and tidies up after deleted fields.
fn track_schedules(
    mut ended: EventReader<ActionEnded>,
    calendar: Res<Calendar>,
    mut schedules: ResMut<Schedules>,
    registry: Res<FieldRegistry>,
    mut messages: EventWriter<GameMessage>,
    mut clears: EventWriter<ClearMessageKey>,
) {
    let today = calendar.current_day + 1;

    schedules.items.retain(|s| {
        let keep = registry.get(s.field).is_some();
        if !keep {
            clears.write(ClearMessageKey(stall_key(s.id)));
        }
        keep
    });

    for ev in ended.read() {
        let Some(schedule) = schedules.items.iter_mut().find(|s| s.state == RunState::Running(ev.id)) else {
            continue;
        };
        match ev.end {
            ActionEnd::Completed => {
                if schedule.advance(today) {
                    let field = registry.get(schedule.field).map_or("?", |f| f.name.as_str());
                    messages.write(GameMessage {
                        day: today,
                        kind: MessageKind::ScheduleDone,
                        severity: Severity::Info,
                        text: format!("Schedule on {field} has run all its steps"),
                        tile: None,
                        field: Some(schedule.field),
                        key: None,
                    });
                }
            }
            ActionEnd::Cancelled | ActionEnd::Dropped => {
                let reason = if ev.end == ActionEnd::Cancelled { "the action was cancelled" } else { "the action was dropped" };
                messages.write(stalled_message(today, &registry, schedule, reason));
                schedule.state = RunState::Stalled(reason.into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(planted: usize, mature: usize, avail: Option<f32>) -> FieldStats {
        FieldStats { planted, mature, mean_available: avail }
    }

    #[test]
    fn wait_conditions() {
        let none = stats(0, 0, None);
        assert!(wait_met(Wait::Immediately, 5, 112, 0, &none));
        assert!(!wait_met(Wait::AnyMature, 5, 112, 0, &stats(4, 0, None)));
        assert!(wait_met(Wait::AnyMature, 5, 112, 0, &stats(4, 1, None)));
        assert!(!wait_met(Wait::AllMature, 5, 112, 0, &stats(4, 3, None)));
        assert!(wait_met(Wait::AllMature, 5, 112, 0, &stats(4, 4, None)));
        assert!(!wait_met(Wait::AllMature, 5, 112, 0, &none), "an empty field is not 'all mature'");
        // Day 0 is day-of-year 1; the year wraps.
        assert!(wait_met(Wait::DayOfYear(1), 0, 112, 0, &none));
        assert!(wait_met(Wait::DayOfYear(1), 112, 112, 0, &none));
        assert!(wait_met(Wait::DayOfYear(10), 9 + 112, 112, 0, &none));
        assert!(!wait_met(Wait::DayOfYear(10), 10, 112, 0, &none));
        assert!(!wait_met(Wait::AfterDays(3), 12, 112, 10, &none));
        assert!(wait_met(Wait::AfterDays(3), 13, 112, 10, &none));
        assert!(wait_met(Wait::SoilDry(0.3), 5, 112, 0, &stats(0, 0, Some(0.2))));
        assert!(!wait_met(Wait::SoilDry(0.3), 5, 112, 0, &stats(0, 0, Some(0.5))));
        assert!(!wait_met(Wait::SoilDry(0.3), 5, 112, 0, &none));
    }

    fn two_steps(repeat: bool) -> Schedule {
        let mut s = Schedule::new(0, FieldId(0), vec![Step::new(ActionKind::Till, Wait::Immediately), Step::new(ActionKind::Harvest, Wait::AnyMature)], 0);
        s.repeat = repeat;
        s
    }

    #[test]
    fn advance_runs_once_or_wraps() {
        let mut once = two_steps(false);
        assert!(!once.advance(3));
        assert_eq!((once.cursor, once.since_day, &once.state), (1, 3, &RunState::Waiting));
        assert!(once.advance(5));
        assert_eq!(once.state, RunState::Done);

        let mut looping = two_steps(true);
        looping.advance(3);
        assert!(!looping.advance(5));
        assert_eq!((looping.cursor, &looping.state), (0, &RunState::Waiting));

        once.restart(9);
        assert_eq!((once.cursor, once.since_day, &once.state), (0, 9, &RunState::Waiting));
    }

    use crate::climate::ClimatePlugin;
    use crate::crops::{CropRosterEntry, CropsPlugin, Equipment};
    use crate::economy::{EconomyPlugin, Inventory};
    use crate::farm::{FarmConfig, FarmPlugin};
    use crate::field::{DeleteFieldRequested, FieldPlugin, PaintFieldRequested};
    use crate::crops::{CancelActionRequested, GrowthStage, QualityThresholds};
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
        app.add_plugins(SchedulePlugin);
        crate::sim_order::configure(&mut app);
        app.insert_resource(FarmConfig { width: 32, height: 32, ..default() });
        app.update();

        let handle = app.world_mut().resource_mut::<Assets<CropDef>>().add(CropDef {
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
            daily_water_use_mm: 0.0,
            fertility_requirement: 0.0,
            base_yield: 1,
            sell_price: 1,
            seed_cost: 1,
            quality_thresholds: QualityThresholds { premium_max_stress: 1.0, good_max_stress: 2.0, lethal_stress: 1000.0 },
            optimal_harvest_window_days: 100,
            overripe_stress_per_day: 0.0,
            fertility_impact_per_day: 0.0,
        });
        app.world_mut().resource_mut::<CropRoster>().entries.push(CropRosterEntry { file_stem: "t".into(), handle });

        let field = app.world_mut().resource_mut::<FieldRegistry>().create("A".into());
        app.world_mut().send_event(PaintFieldRequested { a: IVec2::new(0, 0), b: IVec2::new(1, 1), field: Some(field) });
        app.update();
        (app, field)
    }

    fn step_day(app: &mut App) {
        app.world_mut().send_event(StepDayRequested);
        app.update();
    }

    fn add_schedule(app: &mut App, field: FieldId, steps: Vec<Step>, repeat: bool) {
        let mut schedules = app.world_mut().resource_mut::<Schedules>();
        let id = schedules.create(field, steps, 0);
        schedules.items.iter_mut().find(|s| s.id == id).unwrap().repeat = repeat;
    }

    fn state(app: &App) -> RunState {
        app.world().resource::<Schedules>().items[0].state.clone()
    }

    fn full_cycle() -> Vec<Step> {
        let mut plant = Step::new(ActionKind::Plant, Wait::Immediately);
        plant.crop_id = Some("t".into());
        vec![
            Step::new(ActionKind::Till, Wait::Immediately),
            plant,
            Step::new(ActionKind::Harvest, Wait::AllMature),
        ]
    }

    fn crops_in_field(app: &mut App) -> usize {
        let mut q = app.world_mut().query::<&PlantedCrop>();
        q.iter(app.world()).count()
    }

    #[test]
    fn full_cycle_runs_till_plant_harvest_then_finishes() {
        let (mut app, field) = test_app();
        add_schedule(&mut app, field, full_cycle(), false);

        step_day(&mut app); // till
        assert_eq!(app.world().resource::<Schedules>().items[0].cursor, 1);
        step_day(&mut app); // plant
        assert_eq!(app.world().resource::<Schedules>().items[0].cursor, 2);
        assert_eq!(crops_in_field(&mut app), 4);

        // Waits for maturity rather than harvesting at once.
        step_day(&mut app);
        assert_eq!(state(&app), RunState::Waiting);
        assert_eq!(crops_in_field(&mut app), 4);

        for _ in 0..12 {
            step_day(&mut app);
        }
        assert_eq!(state(&app), RunState::Done);
        assert_eq!(crops_in_field(&mut app), 0);
        let inventory = app.world().resource::<Inventory>();
        assert!(inventory.harvested.get("t").is_some_and(|l| !l.is_empty()), "harvest reached the inventory");
    }

    #[test]
    fn repeating_schedule_starts_over() {
        let (mut app, field) = test_app();
        add_schedule(&mut app, field, full_cycle(), true);
        let mut wrapped = false;
        for _ in 0..30 {
            step_day(&mut app);
            let s = &app.world().resource::<Schedules>().items[0];
            assert_ne!(s.state, RunState::Done);
            if s.cursor == 0 && s.since_day > 0 {
                wrapped = true;
            }
        }
        assert!(wrapped, "cursor should have wrapped back to step 0");
    }

    #[test]
    fn cancelling_the_running_action_stalls_the_schedule() {
        let (mut app, field) = test_app();
        app.world_mut().resource_mut::<Equipment>().daily_capacity.insert(ActionKind::Till, 1);
        add_schedule(&mut app, field, full_cycle(), false);
        step_day(&mut app); // till queued, 1 of 4 tiles done
        let RunState::Running(id) = state(&app) else { panic!("expected running, got {:?}", state(&app)) };

        app.world_mut().send_event(CancelActionRequested(id));
        app.update();
        assert!(matches!(state(&app), RunState::Stalled(_)));
        let log = app.world().resource::<crate::messages::MessageLog>();
        assert!(log.entries.iter().any(|m| m.kind == MessageKind::ScheduleStalled));

        // Stalled schedules do not fire again on their own.
        step_day(&mut app);
        assert!(app.world().resource::<ActionQueue>().pending.is_empty());
    }

    #[test]
    fn deleting_the_field_removes_its_schedules() {
        let (mut app, field) = test_app();
        add_schedule(&mut app, field, full_cycle(), false);
        app.world_mut().send_event(DeleteFieldRequested(field));
        app.update();
        app.update();
        assert!(app.world().resource::<Schedules>().items.is_empty());
    }

    #[test]
    fn disabled_schedules_do_not_fire() {
        let (mut app, field) = test_app();
        add_schedule(&mut app, field, full_cycle(), false);
        app.world_mut().resource_mut::<Schedules>().items[0].enabled = false;
        step_day(&mut app);
        assert_eq!(state(&app), RunState::Waiting);
        assert!(app.world().resource::<ActionQueue>().pending.is_empty());
    }

    #[test]
    fn unknown_crop_stalls_with_a_message() {
        let (mut app, field) = test_app();
        let mut plant = Step::new(ActionKind::Plant, Wait::Immediately);
        plant.crop_id = Some("nonexistent".into());
        add_schedule(&mut app, field, vec![plant], false);
        step_day(&mut app);
        assert!(matches!(state(&app), RunState::Stalled(_)));
    }

    #[test]
    fn schedules_round_trip_through_json() {
        let mut schedules = Schedules::default();
        schedules.create(FieldId(2), full_cycle(), 7);
        schedules.items[0].state = RunState::Running(41);
        let json = serde_json::to_string(&schedules).unwrap();
        let back: Schedules = serde_json::from_str(&json).unwrap();
        assert_eq!(back.items[0].steps, schedules.items[0].steps);
        assert_eq!(back.items[0].state, RunState::Running(41));
        assert_eq!(back.next_id, 1);
    }
}
