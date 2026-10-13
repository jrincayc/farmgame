//! egui panels (Phase 6, + camera zoom buttons from the pan/zoom batch,
//! + a save/load panel from Phase 7) - the real UI replacing the dev
//! keyboard controls from Phases 4-5. Reads/writes the same resources
//! those dev controls did (`ActionQueue`, `Wallet`, `Inventory`,
//! `TimeControl`), so this is a UI-only addition, not a
//! simulation-logic change.
//!
//! Requirements §4.6a: the action-queue panel is intentionally
//! FIFO-display-only - no reorder/cancel controls, even though
//! `PendingAction::id` already supports that data-side. See the
//! requirements doc's resolved §9 note for why that's deferred rather
//! than missing.

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};

use crate::climate::{Region, WeatherToday};
use crate::crops::{
    days_until_mature, is_mature, overripe_days, ActionKind, ActionQueue, CropDef, CropRoster,
    NextActionId, PendingAction, RangeStatus, TileOrField
};
use crate::field::{ActionTargetField, DeleteFieldRequested, FieldBrush, FieldId, FieldIndex, FieldRegistry, TileField};
use crate::economy::{Inventory, SellRequested, Wallet};
use crate::farm::{FarmConfig, TileMap};
use crate::save::{DeleteSaveRequested, LoadRequested, SaveRequested, SAVE_DIR};
use crate::time::{Calendar, StepDayRequested, TimeControl, TimeMode, TimerSpeed};

use super::render::{farm_world_center, tile_world_pos, BUTTON_ZOOM_STEP, MAX_ZOOM_FACTOR, MIN_ZOOM_FACTOR};
use super::{CameraState, MessagesWindowOpen, OptionsWindowOpen, SaveLoadWindowOpen, SelectedTile, ShowSoilOverlay, UiTheme};
use crate::messages::{GameOptions, MessageLog, Severity};
use crate::soil::{self, SoilRoster};
use crate::farm::SoilState;
use crate::crops::PlantedCrop;
use crate::crops::crop_def;

/// Display label for `RangeStatus` in the context of temperature.
/// Phase 8 - the tile inspector's stress-diagnostics display.
fn temp_status_label(status: RangeStatus) -> &'static str {
    match status {
        RangeStatus::BelowMin => "too cold (risk of death)",
        RangeStatus::AboveMax => "too hot (risk of death)",
        RangeStatus::BelowIdeal => "a bit cold",
        RangeStatus::AboveIdeal => "a bit hot",
        RangeStatus::Ideal => "ideal",
    }
}

/// Drought-axis wording for `RangeStatus` (plant-available water).
fn drought_status_label(status: RangeStatus) -> &'static str {
    match status {
        RangeStatus::BelowMin => "too dry (risk of death)",
        RangeStatus::BelowIdeal => "a bit dry",
        _ => "ideal",
    }
}

/// Waterlogging-axis wording for `RangeStatus` (air-filled porosity).
fn waterlog_status_label(status: RangeStatus) -> &'static str {
    match status {
        RangeStatus::BelowMin => "waterlogged (risk of death)",
        RangeStatus::BelowIdeal => "soggy",
        _ => "ideal",
    }
}

fn apply_theme(ctx: &egui::Context, theme: &UiTheme) {
    ctx.set_visuals(if theme.dark { egui::Visuals::dark() } else { egui::Visuals::light() });
}

fn season_label(day: u32, year_length: u32) -> &'static str {
    let year_length = year_length.max(1);
    let quarter = (year_length / 4).max(1);
    match (day % year_length) / quarter {
        0 => "Spring",
        1 => "Summer",
        2 => "Fall",
        _ => "Winter",
    }
}

fn time_mode_label(mode: &TimeMode) -> String {
    match mode {
        TimeMode::Paused => "Paused".to_string(),
        TimeMode::Timer { speed, .. } => format!("Timer ({:?})", speed),
    }
}

