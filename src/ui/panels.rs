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
    NextActionId, PendingAction, RangeStatus,
};
use crate::economy::{Inventory, SellRequested, Wallet};
use crate::farm::{FarmConfig, TileMap};
use crate::save::{DeleteSaveRequested, LoadRequested, SaveRequested, SAVE_DIR};
use crate::time::{Calendar, StepDayRequested, TimeControl, TimeMode, TimerSpeed};

use super::render::{farm_world_center, BUTTON_ZOOM_STEP, MAX_ZOOM_FACTOR, MIN_ZOOM_FACTOR};
use super::{CameraState, SaveLoadWindowOpen, SelectedTile, ShowSoilOverlay, UiTheme};
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
    mut theme: ResMut<UiTheme>,
    calendar: Res<Calendar>,
    weather: Res<WeatherToday>,
    mut control: ResMut<TimeControl>,
    mut step_requests: EventWriter<StepDayRequested>,
    mut camera_state: ResMut<CameraState>,
    config: Res<FarmConfig>,
    mut save_load_open: ResMut<SaveLoadWindowOpen>,
    mut soil_overlay: ResMut<ShowSoilOverlay>,
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

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if theme.dark { "Light theme" } else { "Dark theme" };
                if ui.button(label).clicked() {
                    theme.dark = !theme.dark;
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

        ui.separator();

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
    tile: Entity,
    kind: ActionKind,
    crop_to_plant: Option<Handle<CropDef>>,
) {
    enqueue_with_depth(queue, next_id, tile, kind, crop_to_plant, 0.0);
}

/// Like `enqueue`, but with a water depth in mm (only meaningful for
/// `ActionKind::Water` - see `PendingAction::water_depth_mm`).
fn enqueue_with_depth(
    queue: &mut ActionQueue,
    next_id: &mut NextActionId,
    tile: Entity,
    kind: ActionKind,
    crop_to_plant: Option<Handle<CropDef>>,
    water_depth_mm: f32,
) {
    let id = next_id.0;
    next_id.0 += 1;
    queue.pending.push_back(PendingAction {
        id,
        tile,
        kind,
        crop_to_plant,
        water_depth_mm,
    });
}

pub fn action_queue_panel(
    mut contexts: EguiContexts,
    selected: Res<SelectedTile>,
    tile_map: Res<TileMap>,
    roster: Res<CropRoster>,
    crop_defs: Res<Assets<CropDef>>,
    mut queue: ResMut<ActionQueue>,
    mut next_id: ResMut<NextActionId>,
    mut crop_choice: Local<usize>,
    mut water_depth: Local<Option<f32>>,
) {
    egui::SidePanel::left("action_queue").show(contexts.ctx_mut(), |ui| {
        ui.heading("Actions");

        let tile = selected.0.and_then(|coord| tile_map.tiles.get(&coord).copied());
        if tile.is_none() {
            ui.label("Select a tile to queue actions on it.");
        }

        ui.add_enabled_ui(tile.is_some(), |ui| {
            if ui.button("Till").clicked() {
                if let Some(t) = tile {
                    enqueue(&mut queue, &mut next_id, t, ActionKind::Till, None);
                }
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
                if let Some(t) = tile {
                    enqueue_with_depth(&mut queue, &mut next_id, t, ActionKind::Water, None, *depth);
                }
            }
            if ui.button("Fertilize").clicked() {
                if let Some(t) = tile {
                    enqueue(&mut queue, &mut next_id, t, ActionKind::Fertilize, None);
                }
            }
            if ui.button("Harvest").clicked() {
                if let Some(t) = tile {
                    enqueue(&mut queue, &mut next_id, t, ActionKind::Harvest, None);
                }
            }
            if ui.button("Clear").clicked() {
                if let Some(t) = tile {
                    enqueue(&mut queue, &mut next_id, t, ActionKind::Clear, None);
                }
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
                    if let Some(t) = tile {
                        let handle = roster.entries[*crop_choice].handle.clone();
                        enqueue(&mut queue, &mut next_id, t, ActionKind::Plant, Some(handle));
                    }
                }
            }
        });

        ui.separator();
        ui.heading("Pending Queue (FIFO)");
        egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
            if queue.pending.is_empty() {
                ui.label("(empty)");
            }
            for action in &queue.pending {
                if action.kind == ActionKind::Water {
                    ui.label(format!("#{} - Water ({:.0} mm)", action.id, action.water_depth_mm));
                } else {
                    ui.label(format!("#{} - {:?}", action.id, action.kind));
                }
            }
        });
    });
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
