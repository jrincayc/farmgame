//! The Schedules window (design doc §3.11): create, edit and control the
//! per-field action sequences from `crate::schedule`.

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};

use super::SchedulesWindowOpen;
use crate::crops::{ActionKind, CropDef, CropRoster};
use crate::field::{FieldId, FieldRegistry};
use crate::messages::ClearMessageKey;
use crate::schedule::{RunState, Schedules, Step, Wait};
use crate::time::Calendar;

const KINDS: [ActionKind; 6] = [
    ActionKind::Till,
    ActionKind::Plant,
    ActionKind::Water,
    ActionKind::Fertilize,
    ActionKind::Harvest,
    ActionKind::Clear,
];

fn wait_index(w: &Wait) -> usize {
    match w {
        Wait::Immediately => 0,
        Wait::AnyMature => 1,
        Wait::AllMature => 2,
        Wait::DayOfYear(_) => 3,
        Wait::AfterDays(_) => 4,
        Wait::SoilDry(_) => 5,
    }
}

fn default_wait(index: usize) -> Wait {
    match index {
        0 => Wait::Immediately,
        1 => Wait::AnyMature,
        2 => Wait::AllMature,
        3 => Wait::DayOfYear(1),
        4 => Wait::AfterDays(1),
        _ => Wait::SoilDry(0.3),
    }
}

const WAIT_NAMES: [&str; 6] = [
    "immediately",
    "when any crop is mature",
    "when all crops are mature",
    "on day of year",
    "days after previous step",
    "when soil is dry",
];

fn state_text(state: &RunState) -> String {
    match state {
        RunState::Waiting => "waiting for its turn".into(),
        RunState::Running(id) => format!("running (action #{id})"),
        RunState::Stalled(why) => format!("STOPPED: {why}"),
        RunState::Done => "finished".into(),
    }
}

/// A fresh Till -> Plant -> Harvest cycle for the first crop in the roster.
fn full_cycle(roster: &CropRoster, crop_defs: &Assets<CropDef>) -> Vec<Step> {
    let mut plant = Step::new(ActionKind::Plant, Wait::Immediately);
    plant.crop_id = roster
        .entries
        .first()
        .and_then(|e| crop_defs.get(&e.handle))
        .map(|d| d.id.clone());
    vec![
        Step::new(ActionKind::Till, Wait::Immediately),
        plant,
        Step::new(ActionKind::Harvest, Wait::AllMature),
    ]
}