/// Top bar: day/season readout, time control, theme toggle, and camera
/// zoom controls. Applies the current `UiTheme` to the egui context
/// every frame (idempotent, cheap) rather than only on toggle.
pub fn top_bar(
    mut contexts: EguiContexts,
    theme: Res<UiTheme>,
    calendar: Res<Calendar>,
    weather: Res<WeatherToday>,
    mut control: ResMut<TimeControl>,
    mut step_requests: EventWriter<StepDayRequested>,
    mut camera_state: ResMut<CameraState>,
    config: Res<FarmConfig>,
    mut save_load_open: ResMut<SaveLoadWindowOpen>,
    mut soil_overlay: ResMut<ShowSoilOverlay>,
    mut brush: ResMut<FieldBrush>,
    log: Res<MessageLog>,
    mut messages_open: ResMut<MessagesWindowOpen>,
    mut options_open: ResMut<OptionsWindowOpen>,
) {
    let ctx = contexts.ctx_mut();
    apply_theme(ctx, &theme);

    egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.label(format!(
                "Day {} ({})",
                calendar.current_day,
                season_label(calendar.current_day, calendar.year_length)
            ));

            ui.separator();

            // Phase 8 addition: today's weather was previously only
            // visible via console info!() logs from the old dev-controls
            // path - a real gap against Pillar 2 ("player agency
            // through information," requirements §2), since watering
            // decisions depend on knowing whether it already rained.
            //
            // Fixed-width slots (add_sized, not label) so "Dry" vs
            // "Rain: 0.62" vs a frost callout don't change the row's
            // total width and shove Step/Pause/etc. sideways under the
            // cursor - reported as a real usability problem, not just
            // cosmetic, since it made mis-clicks likely right after a
            // weather change.
            let rain_text = if weather.rainfall > 0.0 {
                format!("Rain: {:.2}", weather.rainfall)
            } else {
                "Dry".to_string()
            };
            ui.add_sized([70.0, ui.spacing().interact_size.y], egui::Label::new(rain_text));

            let frost_text = egui::RichText::new(if weather.is_frost { "Frost" } else { "" })
                .color(egui::Color32::LIGHT_BLUE);
            ui.add_sized([45.0, ui.spacing().interact_size.y], egui::Label::new(frost_text));

            ui.separator();

            if ui.button("Step").clicked() {
                step_requests.write(StepDayRequested);
            }
            if ui.button("Pause").clicked() {
                control.mode = TimeMode::Paused;
            }
            if ui.button("Slow").clicked() {
                control.mode = TimeMode::Timer {
                    speed: TimerSpeed::Slow,
                    elapsed_ms: 0.0,
                };
            }
            if ui.button("Normal").clicked() {
                control.mode = TimeMode::Timer {
                    speed: TimerSpeed::Normal,
                    elapsed_ms: 0.0,
                };
            }
            if ui.button("Fast").clicked() {
                control.mode = TimeMode::Timer {
                    speed: TimerSpeed::Fast,
                    elapsed_ms: 0.0,
                };
            }

            ui.separator();
            ui.label(time_mode_label(&control.mode));

            ui.separator();
            if ui.button("Zoom Out").clicked() {
                camera_state.zoom_factor =
                    (camera_state.zoom_factor / BUTTON_ZOOM_STEP).clamp(MIN_ZOOM_FACTOR, MAX_ZOOM_FACTOR);
            }
            if ui.button("Zoom In").clicked() {
                camera_state.zoom_factor =
                    (camera_state.zoom_factor * BUTTON_ZOOM_STEP).clamp(MIN_ZOOM_FACTOR, MAX_ZOOM_FACTOR);
            }
            if ui.button("Reset View").clicked() {
                camera_state.zoom_factor = MIN_ZOOM_FACTOR;
                camera_state.center = farm_world_center(&config);
            }

            ui.separator();
            // Phase 8: was an always-open floating window; now toggled
            // from here, with egui's own window close button (×) wired
            // back to the same SaveLoadWindowOpen resource.
            if ui.button("Save / Load").clicked() {
                save_load_open.0 = !save_load_open.0;
            }

            ui.checkbox(&mut soil_overlay.0, "Soil types");
            if ui.button("Fields").clicked() {
                brush.window_open = !brush.window_open;
                if !brush.window_open {
                    brush.paint = false;
                }
            }

            let unread = log.unread();
            let label = if unread > 0 {
                egui::RichText::new(format!("Messages ({unread})")).color(egui::Color32::from_rgb(220, 120, 0))
            } else {
                egui::RichText::new("Messages")
            };
            if ui.button(label).clicked() {
                messages_open.0 = !messages_open.0;
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Options").clicked() {
                    options_open.0 = !options_open.0;
                }
            });
        });
    });
}

