//! Save/load (design doc §2.8; requirements §4.6) - **Phase 7.**
//!
//! JSON, matching the format already used for crop data (requirements
//! §3's "one format everywhere" rationale). Rather than trying to
//! derive `Serialize`/`Deserialize` on live ECS state directly, this
//! module builds an explicit `SaveGame` snapshot struct that systems
//! populate from/apply to the world - the same approach the design doc
//! sketched in §2.8, extended here to also cover `Inventory`,
//! `FarmConfig`, `Market`, `UiTheme`, and (per your call to persist it)
//! `ActionQueue`, none of which were in that original sketch.
//!
//! **Why `PlantedCrop`/`ActionQueue` need mirror structs but most other
//! resources don't:** `PlantedCrop.def` is a `Handle<CropDef>` and
//! `PendingAction.tile` is an `Entity` - neither survives a save/reload,
//! since asset handles are asset-server-internal and entity IDs aren't
//! stable across runs. Every other persisted resource (`Wallet`,
//! `Inventory`, `Calendar`, `Equipment`, `Market`, `UiTheme`,
//! `FarmConfig`, `SoilState`) is plain data with no such references, so
//! those derive `Serialize`/`Deserialize` directly and are embedded in
//! `SaveGame` as-is - see each type's own doc comment (in its home
//! module) for the Phase 7 derive note.
//!
//! **`ActionQueue` persistence (your call, not the original plan):**
//! saving mid-plan and reloading resumes exactly where you left off,
//! including anything still queued. The one edge case worth knowing:
//! if a save references a crop id whose JSON has since been deleted (or
//! a farm size the save's tiles don't fit, which shouldn't happen since
//! the farm is rebuilt at the saved size, but is guarded anyway), that
//! one tile or queued action is dropped with a `warn!` and the rest of
//! the load proceeds normally - a corrupted reference doesn't fail the
//! whole load.
//!
//! **Phase 8 addition:** `SavedAction` gained `water_intensity`
//! (mirroring the same field on `PendingAction`, added for the new "½
//! Water" button - see `crops::queue::PendingAction`'s doc comment).
//! It's `#[serde(default)]`'d so a save file written before this field
//! existed still loads cleanly, with every action defaulting to full
//! (`1.0`) intensity rather than failing to parse.

use std::fs;
use std::path::PathBuf;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::crops::{ActionKind, ActionQueue, CropDef, CropRoster, Equipment, NextActionId, PendingAction, PlantedCrop, TileOrField};
use crate::economy::{Inventory, Market, Wallet};
use crate::farm::{self, FarmConfig, SoilState, Tile, TileMap};
use crate::field::{FieldId, FieldIndex, FieldRegistry, TileField};
use crate::soil::SoilRoster;
use crate::time::{Calendar, TimeControl, TimeMode};
use crate::ui::UiTheme;

/// Save files live under `<repo root>/save/`, one file per save
/// (`save/<name>.json`) - a plain subdirectory rather than the repo
/// root itself specifically so it's easy to `.gitignore` as a whole
/// without risking an unrelated top-level file getting swept up in the
/// same ignore rule. `pub` so `ui::panels::save_load_panel` can scan it
/// for the save-list UI without duplicating the path.
pub const SAVE_DIR: &str = "save";

/// Schema version, embedded in every save file but not yet read by
/// anything - there's only one schema so far. Exists so a future format
/// change has somewhere to branch on ("if version < 2, migrate field X")
/// instead of needing to guess a save file's age from its shape.
const SAVE_VERSION: u32 = 1;

#[derive(Event, Debug, Clone)]
pub struct SaveRequested {
    pub name: String,
}

#[derive(Event, Debug, Clone)]
pub struct LoadRequested {
    pub name: String,
}

#[derive(Event, Debug, Clone)]
pub struct DeleteSaveRequested {
    pub name: String,
}

