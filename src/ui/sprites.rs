//! Sprite asset loading for soil textures and crop growth-stage art
//! (requirements §10; design doc's "growth-stage art stays fully
//! data-driven per crop" / "soil visuals are layered, not flat").
//!
//! **Soil:** one base texture per `SoilType`, loaded once at Startup
//! from `assets/soil/<type>.png`, plus a single tilled-overlay texture
//! (`assets/soil/tilled_overlay.png`) drawn as a separate sprite layer
//! on top when a tile is tilled. Moisture/fertility are NOT separate
//! images — they're a runtime color tint applied to the soil sprite
//! (see `soil_tint`), matching the "tints/overlays, not flat
//! per-combination textures" decision already locked into the
//! requirements doc.
//!
//! **Crops:** growth-stage art is discovered per crop from its own
//! `CropDef.growth_stages` names (plus a fixed `"dead"` state) — NOT a
//! hardcoded stage count, so a crop with 5 stages just needs 5 image
//! files dropped in, no code change. Path convention:
//!   `assets/crops/<crop_id>/<stage_name>.png`
//!   `assets/crops/<crop_id>/dead.png`
//! `<crop_id>` is `CropRosterEntry::file_stem` — the folder name under
//! `assets/crops/`, same convention `roster.rs` already uses for the
//! JSON files themselves.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::crops::{CropDef, CropRoster};
use crate::soil::SoilRoster;

/// One `Handle<Image>` per soil in the `SoilRoster` (same indexing, each
/// from its `SoilDef::sprite` path), plus the tilled-furrow overlay.
#[derive(Resource, Debug, Clone)]
pub struct SoilSpriteRoster {
    pub by_index: Vec<Handle<Image>>,
    pub tilled_overlay: Handle<Image>,
}

/// Per-crop, per-stage-name sprite handles, keyed by `CropDef.id`.
/// `"dead"` is a synthetic key alongside the real growth-stage names,
/// backing the dead-crop art.
#[derive(Resource, Debug, Clone, Default)]
pub struct CropSpriteRoster {
    pub sprites: HashMap<String, HashMap<String, Handle<Image>>>,
}

impl CropSpriteRoster {
    pub fn get(&self, crop_id: &str, stage_or_dead: &str) -> Option<&Handle<Image>> {
        self.sprites.get(crop_id)?.get(stage_or_dead)
    }
}

/// Startup: queue the fixed soil/overlay image loads. Order relative to
/// `farm::spawn_tiles` (also Startup, different plugin) doesn't matter —
/// `render::spawn_tile_visuals` that reads `SoilSpriteRoster` is
/// `Update`-scheduled, and all `Startup` systems finish before the
/// first `Update` pass runs.
pub fn load_soil_sprites(asset_server: Res<AssetServer>, soils: Res<SoilRoster>, mut commands: Commands) {
    let by_index = soils.defs.iter().map(|d| asset_server.load(d.sprite.clone())).collect();
    let tilled_overlay = asset_server.load("soil/tilled_overlay.png");
    commands.insert_resource(SoilSpriteRoster { by_index, tilled_overlay });
}

/// Reacts to a `CropDef` asset finishing load (same event pattern
/// `roster::validate_crop_defs` already uses) and queues an image load
/// for every growth-stage name it declares, plus `"dead"`.
///
/// Cheap to run every frame: `HashMap::entry(..).or_insert_with(..)`
/// means a crop whose sprites are already queued is a no-op on any
/// later event for the same asset (e.g. a hot-reload).
pub fn load_crop_sprites(
    mut events: EventReader<AssetEvent<CropDef>>,
    crop_defs: Res<Assets<CropDef>>,
    roster: Res<CropRoster>,
    asset_server: Res<AssetServer>,
    mut sprite_roster: ResMut<CropSpriteRoster>,
) {
    for event in events.read() {
        let (AssetEvent::LoadedWithDependencies { id } | AssetEvent::Added { id }) = event else {
            continue;
        };
        let Some(def) = crop_defs.get(*id) else {
            continue;
        };
        // Filesystem path is always built from the roster's file_stem
        // (the folder name), not def.id directly - the two are
        // expected to match (roster.rs already warns if they diverge),
        // but this is the one place that assumption would actually
        // bite if it were ever violated.
        let Some(entry) = roster.entries.iter().find(|e| &e.handle.id() == id) else {
            continue;
        };
        let folder = &entry.file_stem;

        let stage_map = sprite_roster.sprites.entry(def.id.clone()).or_default();
        for stage in &def.growth_stages {
            stage_map
                .entry(stage.name.clone())
                .or_insert_with(|| asset_server.load(format!("crops/{}/{}.png", folder, stage.name)));
        }
        stage_map
            .entry("dead".to_string())
            .or_insert_with(|| asset_server.load(format!("crops/{}/dead.png", folder)));
    }
}

/// Runtime moisture/fertility tint applied to a soil sprite (requirements
/// §10: "moisture/fertility as tints ... rather than flat images per
/// combination"). Low fertility washes the base texture toward gray
/// (a barren look); higher moisture darkens and cools it slightly (a
/// "wet" look) — without needing a separate "watered <soil type>" image
/// per soil type.
///
/// Both effects are deliberately mild (see the lerp factors below) so
/// the underlying `SoilType` texture stays recognizable rather than
/// tinting into mush - a §5-style placeholder like the sim's other
/// unvalidated tuning constants, worth revisiting once actually seen
/// in-game.
pub fn soil_tint(fertility: f32, available_water: f32) -> Color {
    let fert = fertility.clamp(0.0, 1.0);
    let moist = available_water.clamp(0.0, 1.0);

    let gray_amount = (1.0 - fert) * 0.45;
    let wet_amount = moist * 0.25;

    let r = 1.0 - gray_amount * 0.35 - wet_amount * 0.30;
    let g = 1.0 - gray_amount * 0.35 - wet_amount * 0.15;
    let b = 1.0 - gray_amount * 0.35 + wet_amount * 0.25;

    Color::srgb(r.clamp(0.3, 1.0), g.clamp(0.3, 1.0), b.clamp(0.3, 1.0))
}

/// Flat colour for the soil-type overlay (design doc §6.7), derived from a
/// soil's `texture` value (coarse yellow -> fine red) so a new soil type
/// needs no code change.
pub fn soil_overlay_color(texture: f32) -> Color {
    let t = texture.clamp(0.0, 1.0);
    Color::srgb(0.95 - 0.15 * t, 0.85 - 0.65 * t, 0.25 - 0.1 * t)
}