/// Selected-tile inspector (design doc §2.3/§2.4's fields, surfaced
/// read-only).
pub fn tile_inspector_panel(
    mut contexts: EguiContexts,
    selected: Res<SelectedTile>,
    tile_map: Res<TileMap>,
    tiles: Query<(&SoilState, Option<&PlantedCrop>)>,
    crop_defs: Res<Assets<CropDef>>,
    // Phase 8: needed to reproduce today's temperature/frost the same
    // way growth::tick_growth does, for the stress-diagnostics display.
    region: Res<Region>,
    calendar: Res<Calendar>,
    weather: Res<WeatherToday>,
    soils: Res<SoilRoster>,
    registry: Res<FieldRegistry>,
    index: Res<FieldIndex>,
    tile_fields: Query<&TileField>,
    field_tiles: Query<(&SoilState, Option<&PlantedCrop>)>,
) {
    egui::SidePanel::right("tile_inspector").show(contexts.ctx_mut(), |ui| {
        ui.heading("Selected Tile");

        let Some(coord) = selected.0 else {
            ui.label("Click a tile to inspect it.");
            return;
        };
        ui.label(format!("Coord: ({}, {})", coord.x, coord.y));

        let Some(&entity) = tile_map.tiles.get(&coord) else {
            ui.label("Tile not found.");
            return;
        };
        let Ok((soil, planted)) = tiles.get(entity) else {
            ui.label("Tile data unavailable.");
            return;
        };

        let soil_def = soils.get(soil.soil as usize);
        let available = soil_def.available(soil.water_mm);
        let air = soil_def.air(soil.water_mm);
        ui.label(format!("Soil type: {}", soil_def.name));
        ui.label(format!("Fertility: {:.2}", soil.fertility));
        ui.label(format!(
            "Water: {:.0} mm (available {:.0}%, air {:.2})",
            soil.water_mm,
            available * 100.0,
            air
        ));
        ui.label(format!("Tilled: {}", soil.tilled));

        let field_id = tile_fields.get(entity).ok().and_then(|f| f.0);
        match field_id.and_then(|id| registry.get(id)) {
            Some(info) => {
                ui.label(format!("Field: {}", info.name));
            }
            None => {
                ui.label("Field: (none)");
            }
        }

        ui.separator();

        if let Some(id) = field_id {
            field_summary(ui, id, &registry, &index, &tile_map, &field_tiles, &soils, &crop_defs);
            ui.separator();
        }

        match planted {
            None => {
                ui.label("No crop planted.");
            }
            Some(crop) if crop.dead => {
                ui.colored_label(egui::Color32::RED, "Crop is DEAD - queue Clear to remove it.");
            }
            Some(crop) => match crop_defs.get(&crop.def) {
                Some(def) => {
                    let stage_name = def
                        .growth_stages
                        .get(crop.current_stage)
                        .map(|s| s.name.as_str())
                        .unwrap_or("?");
                    ui.label(format!("Crop: {}", def.name));
                    ui.label(format!(
                        "Stage: {} ({}/{})",
                        stage_name,
                        crop.current_stage + 1,
                        def.growth_stages.len()
                    ));
                    ui.label(format!("Days in stage: {}", crop.days_in_stage));

                    // Phase 8: harvest-readiness, previously only
                    // inferrable by comparing days_in_stage against the
                    // stage's duration_days by hand.
                    if is_mature(crop, def) {
                        ui.colored_label(egui::Color32::from_rgb(0, 140, 0), "Ready to harvest!");
                    } else if let Some(days) = days_until_mature(crop, def) {
                        ui.label(format!("{} day(s) until mature", days));
                    }

                    ui.separator();
                    ui.label("Today's growing conditions:");

                    // Phase 8: same computation growth::tick_growth
                    // uses, via the shared crop_def::compute_daily_stress
                    // - see that function's doc comment for why this
                    // isn't a second copy of the stress logic.
                    let year_length = calendar.year_length.max(1);
                    let doy = calendar.current_day % year_length;
                    let temp = region.base_temp_curve.temp_on(doy, year_length);
                    let overripe = overripe_days(crop, def);
                    let breakdown = crop_def::compute_daily_stress(
                        def,
                        temp,
                        weather.is_frost,
                        available,
                        air,
                        soil.fertility,
                        overripe,
                    );

                    ui.label(format!(
                        "  Temp: {:.1}\u{00B0}C (ideal {:.1}-{:.1}) - {}",
                        temp,
                        def.ideal_temp_range.0,
                        def.ideal_temp_range.1,
                        temp_status_label(breakdown.temp_status)
                    ));
                    if def.frost_tolerant && weather.is_frost {
                        ui.label("  Frost today, but this crop is frost-tolerant.");
                    } else if breakdown.frost_hit {
                        ui.colored_label(egui::Color32::RED, "  Frost today - not frost-tolerant!");
                    }
                    ui.label(format!(
                        "  Available water: {:.2} (ideal {:.2}+) - {}",
                        available,
                        def.ideal_available_water,
                        drought_status_label(breakdown.drought_status)
                    ));
                    ui.label(format!(
                        "  Air in soil: {:.2} (ideal {:.2}+) - {}",
                        air,
                        def.ideal_air_porosity,
                        waterlog_status_label(breakdown.waterlog_status)
                    ));
                    let et = soil::daily_et_mm(soil_def, Some((def, crop.current_stage)), temp);
                    let days_left = (soil.water_mm - soil_def.wilting_mm()).max(0.0) / et.max(f32::EPSILON);
                    ui.label(format!(
                        "  Water use today: {:.1} mm (about {:.0} day(s) of water left)",
                        et, days_left
                    ));
                    ui.label(format!(
                        "  Fertility: {:.2} (needs {:.2}) - {}",
                        soil.fertility,
                        def.fertility_requirement,
                        if breakdown.fertility_ok { "OK" } else { "too low" }
                    ));
                    if breakdown.overripe_days > 0 {
                        ui.colored_label(
                            egui::Color32::from_rgb(200, 120, 0),
                            format!("  Overripe by {} day(s)", breakdown.overripe_days),
                        );
                    }
                    ui.label(format!("  Today's total stress: {:.2}", breakdown.total));

                    ui.separator();
                    ui.label(format!("Cumulative stress: {:.2}", crop.cumulative_stress));
                    ui.label(format!("Worst single-day stress: {:.2}", crop.worst_single_day_stress));
                    ui.label(format!(
                        "Quality: {}",
                        crop_def::quality_grade(crop.worst_single_day_stress, &def.quality_thresholds)
                    ));
                }
                None => {
                    ui.label("Crop: (asset still loading...)");
                }
            },
        }
    });
}