/// `Equipment::daily_capacity` is a `HashMap<ActionKind, u32>`. Rather
/// than deriving `Serialize` on `Equipment` directly and relying on
/// serde_json's enum-as-map-key support (uncertain enough, and untested
/// here since `cargo check` isn't run in this workflow, that it's not
/// worth risking), this stores the same data as a plain `Vec` of pairs -
/// always valid JSON regardless of how map-key serialization behaves
/// for a given serde_json version.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SavedEquipment(Vec<(ActionKind, u32)>);

impl From<&Equipment> for SavedEquipment {
    fn from(eq: &Equipment) -> Self {
        SavedEquipment(eq.daily_capacity.iter().map(|(k, v)| (*k, *v)).collect())
    }
}

impl From<SavedEquipment> for Equipment {
    fn from(saved: SavedEquipment) -> Self {
        Equipment {
            daily_capacity: saved.0.into_iter().collect(),
        }
    }
}

/// Mirror of `PlantedCrop`, substituting a plain crop-id `String` for
/// the live `Handle<CropDef>` - re-resolved back to a handle on load via
/// `CropRoster` (see `resolve_crop_handle` below), the same pattern
/// `Inventory`/`ActionQueue`'s `crop_to_plant` already use for the same
/// reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SavedCrop {
    crop_id: String,
    planted_day: u32,
    current_stage: usize,
    days_in_stage: u32,
    cumulative_stress: f32,
    worst_single_day_stress: f32,
    dead: bool,
}

/// Mirror of `SoilState`: the soil is stored by string id (not the
/// roster index, which depends on which soil files exist) and the water as
/// a depth in mm. The `Option` fields let saves from before the soil
/// definitions / water model (`soil_type` + 0-1 `moisture`) still load -
/// see `SavedSoil::to_state`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SavedSoil {
    fertility: f32,
    tilled: bool,
    #[serde(default)]
    soil_id: Option<String>,
    #[serde(default)]
    water_mm: Option<f32>,
    /// Legacy: `"Sandy"` / `"Loamy"` / `"Clay"`.
    #[serde(default, skip_serializing)]
    soil_type: Option<String>,
    /// Legacy: 0.0-1.0.
    #[serde(default, skip_serializing)]
    moisture: Option<f32>,
}

impl SavedSoil {
    fn from_state(soil: &SoilState, soils: &SoilRoster) -> Self {
        Self {
            fertility: soil.fertility,
            tilled: soil.tilled,
            soil_id: Some(soils.get(soil.soil as usize).id.clone()),
            water_mm: Some(soil.water_mm),
            soil_type: None,
            moisture: None,
        }
    }

