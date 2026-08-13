//! egui panels (Phase 6) - the real UI replacing the dev keyboard
//! controls from Phases 4-5. Reads/writes the same resources those dev
//! controls did (`ActionQueue`, `Wallet`, `Inventory`, `TimeControl`),
//! so this is a UI-only addition, not a simulation-logic change.
//!
//! Requirements §4.6a: the action-queue panel is intentionally
//! FIFO-display-only - no reorder/cancel controls, even though
//! `PendingAction::id` already supports that data-side. See the
//! requirements doc's resolved §9 note for why that's deferred rather
//! than missing.

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};

use crate::crops::{ActionKind, ActionQueue, CropDef, NextActionId, PendingAction};
use crate::economy::{Inventory, SellRequested, Wallet};
use crate::farm::TileMap;
use crate::time::{Calendar, StepDayRequested, TimeControl, TimeMode, TimerSpeed};

use super::{SelectedTile, UiTheme};
use crate::farm::SoilState;
use crate::crops::PlantedCrop;

/// Starter roster (design doc §6/§4.3, Phase 4's four crops), paired
/// with their asset paths. Same not-data-driven caveat as
/// `render.rs::crop_initial` - this is UI wiring for a fixed MVP roster,
/// not a stand-in for a real crop-browsing UI (which would read the
/// roster from `Assets<CropDef>` or a manifest instead of a hardcoded
/// list like this).
const STARTER_CROPS: &[(&str, &str)] = &[
    ("Tomato", "crops/tomato.json"),
    ("Wheat", "crops/wheat.json"),
    ("Carrot", "crops/carrot.json"),
    ("Green Bean", "crops/green_bean.json"),
];

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

/// Top bar: day/season readout, time control, theme toggle. Applies the
/// current `UiTheme` to the egui context every frame (idempotent, cheap)
/// rather than only on toggle - simplest way to guarantee the visuals
/// stay correct regardless of what else might touch `egui::Context`
/// visuals later (e.g. a future settings panel).
pub fn top_bar(
    mut contexts: EguiContexts,
    mut theme: ResMut<UiTheme>,
    calendar: Res<Calendar>,
    mut control: ResMut<TimeControl>,
    mut step_requests: EventWriter<StepDayRequested>,
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
/// read-only). Reachable state matches what the dev-control console log
/// used to print, now attached to whichever tile the player actually
/// clicked (`render::tile_click_system`) instead of hardcoded (0,0).
pub fn tile_inspector_panel(
    mut contexts: EguiContexts,
    selected: Res<SelectedTile>,
    tile_map: Res<TileMap>,
    tiles: Query<(&SoilState, Option<&PlantedCrop>)>,
    crop_defs: Res<Assets<CropDef>>,
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

        ui.label(format!("Soil type: {:?}", soil.soil_type));
        ui.label(format!("Fertility: {:.2}", soil.fertility));
        ui.label(format!("Moisture: {:.2}", soil.moisture));
        ui.label(format!("Tilled: {}", soil.tilled));

        ui.separator();

        match planted {
            None => {
                ui.label("No crop planted.");
            }
            Some(crop) if crop.dead => {
                // Design doc §2.4: death must be visible - naming it
                // explicitly here, not just via the tile's red color.
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
                    ui.label(format!("Cumulative stress: {:.2}", crop.cumulative_stress));
                    ui.label(format!("Worst single-day stress: {:.2}", crop.worst_single_day_stress));
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
    let id = next_id.0;
    next_id.0 += 1;
    queue.pending.push_back(PendingAction {
        id,
        tile,
        kind,
        crop_to_plant,
    });
}

/// Action queue panel: queue buttons for the selected tile, a crop
/// picker for Plant, and a FIFO-display-only list of
/// `ActionQueue.pending` (requirements §4.6a/§9 - reorder/cancel
/// deferred, not built here even though the data supports it).
pub fn action_queue_panel(
    mut contexts: EguiContexts,
    selected: Res<SelectedTile>,
    tile_map: Res<TileMap>,
    asset_server: Res<AssetServer>,
    mut queue: ResMut<ActionQueue>,
    mut next_id: ResMut<NextActionId>,
    mut crop_choice: Local<usize>,
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
            if ui.button("Water").clicked() {
                if let Some(t) = tile {
                    enqueue(&mut queue, &mut next_id, t, ActionKind::Water, None);
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
            egui::ComboBox::from_label("Crop")
                .selected_text(STARTER_CROPS[*crop_choice].0)
                .show_ui(ui, |ui| {
                    for (i, (name, _path)) in STARTER_CROPS.iter().enumerate() {
                        ui.selectable_value(&mut *crop_choice, i, *name);
                    }
                });
            if ui.button("Plant").clicked() {
                if let Some(t) = tile {
                    let handle: Handle<CropDef> = asset_server.load(STARTER_CROPS[*crop_choice].1);
                    enqueue(&mut queue, &mut next_id, t, ActionKind::Plant, Some(handle));
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
                ui.label(format!("#{} - {:?}", action.id, action.kind));
            }
        });
    });
}

/// Economy panel: wallet balance, aggregated inventory per crop id, and
/// a per-crop Sell button firing `SellRequested` (design doc §2.5/Phase
/// 5's resolved instant-not-queued design). Aggregates lot quantities
/// for display only - selling still drains every lot for that crop id
/// at once, matching Phase 5's existing `process_sell_requests`
/// behavior; this panel doesn't add per-lot/per-quality selection
/// (requirements §9, deferred).
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