fn enqueue(
    queue: &mut ActionQueue,
    next_id: &mut NextActionId,
    target: TileOrField,
    kind: ActionKind,
    crop_to_plant: Option<Handle<CropDef>>,
    water_depth_mm: f32,
) {
    let id = next_id.0;
    next_id.0 += 1;
    queue
        .pending
        .push_back(PendingAction::new(id, target, kind, crop_to_plant, water_depth_mm));
}

pub fn action_queue_panel(
    mut contexts: EguiContexts,
    selected: Res<SelectedTile>,
    tile_map: Res<TileMap>,
    roster: Res<CropRoster>,
    crop_defs: Res<Assets<CropDef>>,
    mut queue: ResMut<ActionQueue>,
    mut next_id: ResMut<NextActionId>,
    registry: Res<FieldRegistry>,
    index: Res<FieldIndex>,
    mut crop_choice: Local<usize>,
    mut water_depth: Local<Option<f32>>,
    mut field_target: ResMut<ActionTargetField>,
) {
    egui::SidePanel::left("action_queue").show(contexts.ctx_mut(), |ui| {
        ui.heading("Actions");

        // Target: the selected tile, or a whole field.
        if field_target.0.is_some_and(|id| registry.get(id).is_none()) {
            field_target.0 = None;
        }
        let target_label = match field_target.0 {
            Some(id) => registry.get(id).map(|f| f.name.clone()).unwrap_or_default(),
            None => "Selected tile".to_string(),
        };
        egui::ComboBox::from_label("Target")
            .selected_text(target_label)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut field_target.0, None, "Selected tile");
                for f in &registry.fields {
                    ui.selectable_value(&mut field_target.0, Some(f.id), &f.name);
                }
            });

        let target: Option<TileOrField> = match field_target.0 {
            Some(id) => Some(TileOrField::Field(id)),
            None => selected
                .0
                .and_then(|coord| tile_map.tiles.get(&coord).copied())
                .map(TileOrField::Tile),
        };
        if target.is_none() {
            ui.label("Select a tile (or choose a field) to queue actions.");
        }

        ui.add_enabled_ui(target.is_some(), |ui| {
            let mut queue_action = |kind: ActionKind, crop: Option<Handle<CropDef>>, depth: f32| {
                if let Some(t) = target {
                    enqueue(&mut queue, &mut next_id, t, kind, crop, depth);
                }
            };

            if ui.button("Till").clicked() {
                queue_action(ActionKind::Till, None, 0.0);
            }
            // Water depth in mm: 5 mm steps with 10/20/30 presets.
            let depth = water_depth.get_or_insert(20.0);
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(depth).range(5.0..=60.0).speed(5.0).suffix(" mm"));
                for preset in [10.0, 20.0, 30.0] {
                    if ui.small_button(format!("{preset:.0}")).clicked() {
                        *depth = preset;
                    }
                }
            });
            *depth = (*depth / 5.0).round().max(1.0) * 5.0;
            if ui.button(format!("Water {:.0} mm", *depth)).clicked() {
                queue_action(ActionKind::Water, None, *depth);
            }
            if ui.button("Fertilize").clicked() {
                queue_action(ActionKind::Fertilize, None, 0.0);
            }
            if ui.button("Harvest").clicked() {
                queue_action(ActionKind::Harvest, None, 0.0);
            }
            if ui.button("Clear").clicked() {
                queue_action(ActionKind::Clear, None, 0.0);
            }

            ui.separator();

            if roster.entries.is_empty() {
                ui.label("No crops found under assets/crops/ - nothing to plant.");
            } else {
                if *crop_choice >= roster.entries.len() {
                    *crop_choice = 0;
                }

                let selected_label = crop_display_name(&roster.entries[*crop_choice], &crop_defs);
                egui::ComboBox::from_label("Crop")
                    .selected_text(selected_label)
                    .show_ui(ui, |ui| {
                        for (i, entry) in roster.entries.iter().enumerate() {
                            let label = crop_display_name(entry, &crop_defs);
                            ui.selectable_value(&mut *crop_choice, i, label);
                        }
                    });
                if ui.button("Plant").clicked() {
                    let handle = roster.entries[*crop_choice].handle.clone();
                    queue_action(ActionKind::Plant, Some(handle), 0.0);
                }
            }
        });

        ui.separator();
        ui.heading("Pending Queue (FIFO)");
        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
            if queue.pending.is_empty() {
                ui.label("(empty)");
            }
            for action in &queue.pending {
                let what = if action.kind == ActionKind::Water {
                    format!("Water {:.0} mm", action.water_depth_mm)
                } else {
                    format!("{:?}", action.kind)
                };
                match action.target {
                    TileOrField::Tile(_) => {
                        ui.label(format!("#{} - {} (tile)", action.id, what));
                    }
                    TileOrField::Field(id) => {
                        let name = registry.get(id).map(|f| f.name.as_str()).unwrap_or("?");
                        let remaining = index
                            .tiles_of(id)
                            .filter(|c| !action.visited.contains(c))
                            .count();
                        ui.label(format!(
                            "#{} - {} on {}: {} applied, {} skipped, {} remaining",
                            action.id, what, name, action.applied, action.skipped, remaining
                        ));
                    }
                }
            }
        });
    });
}