    /// `fallback_index` is the freshly generated soil for this tile, used
    /// if the saved id no longer exists in the roster.
    fn to_state(&self, soils: &SoilRoster, fallback_index: u8) -> SoilState {
        let id = self.soil_id.as_ref().or(self.soil_type.as_ref());
        let index = id
            .and_then(|id| soils.index_of(id))
            .map(|i| i as u8)
            .unwrap_or_else(|| {
                if let Some(id) = id {
                    warn!("Save references soil '{}', which no longer exists - using generated soil", id);
                }
                fallback_index
            });
        let def = soils.get(index as usize);
        let water_mm = match (self.water_mm, self.moisture) {
            (Some(mm), _) => mm,
            // Old 0-1 moisture: map linearly from wilting point to saturation.
            (None, Some(m)) => def.wilting_mm() + m.clamp(0.0, 1.0) * (def.saturation_mm() - def.wilting_mm()),
            (None, None) => def.field_capacity_mm(),
        };
        SoilState {
            fertility: self.fertility,
            water_mm: water_mm.clamp(def.wilting_mm(), def.saturation_mm()),
            soil: index,
            tilled: self.tilled,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SavedTile {
    coord: IVec2,
    soil: SavedSoil,
    planted_crop: Option<SavedCrop>,
    /// Field assignment (`FieldId`); absent in older saves.
    #[serde(default)]
    field: Option<u32>,
}

/// Depth the legacy `water_intensity` of 1.0 maps to (0.5 maps to half).
const LEGACY_FULL_WATERING_MM: f32 = 20.0;

/// Mirror of `PendingAction`, substituting the tile's farm coordinate
/// for the live `Entity` (re-resolved via the freshly-rebuilt `TileMap`
/// on load) and a crop-id `String` for `Handle<CropDef>`, same reasoning
/// as `SavedCrop` above.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SavedAction {
    id: u64,
    /// The target is a single tile (this) or a field (`field`).
    #[serde(default)]
    tile_coord: Option<IVec2>,
    #[serde(default)]
    field: Option<u32>,
    /// Tiles already visited, and the applied/skipped counts.
    #[serde(default)]
    visited: Vec<IVec2>,
    #[serde(default)]
    applied: u32,
    #[serde(default)]
    skipped: u32,
    kind: ActionKind,
    crop_to_plant: Option<String>,
    /// Mirrors `PendingAction::water_depth_mm`. Missing in older saves,
    /// which stored `water_intensity` instead.
    #[serde(default)]
    water_depth_mm: Option<f32>,
    /// Legacy: 1.0 = full, 0.5 = half watering.
    #[serde(default, skip_serializing)]
    water_intensity: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SaveGame {
    version: u32,
    calendar: Calendar,
    wallet: Wallet,
    equipment: SavedEquipment,
    market: Market,
    ui_theme: UiTheme,
    farm_config: FarmConfig,
    inventory: Inventory,
    #[serde(default)]
    fields: FieldRegistry,
    tiles: Vec<SavedTile>,
    action_queue: Vec<SavedAction>,
    next_action_id: u64,
}

/// Rejects anything that isn't a simple name - no path separators, no
/// `..`, not empty - before it's used to build a filesystem path. This
/// is the one place user-typed text (the save-name field in
/// `ui::panels::save_load_panel`) reaches the filesystem, so it's worth
/// being strict here even though this is a single-player local tool,
/// not a security boundary in the adversarial sense.
fn sanitized_path(name: &str) -> Option<PathBuf> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let valid = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == ' ');
    if !valid {
        return None;
    }
    Some(PathBuf::from(SAVE_DIR).join(format!("{name}.json")))
}

pub struct SavePlugin;

impl Plugin for SavePlugin {
    fn build(&self, app: &mut App) {
        app.add_event::<SaveRequested>()
            .add_event::<LoadRequested>()
            .add_event::<DeleteSaveRequested>()
            .add_systems(Update, (save_game, load_game, delete_save));
    }
}

/// Reacts to `SaveRequested`. Builds the full `SaveGame` snapshot from
/// current world/resource state and writes it to `save/<name>.json`,
/// creating the `save/` directory first if it doesn't exist yet.
fn save_game(
    mut events: EventReader<SaveRequested>,
    calendar: Res<Calendar>,
    wallet: Res<Wallet>,
    equipment: Res<Equipment>,
    market: Res<Market>,
    theme: Res<UiTheme>,
    config: Res<FarmConfig>,
    inventory: Res<Inventory>,
    queue: Res<ActionQueue>,
    next_id: Res<NextActionId>,
    tiles: Query<(&Tile, &SoilState, Option<&PlantedCrop>, &TileField)>,
    tile_lookup: Query<&Tile>,
    fields: Res<FieldRegistry>,
    crop_defs: Res<Assets<CropDef>>,
    soils: Res<SoilRoster>,
) {
    for ev in events.read() {
        let Some(path) = sanitized_path(&ev.name) else {
            warn!("Refusing to save: '{}' isn't a valid save name", ev.name);
            continue;
        };

        let saved_tiles: Vec<SavedTile> = tiles
            .iter()
            .map(|(tile, soil, planted, tile_field)| {
                let planted_crop = planted.and_then(|crop| {
                    crop_defs.get(&crop.def).map(|def| SavedCrop {
                        crop_id: def.id.clone(),
                        planted_day: crop.planted_day,
                        current_stage: crop.current_stage,
                        days_in_stage: crop.days_in_stage,
                        cumulative_stress: crop.cumulative_stress,
                        worst_single_day_stress: crop.worst_single_day_stress,
                        dead: crop.dead,
                    })
                });
                SavedTile {
                    coord: tile.coord,
                    soil: SavedSoil::from_state(soil, &soils),
                    planted_crop,
                    field: tile_field.0.map(|f| f.0),
                }
            })
            .collect();

        let saved_actions: Vec<SavedAction> = queue
            .pending
            .iter()
            .filter_map(|action| {
                let (tile_coord, field) = match action.target {
                    TileOrField::Tile(entity) => {
                        let Ok(tile) = tile_lookup.get(entity) else {
                            warn!(
                                "Dropping queued action #{} from save: its tile entity no longer exists",
                                action.id
                            );
                            return None;
                        };
                        (Some(tile.coord), None)
                    }
                    TileOrField::Field(id) => (None, Some(id.0)),
                };
                let mut visited: Vec<IVec2> = action.visited.iter().copied().collect();
                visited.sort_by_key(|c| (c.y, c.x));
                let crop_to_plant = action
                    .crop_to_plant
                    .as_ref()
                    .and_then(|h| crop_defs.get(h))
                    .map(|def| def.id.clone());
                Some(SavedAction {
                    id: action.id,
                    tile_coord,
                    field,
                    visited,
                    applied: action.applied,
                    skipped: action.skipped,
                    kind: action.kind,
                    crop_to_plant,
                    water_depth_mm: Some(action.water_depth_mm),
                    water_intensity: None,
                })
            })
            .collect();

        let save = SaveGame {
            version: SAVE_VERSION,
            calendar: calendar.clone(),
            wallet: wallet.clone(),
            equipment: SavedEquipment::from(&*equipment),
            market: market.clone(),
            ui_theme: *theme,
            farm_config: config.clone(),
            inventory: inventory.clone(),
            fields: fields.clone(),
            tiles: saved_tiles,
            action_queue: saved_actions,
            next_action_id: next_id.0,
        };

        let json = match serde_json::to_string_pretty(&save) {
            Ok(j) => j,
            Err(e) => {
                error!("Failed to serialize save '{}': {}", ev.name, e);
                continue;
            }
        };

        if let Err(e) = fs::create_dir_all(SAVE_DIR) {
            error!("Could not create '{}' directory: {}", SAVE_DIR, e);
            continue;
        }
        match fs::write(&path, json) {
            Ok(()) => info!("Saved game to {:?}", path),
            Err(e) => error!("Failed to write save file {:?}: {}", path, e),
        }
    }
}

/// Looks up a `Handle<CropDef>` for a saved crop-id string via
/// `CropRoster`. Tries `file_stem` first (the roster's primary key,
/// matching what `save_game` writes since it reads `CropDef.id` -
/// which `roster::validate_crop_defs` already warns about if it ever
/// diverges from the filename) and falls back to matching against the
/// loaded `CropDef.id` itself, in case that divergence has actually
/// happened for some crop.
fn resolve_crop_handle(id: &str, roster: &CropRoster, crop_defs: &Assets<CropDef>) -> Option<Handle<CropDef>> {
    roster
        .entries
        .iter()
        .find(|e| e.file_stem == id)
        .or_else(|| {
            roster
                .entries
                .iter()
                .find(|e| crop_defs.get(&e.handle).map(|d| d.id.as_str()) == Some(id))
        })
        .map(|e| e.handle.clone())
}

/// Reacts to `LoadRequested`. Reads and parses `save/<name>.json`; on
/// any error (missing file, corrupt JSON), logs it and leaves the
/// current game state untouched rather than partially applying a broken
/// load. On success: despawns the current farm, rebuilds it at the
/// save's `FarmConfig` size, restores every tile's soil/crop, rebuilds
/// `ActionQueue`, and overwrites every other persisted resource.
fn load_game(
    mut events: EventReader<LoadRequested>,
    mut commands: Commands,
    mut calendar: ResMut<Calendar>,
    mut wallet: ResMut<Wallet>,
    mut equipment: ResMut<Equipment>,
    mut market: ResMut<Market>,
    mut theme: ResMut<UiTheme>,
    mut config: ResMut<FarmConfig>,
    mut inventory: ResMut<Inventory>,
    mut queue: ResMut<ActionQueue>,
    mut next_id: ResMut<NextActionId>,
    mut control: ResMut<TimeControl>,
    mut tile_map: ResMut<TileMap>,
    existing_tiles: Query<Entity, With<Tile>>,
    // Grouped: a system function takes at most 16 parameters.
    (roster, crop_defs, soils): (Res<CropRoster>, Res<Assets<CropDef>>, Res<SoilRoster>),
    (mut field_registry, mut field_index): (ResMut<FieldRegistry>, ResMut<FieldIndex>),
) {
    for ev in events.read() {
        let Some(path) = sanitized_path(&ev.name) else {
            warn!("Refusing to load: '{}' isn't a valid save name", ev.name);
            continue;
        };

        let contents = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                error!("Could not read save file {:?}: {}", path, e);
                continue;
            }
        };
        let save: SaveGame = match serde_json::from_str(&contents) {
            Ok(s) => s,
            Err(e) => {
                error!("Save file {:?} is corrupt or from an incompatible version: {}", path, e);
                continue;
            }
        };

