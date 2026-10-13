//! Fields (design doc §6.5; requirements §4.2a): named, persistent groups
//! of tiles that queued actions can target as a whole.
//!
//! The per-tile `TileField` component is the source of truth;
//! `FieldIndex` is a lookup kept in sync by `apply_paint` /
//! `delete_field` and rebuilt from the tile values on load, so resolving
//! a field never means scanning every tile.

use std::collections::{BTreeSet, HashMap};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::farm::{FarmConfig, TileMap};

/// Stable within a session and across saves; never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FieldId(pub u32);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldInfo {
    pub id: FieldId,
    pub name: String,
    pub color: [u8; 3],
}

/// Fields in creation order. Renaming edits only `name`, so saves and
/// queued actions (which hold `FieldId`s) never depend on a string that
/// can change. Name uniqueness is enforced by the UI, not here.
#[derive(Resource, Debug, Clone, Default, Serialize, Deserialize)]
pub struct FieldRegistry {
    pub fields: Vec<FieldInfo>,
    pub next_id: u32,
}

const PALETTE: [[u8; 3]; 8] = [
    [230, 25, 75],
    [60, 120, 230],
    [255, 225, 25],
    [60, 180, 75],
    [245, 130, 48],
    [145, 30, 180],
    [70, 240, 240],
    [240, 50, 230],
];

impl FieldRegistry {
    pub fn create(&mut self, name: String) -> FieldId {
        let id = FieldId(self.next_id);
        self.next_id += 1;
        self.fields.push(FieldInfo {
            id,
            name,
            color: PALETTE[id.0 as usize % PALETTE.len()],
        });
        id
    }

    pub fn get(&self, id: FieldId) -> Option<&FieldInfo> {
        self.fields.iter().find(|f| f.id == id)
    }

    pub fn name_in_use(&self, name: &str, except: Option<FieldId>) -> bool {
        self.fields.iter().any(|f| Some(f.id) != except && f.name == name)
    }

    /// First "Field N" not already taken.
    pub fn suggest_name(&self) -> String {
        (1..)
            .map(|n| format!("Field {n}"))
            .find(|n| !self.name_in_use(n, None))
            .unwrap()
    }

    /// Renames if the name is non-empty and unused; returns whether it did.
    pub fn rename(&mut self, id: FieldId, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() || self.name_in_use(name, Some(id)) {
            return false;
        }
        match self.fields.iter_mut().find(|f| f.id == id) {
            Some(f) => {
                f.name = name.to_string();
                true
            }
            None => false,
        }
    }
}

/// Which tiles (as `(x, y)`) belong to each field.
#[derive(Resource, Debug, Clone, Default)]
pub struct FieldIndex {
    pub tiles: HashMap<FieldId, BTreeSet<(i32, i32)>>,
}

impl FieldIndex {
    pub fn tiles_of(&self, id: FieldId) -> impl Iterator<Item = IVec2> + '_ {
        self.tiles
            .get(&id)
            .into_iter()
            .flat_map(|s| s.iter().map(|&(x, y)| IVec2::new(x, y)))
    }

    pub fn len_of(&self, id: FieldId) -> usize {
        self.tiles.get(&id).map_or(0, |s| s.len())
    }

    fn set(&mut self, coord: IVec2, old: Option<FieldId>, new: Option<FieldId>) {
        if let Some(old) = old {
            if let Some(set) = self.tiles.get_mut(&old) {
                set.remove(&(coord.x, coord.y));
            }
        }
        if let Some(new) = new {
            self.tiles.entry(new).or_default().insert((coord.x, coord.y));
        }
    }

    pub fn rebuild(&mut self, assignments: impl IntoIterator<Item = (IVec2, FieldId)>) {
        self.tiles.clear();
        for (coord, id) in assignments {
            self.set(coord, None, Some(id));
        }
    }
}

/// The field a tile is assigned to, if any.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TileField(pub Option<FieldId>);

/// Assign every tile in the inclusive rectangle to `field`, or unassign
/// them with `None`. Assigning removes a tile from its previous field.
#[derive(Event, Debug, Clone, Copy)]
pub struct PaintFieldRequested {
    pub a: IVec2,
    pub b: IVec2,
    pub field: Option<FieldId>,
}

/// Deletes a field; its tiles become unassigned.
#[derive(Event, Debug, Clone, Copy)]
pub struct DeleteFieldRequested(pub FieldId);

/// Brush / display state for the field tools. Not persisted.
#[derive(Resource, Debug, Clone)]
pub struct FieldBrush {
    pub window_open: bool,
    pub show_boundaries: bool,
    /// Paint mode: left-drag assigns a rectangle instead of selecting.
    pub paint: bool,
    /// `None` is the "Unassigned" eraser.
    pub active: Option<FieldId>,
    pub drag_start: Option<IVec2>,
}

impl Default for FieldBrush {
    fn default() -> Self {
        Self {
            window_open: false,
            show_boundaries: true,
            paint: false,
            active: None,
            drag_start: None,
        }
    }
}