/// Floating window for creating, renaming, deleting and painting fields.
pub fn fields_window(
    mut contexts: EguiContexts,
    mut brush: ResMut<FieldBrush>,
    mut registry: ResMut<FieldRegistry>,
    index: Res<FieldIndex>,
    mut deletes: EventWriter<DeleteFieldRequested>,
    mut name_buffers: Local<std::collections::HashMap<FieldId, String>>,
) {
    if !brush.window_open {
        return;
    }
    let mut open = true;
    egui::Window::new("Fields").open(&mut open).show(contexts.ctx_mut(), |ui| {
        ui.checkbox(&mut brush.paint, "Paint mode (left-drag a rectangle on the map)");
        ui.radio_value(&mut brush.active, None, "Unassigned (eraser)");

        let mut to_delete = None;
        let ids: Vec<FieldId> = registry.fields.iter().map(|f| f.id).collect();
        for id in ids {
            let Some(info) = registry.get(id).cloned() else { continue };
            let buffer = name_buffers.entry(id).or_insert_with(|| info.name.clone());
            ui.horizontal(|ui| {
                let [r, g, b] = info.color;
                ui.colored_label(egui::Color32::from_rgb(r, g, b), "\u{25A0}");
                ui.radio_value(&mut brush.active, Some(id), "");
                let response = ui.add(egui::TextEdit::singleline(buffer).desired_width(110.0));
                if response.lost_focus() {
                    // Names are unique and non-empty; otherwise revert.
                    if !registry.rename(id, buffer) {
                        *buffer = info.name.clone();
                    } else {
                        *buffer = buffer.trim().to_string();
                    }
                }
                ui.label(format!("{} tiles", index.len_of(id)));
                if ui.small_button("Delete").clicked() {
                    to_delete = Some(id);
                }
            });
        }
        if let Some(id) = to_delete {
            deletes.write(DeleteFieldRequested(id));
            name_buffers.remove(&id);
        }

        if ui.button("New field").clicked() {
            let name = registry.suggest_name();
            let id = registry.create(name);
            brush.active = Some(id);
        }
    });
    if !open {
        brush.window_open = false;
        brush.paint = false;
    }
}