        // 1. Wipe the current farm.
        farm::despawn_all_tiles(&mut commands, &mut tile_map, &existing_tiles);

        // 2. Rebuild at the saved size.
        *config = save.farm_config.clone();
        farm::spawn_tile_grid(&mut commands, &config, &mut tile_map, &soils);

        // Fields: restore the registry, then (below) each tile's
        // assignment; the index is rebuilt from the tile values.
        *field_registry = save.fields.clone();
        let mut assignments: Vec<(IVec2, FieldId)> = Vec::new();

        // 3. Overwrite each tile's soil, and re-plant if the save had a
        // live crop there.
        for saved_tile in &save.tiles {
            let Some(&entity) = tile_map.tiles.get(&saved_tile.coord) else {
                warn!(
                    "Save references tile {:?}, outside the rebuilt {}x{} farm - skipping",
                    saved_tile.coord, config.width, config.height
                );
                continue;
            };
            let fallback = saved_tile.soil.to_state(&soils, 0);
            commands.entity(entity).insert(fallback);

            if let Some(id) = saved_tile.field.map(FieldId).filter(|id| field_registry.get(*id).is_some()) {
                commands.entity(entity).insert(TileField(Some(id)));
                assignments.push((saved_tile.coord, id));
            }

            if let Some(saved_crop) = &saved_tile.planted_crop {
                match resolve_crop_handle(&saved_crop.crop_id, &roster, &crop_defs) {
                    Some(handle) => {
                        commands.entity(entity).insert(PlantedCrop {
                            def: handle,
                            planted_day: saved_crop.planted_day,
                            current_stage: saved_crop.current_stage,
                            days_in_stage: saved_crop.days_in_stage,
                            cumulative_stress: saved_crop.cumulative_stress,
                            worst_single_day_stress: saved_crop.worst_single_day_stress,
                            dead: saved_crop.dead,
                        });
                    }
                    None => {
                        warn!(
                            "Save references crop '{}', which no longer exists - tile {:?} loads as bare soil",
                            saved_crop.crop_id, saved_tile.coord
                        );
                    }
                }
            }
        }