pub fn schedules_window(
    mut contexts: EguiContexts,
    mut open: ResMut<SchedulesWindowOpen>,
    mut schedules: ResMut<Schedules>,
    registry: Res<FieldRegistry>,
    roster: Res<CropRoster>,
    crop_defs: Res<Assets<CropDef>>,
    calendar: Res<Calendar>,
    mut clears: EventWriter<ClearMessageKey>,
    mut new_field: Local<Option<FieldId>>,
) {
    if !open.0 {
        return;
    }
    let Some(ctx) = contexts.try_ctx_mut() else {
        return;
    };
    let mut window_open = true;
    let today = calendar.current_day;
    let mut delete = None;
    let mut clear_stall: Vec<u32> = Vec::new();

    egui::Window::new("Schedules").open(&mut window_open).default_width(840.0).show(ctx, |ui| {
        ui.label("A schedule runs its steps in order on one field, each when its wait condition is met.");

        for schedule in schedules.items.iter_mut() {
            let field_name = registry.get(schedule.field).map_or("?".to_string(), |f| f.name.clone());
            let running = matches!(schedule.state, RunState::Running(_));
            let title = format!(
                "{field_name} - step {}/{} - {}",
                (schedule.cursor + 1).min(schedule.steps.len().max(1)),
                schedule.steps.len(),
                state_text(&schedule.state)
            );
            egui::CollapsingHeader::new(title)
                .id_salt(("schedule", schedule.id))
                .default_open(true)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut schedule.enabled, "Enabled");
                        // Turning Repeat on revives a finished schedule.
                        if ui.checkbox(&mut schedule.repeat, "Repeat").changed()
                            && schedule.repeat
                            && schedule.state == RunState::Done
                        {
                            schedule.restart(today + 1);
                        }
                        if ui.small_button("Delete").clicked() {
                            delete = Some(schedule.id);
                        }
                    });
                    match &schedule.state {
                        RunState::Stalled(_) => {
                            ui.horizontal(|ui| {
                                if ui.button("Retry step").clicked() {
                                    schedule.state = RunState::Waiting;
                                    clear_stall.push(schedule.id);
                                }
                                if ui.button("Skip step").clicked() {
                                    schedule.advance(today + 1);
                                    clear_stall.push(schedule.id);
                                }
                            });
                        }
                        RunState::Done => {
                            if ui.button("Restart").clicked() {
                                schedule.restart(today + 1);
                            }
                        }
                        _ => {}
                    }
                    if running {
                        ui.weak("(steps are locked while an action is running)");
                    }

                    let mut remove = None;
                    let mut swap = None;
                    let count = schedule.steps.len();
                    let cursor = schedule.cursor;
                    ui.add_enabled_ui(!running, |ui| {
                        for (i, step) in schedule.steps.iter_mut().enumerate() {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(if i == cursor { "\u{25B6}" } else { "  " });
                                ui.label(format!("{}.", i + 1));
                                egui::ComboBox::from_id_salt(("kind", schedule.id, i))
                                    .width(80.0)
                                    .selected_text(format!("{:?}", step.kind))
                                    .show_ui(ui, |ui| {
                                        for kind in KINDS {
                                            ui.selectable_value(&mut step.kind, kind, format!("{kind:?}"));
                                        }
                                    });
                                match step.kind {
                                    ActionKind::Plant => {
                                        let current = step
                                            .crop_id
                                            .as_ref()
                                            .map_or("(choose crop)".to_string(), |id| id.clone());
                                        egui::ComboBox::from_id_salt(("crop", schedule.id, i))
                                            .width(90.0)
                                            .selected_text(current)
                                            .show_ui(ui, |ui| {
                                                for entry in &roster.entries {
                                                    let Some(def) = crop_defs.get(&entry.handle) else { continue };
                                                    ui.selectable_value(&mut step.crop_id, Some(def.id.clone()), &def.name);
                                                }
                                            });
                                    }
                                    ActionKind::Water => {
                                        ui.add(egui::DragValue::new(&mut step.water_depth_mm).range(1.0..=60.0).suffix(" mm"));
                                    }
                                    _ => {}
                                }
                                let mut which = wait_index(&step.wait);
                                let before = which;
                                egui::ComboBox::from_id_salt(("wait", schedule.id, i))
                                    .width(170.0)
                                    .selected_text(WAIT_NAMES[which])
                                    .show_ui(ui, |ui| {
                                        for (n, name) in WAIT_NAMES.iter().enumerate() {
                                            ui.selectable_value(&mut which, n, *name);
                                        }
                                    });
                                if which != before {
                                    step.wait = default_wait(which);
                                }
                                match &mut step.wait {
                                    Wait::DayOfYear(d) => {
                                        ui.add(egui::DragValue::new(d).range(1..=calendar.year_length.max(1)));
                                    }
                                    Wait::AfterDays(n) => {
                                        ui.add(egui::DragValue::new(n).range(0..=365));
                                    }
                                    Wait::SoilDry(f) => {
                                        ui.add(egui::Slider::new(f, 0.05..=0.95).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)));
                                    }
                                    _ => {}
                                }
                                if ui.small_button("Up").clicked() && i > 0 {
                                    swap = Some((i - 1, i));
                                }
                                if ui.small_button("Down").clicked() && i + 1 < count {
                                    swap = Some((i, i + 1));
                                }
                                if ui.small_button("Remove").clicked() {
                                    remove = Some(i);
                                }
                            });
                        }
                        if ui.button("Add step").clicked() {
                            schedule.steps.push(Step::new(ActionKind::Till, Wait::Immediately));
                            if schedule.state == RunState::Done {
                                schedule.state = RunState::Waiting;
                            }
                        }
                    });
                    if let Some((a, b)) = swap {
                        schedule.steps.swap(a, b);
                    }
                    if let Some(i) = remove {
                        schedule.steps.remove(i);
                        if i < schedule.cursor {
                            schedule.cursor -= 1;
                        }
                        if schedule.cursor >= schedule.steps.len() && !schedule.steps.is_empty() {
                            schedule.cursor = 0;
                        }
                    }
                });
        }

        ui.separator();
        ui.horizontal(|ui| {
            ui.label("New schedule for");
            let label = new_field
                .and_then(|id| registry.get(id))
                .map_or("(choose field)".to_string(), |f| f.name.clone());
            egui::ComboBox::from_id_salt("schedule_new_field").selected_text(label).show_ui(ui, |ui| {
                for info in &registry.fields {
                    ui.selectable_value(&mut *new_field, Some(info.id), &info.name);
                }
            });
        });
        ui.horizontal(|ui| {
            if let Some(field) = *new_field {
                if registry.get(field).is_some() {
                    if ui.button("Till, Plant, Harvest").clicked() {
                        let steps = full_cycle(&roster, &crop_defs);
                        schedules.create(field, steps, today + 1);
                    }
                    if ui.button("Empty").clicked() {
                        schedules.create(field, Vec::new(), today + 1);
                    }
                }
            }
        });
    });

    if let Some(id) = delete {
        schedules.items.retain(|s| s.id != id);
        clear_stall.push(id);
    }
    for id in clear_stall {
        clears.write(ClearMessageKey(format!("schedule:{id}")));
    }
    if !window_open {
        open.0 = false;
    }
}