fn crop_display_name(entry: &crate::crops::CropRosterEntry, crop_defs: &Assets<CropDef>) -> String {
    crop_defs
        .get(&entry.handle)
        .map(|def| def.name.clone())
        .unwrap_or_else(|| entry.file_stem.clone())
}

pub fn economy_panel(
    mut contexts: EguiContexts,
    wallet: Res<Wallet>,
    inventory: Res<Inventory>,
    mut sell_requests: EventWriter<SellRequested>,
) {
    egui::TopBottomPanel::bottom("economy_panel").show(contexts.ctx_mut(), |ui| {
        ui.horizontal(|ui| {
            ui.heading(format!("Wallet: {}", wallet.money));
            ui.separator();

            if inventory.harvested.is_empty() {
                ui.label("Inventory: (empty)");
            } else {
                for (crop_id, lots) in inventory.harvested.iter() {
                    let total: u32 = lots.iter().map(|lot| lot.quantity).sum();
                    ui.label(format!("{} x{}", crop_id, total));
                    if ui.button(format!("Sell {}", crop_id)).clicked() {
                        sell_requests.write(SellRequested {
                            crop_id: crop_id.clone(),
                        });
                    }
                    ui.separator();
                }
            }
        });
    });
}

/// Save/load panel (Phase 7) - a floating egui window rather than a
/// docked side/top/bottom panel, so it doesn't compete for screen space
/// with the four panels above; the player can drag it out of the way or
/// collapse it via egui's own window chrome.
///
/// Re-scans `save/*.json` from disk every frame the window is open
/// (`std::fs::read_dir`, same low-tech pattern `crops::roster::
/// CropRoster` uses at startup) rather than caching a save-list
/// resource - simplest thing that works at "a handful of save files"
/// scale, and it means a save made in one click always shows up in the
/// list without needing a manual refresh or a change-detection setup.
pub fn save_load_panel(
    mut contexts: EguiContexts,
    mut save_name: Local<String>,
    mut save_requests: EventWriter<SaveRequested>,
    mut load_requests: EventWriter<LoadRequested>,
    mut delete_requests: EventWriter<DeleteSaveRequested>,
    mut open: ResMut<SaveLoadWindowOpen>,
) {
    // Phase 8: gated behind SaveLoadWindowOpen (toggled from the top
    // bar) instead of always-open. `.open(&mut open.0)` gives the
    // window a close (×) button that writes back to the same resource,
    // so either the top-bar button or the window's own chrome closes it.
    if !open.0 {
        return;
    }
    egui::Window::new("Save / Load").open(&mut open.0).show(contexts.ctx_mut(), |ui| {
        ui.horizontal(|ui| {
            ui.label("Name:");
            ui.text_edit_singleline(&mut *save_name);
            let name_trimmed = save_name.trim();
            if ui
                .add_enabled(!name_trimmed.is_empty(), egui::Button::new("Save"))
                .clicked()
            {
                save_requests.write(SaveRequested {
                    name: name_trimmed.to_string(),
                });
            }
        });

        ui.separator();
        ui.label("Existing saves:");

        let mut names: Vec<String> = std::fs::read_dir(SAVE_DIR)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("json") {
                    path.file_stem().and_then(|s| s.to_str()).map(str::to_string)
                } else {
                    None
                }
            })
            .collect();
        names.sort();

        if names.is_empty() {
            ui.label("(no saves yet)");
        }
        for name in names {
            ui.horizontal(|ui| {
                ui.label(&name);
                if ui.button("Load").clicked() {
                    load_requests.write(LoadRequested { name: name.clone() });
                }
                if ui.button("Delete").clicked() {
                    delete_requests.write(DeleteSaveRequested { name: name.clone() });
                }
            });
        }
    });
}