        field_index.rebuild(assignments);

        // 4. Rebuild the action queue against the new TileMap/handles.
        queue.pending.clear();
        for saved_action in &save.action_queue {
            let target = match (saved_action.tile_coord, saved_action.field) {
                (Some(coord), _) => match tile_map.tiles.get(&coord) {
                    Some(&entity) => TileOrField::Tile(entity),
                    None => {
                        warn!(
                            "Dropping queued action #{} on load: tile {:?} isn't in the rebuilt farm",
                            saved_action.id, coord
                        );
                        continue;
                    }
                },
                (None, Some(id)) => TileOrField::Field(FieldId(id)),
                (None, None) => {
                    warn!("Dropping queued action #{} on load: it has no target", saved_action.id);
                    continue;
                }
            };
            let crop_to_plant = match &saved_action.crop_to_plant {
                Some(id) => match resolve_crop_handle(id, &roster, &crop_defs) {
                    Some(h) => Some(h),
                    None => {
                        warn!(
                            "Dropping queued action #{}: crop '{}' no longer exists",
                            saved_action.id, id
                        );
                        continue;
                    }
                },
                None => None,
            };
            let mut action = PendingAction::new(
                saved_action.id,
                target,
                saved_action.kind,
                crop_to_plant,
                saved_action
                    .water_depth_mm
                    .or(saved_action.water_intensity.map(|i| i * LEGACY_FULL_WATERING_MM))
                    .unwrap_or(LEGACY_FULL_WATERING_MM),
            );
            action.visited = saved_action.visited.iter().copied().collect();
            action.applied = saved_action.applied;
            action.skipped = saved_action.skipped;
            queue.pending.push_back(action);
        }
        next_id.0 = save.next_action_id;