/// The field queued actions currently target; `None` means the selected
/// tile. Set by the Target dropdown, and by clicking a tile (a tile in a
/// field targets that field, an unassigned tile targets itself).
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct ActionTargetField(pub Option<FieldId>);

pub struct FieldPlugin;

impl Plugin for FieldPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FieldRegistry>()
            .init_resource::<FieldIndex>()
            .init_resource::<FieldBrush>()
            .init_resource::<ActionTargetField>()
            .add_event::<PaintFieldRequested>()
            .add_event::<DeleteFieldRequested>()
            .add_systems(Update, (apply_paint, delete_field));
    }
}

fn apply_paint(
    mut events: EventReader<PaintFieldRequested>,
    registry: Res<FieldRegistry>,
    tile_map: Res<TileMap>,
    mut index: ResMut<FieldIndex>,
    mut tiles: Query<&mut TileField>,
) {
    for ev in events.read() {
        if let Some(id) = ev.field {
            if registry.get(id).is_none() {
                warn!("Paint requested for missing field {:?}, ignoring", id);
                continue;
            }
        }
        let (min, max) = (ev.a.min(ev.b), ev.a.max(ev.b));
        for y in min.y..=max.y {
            for x in min.x..=max.x {
                let coord = IVec2::new(x, y);
                let Some(&entity) = tile_map.tiles.get(&coord) else {
                    continue;
                };
                let Ok(mut tile_field) = tiles.get_mut(entity) else {
                    continue;
                };
                if tile_field.0 != ev.field {
                    index.set(coord, tile_field.0, ev.field);
                    tile_field.0 = ev.field;
                }
            }
        }
    }
}

fn delete_field(
    mut events: EventReader<DeleteFieldRequested>,
    mut registry: ResMut<FieldRegistry>,
    mut index: ResMut<FieldIndex>,
    mut brush: ResMut<FieldBrush>,
    tile_map: Res<TileMap>,
    mut tiles: Query<&mut TileField>,
) {
    for ev in events.read() {
        let id = ev.0;
        registry.fields.retain(|f| f.id != id);
        if let Some(set) = index.tiles.remove(&id) {
            for (x, y) in set {
                if let Some(&entity) = tile_map.tiles.get(&IVec2::new(x, y)) {
                    if let Ok(mut tf) = tiles.get_mut(entity) {
                        tf.0 = None;
                    }
                }
            }
        }
        if brush.active == Some(id) {
            brush.active = None;
        }
    }
}

/// Tiles visited in tractor order (design doc §6.5): rows numbered from
/// the top of the farm starting at 1, odd rows right to left and even rows
/// left to right. Parity comes from the farm-global row number, not the
/// row's position within the field. Tile y is Bevy's y-up, so the top row
/// is `height - 1`.
pub fn tractor_order(tiles: impl IntoIterator<Item = IVec2>, config: &FarmConfig) -> Vec<IVec2> {
    let height = config.height.clamp(FarmConfig::MIN_DIMENSION, FarmConfig::MAX_DIMENSION) as i32;
    let row = |c: &IVec2| height - c.y;
    let mut v: Vec<IVec2> = tiles.into_iter().collect();
    v.sort_by_key(|c| {
        let r = row(c);
        (r, if r % 2 == 1 { -c.x } else { c.x })
    });
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> FarmConfig {
        FarmConfig { width: 40, height: 40, ..default() }
    }

    #[test]
    fn tractor_order_alternates_by_global_row_from_the_top() {
        // Top row of a 40-high farm is y = 39 (row 1, odd -> right to left).
        let order = tractor_order(
            [
                IVec2::new(1, 39),
                IVec2::new(2, 39),
                IVec2::new(1, 38),
                IVec2::new(2, 38),
                IVec2::new(1, 37),
                IVec2::new(2, 37),
            ],
            &config(),
        );
        let xy: Vec<(i32, i32)> = order.iter().map(|c| (c.x, c.y)).collect();
        assert_eq!(xy, [(2, 39), (1, 39), (1, 38), (2, 38), (2, 37), (1, 37)]);
    }

    #[test]
    fn tractor_parity_ignores_what_is_in_the_field() {
        // The same row keeps its direction whether or not the rows above it
        // are in the field.
        let alone = tractor_order([IVec2::new(1, 38), IVec2::new(2, 38)], &config());
        assert_eq!((alone[0].x, alone[1].x), (1, 2));
        let with_above = tractor_order(
            [IVec2::new(1, 39), IVec2::new(1, 38), IVec2::new(2, 38)],
            &config(),
        );
        let row38: Vec<i32> = with_above.iter().filter(|c| c.y == 38).map(|c| c.x).collect();
        assert_eq!(row38, [1, 2]);
    }

    #[test]
    fn registry_names_are_unique_on_rename_and_ids_not_reused() {
        let mut r = FieldRegistry::default();
        let a = r.create("North".into());
        let b = r.create("South".into());
        assert!(!r.rename(b, "North"));
        assert!(r.rename(b, "South 2"));
        r.fields.retain(|f| f.id != b);
        let c = r.create("New".into());
        assert!(c != a && c != b);
        assert_eq!(r.suggest_name(), "Field 1");
    }
}