/// Field-level summary for the inspector (design doc §6.5): tile count,
/// planted tiles that are too dry or soggy by their own crop's ideal
/// range, and mm needed to reach field capacity across the field's tiles.
fn field_summary(
    ui: &mut egui::Ui,
    id: FieldId,
    registry: &FieldRegistry,
    index: &FieldIndex,
    tile_map: &TileMap,
    tiles: &Query<(&SoilState, Option<&PlantedCrop>)>,
    soils: &SoilRoster,
    crop_defs: &Assets<CropDef>,
) {
    let Some(info) = registry.get(id) else { return };
    let (mut count, mut planted, mut dry, mut soggy) = (0u32, 0u32, 0u32, 0u32);
    let (mut sum, mut min, mut max) = (0.0f32, f32::MAX, 0.0f32);
    for coord in index.tiles_of(id) {
        let Some(&entity) = tile_map.tiles.get(&coord) else { continue };
        let Ok((soil, crop)) = tiles.get(entity) else { continue };
        let def = soils.get(soil.soil as usize);
        count += 1;
        let need = (def.field_capacity_mm() - soil.water_mm).max(0.0);
        sum += need;
        min = min.min(need);
        max = max.max(need);
        if let Some(crop_def) = crop.filter(|c| !c.dead).and_then(|c| crop_defs.get(&c.def)) {
            planted += 1;
            dry += (def.available(soil.water_mm) < crop_def.ideal_available_water) as u32;
            soggy += (def.air(soil.water_mm) < crop_def.min_air_porosity) as u32;
        }
    }
    if count == 0 {
        ui.label(format!("Field '{}': no tiles", info.name));
        return;
    }
    ui.label(format!("Field '{}': {} tiles", info.name, count));
    ui.label(format!("  {} of {} planted tiles too dry", dry, planted));
    ui.label(format!("  {} of {} planted tiles waterlogged", soggy, planted));
    ui.label(format!(
        "  To field capacity: mean {:.1} mm (min {:.1}, max {:.1})",
        sum / count as f32,
        min,
        max
    ));
}