        // 5. Everything else is a straight overwrite.
        *calendar = save.calendar.clone();
        *wallet = save.wallet.clone();
        *equipment = Equipment::from(save.equipment.clone());
        *market = save.market.clone();
        *theme = save.ui_theme;
        *inventory = save.inventory.clone();

        // Time resumes Paused regardless of what it was when saved -
        // TimeControl/TimeMode aren't persisted at all (see this
        // module's doc comment), and defaulting to Paused is the safe
        // choice: a Timer resuming mid-countdown right after load could
        // tick days forward before the player's even looked at the
        // screen.
        control.mode = TimeMode::Paused;

        info!("Loaded game from {:?}", path);
    }
}

/// Reacts to `DeleteSaveRequested` - just removes the file. No
/// confirmation dialog for MVP (matches the project's general "keep the
/// UI simple" bar so far); worth adding if an accidental-delete report
/// ever comes in from actual play.
fn delete_save(mut events: EventReader<DeleteSaveRequested>) {
    for ev in events.read() {
        let Some(path) = sanitized_path(&ev.name) else {
            warn!("Refusing to delete: '{}' isn't a valid save name", ev.name);
            continue;
        };
        match fs::remove_file(&path) {
            Ok(()) => info!("Deleted save {:?}", path),
            Err(e) => error!("Failed to delete save {:?}: {}", path, e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_soil_and_watering_fields_still_load() {
        let soil: SavedSoil =
            serde_json::from_str(r#"{"fertility":0.5,"moisture":0.5,"soil_type":"Clay","tilled":true}"#).unwrap();
        let roster = SoilRoster::load_from_dir(crate::soil::SOILS_DIR);
        let state = soil.to_state(&roster, 0);
        let clay = roster.get(roster.index_of("clay").unwrap());
        assert_eq!(roster.get(state.soil as usize).id, "clay");
        assert!(state.water_mm > clay.wilting_mm() && state.water_mm < clay.saturation_mm());
        assert!(state.tilled);

        let action: SavedAction =
            serde_json::from_str(r#"{"id":1,"tile_coord":[0,0],"kind":"Water","crop_to_plant":null,"water_intensity":0.5}"#)
                .unwrap();
        assert_eq!(action.water_intensity.unwrap() * LEGACY_FULL_WATERING_MM, 10.0);
    }
}