/// Message log window. Clicking a message selects its tile / targets its
/// field and moves the camera there.
pub fn messages_window(
    mut contexts: EguiContexts,
    mut open: ResMut<MessagesWindowOpen>,
    mut log: ResMut<MessageLog>,
    registry: Res<FieldRegistry>,
    mut selected: ResMut<SelectedTile>,
    mut target_field: ResMut<ActionTargetField>,
    mut camera: ResMut<CameraState>,
) {
    if !open.0 {
        return;
    }
    let mut window_open = true;
    let mut jump = None;
    egui::Window::new("Messages")
        .open(&mut window_open)
        .default_width(380.0)
        .show(contexts.ctx_mut(), |ui| {
            ui.horizontal(|ui| {
                if ui.button("Clear all").clicked() {
                    log.entries.clear();
                }
                ui.label(format!("{} messages", log.entries.len()));
            });
            ui.separator();
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                if log.entries.is_empty() {
                    ui.label("(no messages)");
                }
                for m in log.entries.iter().rev() {
                    let color = match m.severity {
                        Severity::Info => ui.visuals().text_color(),
                        Severity::Warning => egui::Color32::from_rgb(200, 120, 0),
                        Severity::Alert => egui::Color32::from_rgb(210, 40, 40),
                    };
                    let mut text = egui::RichText::new(format!("Day {}: {}", m.day, m.text)).color(color);
                    if m.unread {
                        text = text.strong();
                    }
                    if ui.selectable_label(false, text).clicked() {
                        jump = Some((m.tile, m.field.filter(|id| registry.get(*id).is_some())));
                    }
                }
            });
        });
    // Everything visible while the window is open counts as read.
    log.mark_all_read();
    if let Some((tile, field)) = jump {
        if let Some(coord) = tile {
            selected.0 = Some(coord);
            camera.center = tile_world_pos(coord);
        }
        target_field.0 = field;
    }
    open.0 = window_open;
}

pub fn options_window(
    mut contexts: EguiContexts,
    mut open: ResMut<OptionsWindowOpen>,
    mut theme: ResMut<UiTheme>,
    mut options: ResMut<GameOptions>,
    mut brush: ResMut<FieldBrush>,
) {
    if !open.0 {
        return;
    }
    let mut window_open = true;
    egui::Window::new("Options").open(&mut window_open).show(contexts.ctx_mut(), |ui| {
        ui.checkbox(&mut theme.dark, "Dark theme");
        ui.checkbox(&mut brush.show_boundaries, "Field outlines");
        ui.separator();
        ui.label("Auto-pause when:");
        ui.checkbox(&mut options.auto_pause_on_ready, "crops become ready to harvest");
        ui.checkbox(&mut options.auto_pause_on_death, "crops die");
    });
    open.0 = window_open;
}
