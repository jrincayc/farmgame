//! Crop roster discovery (requirements §4.3: "adding a new crop should
//! mean adding a new JSON data entry, not writing new code"). Before
//! this, the *data* was already crop-agnostic, but two things weren't:
//! `ui::panels::STARTER_CROPS` hardcoded the four starter crops' names
//! and paths for the Plant dropdown, and `ui::render::crop_initial`
//! hardcoded a crop-id-to-display-letter lookup. Both meant adding a
//! fifth crop JSON still required a code change - this module closes
//! that gap by scanning `assets/crops/*.json` at startup instead.
//!
//! **Desktop-only assumption, consistent with the project's target
//! platform (requirements §3):** this uses `std::fs::read_dir` against
//! the source `assets/crops` directory directly, rather than going
//! through Bevy's virtual asset filesystem (which dropped folder-loading
//! support as of Bevy 0.14, precisely because it doesn't work reliably
//! cross-platform, especially on wasm). Since `cargo run`'s working
//! directory is the repo root, `assets/crops` resolves the same way it
//! does for `build.rs`'s copy step - this isn't reading from
//! `target/<profile>/assets`, just discovering *which* files exist so
//! `AssetServer::load` (which does go through the proper Bevy asset
//! path) knows what to ask for.

use std::fs;

use bevy::prelude::*;

use super::crop_def::CropDef;

/// Directory relative to the current working directory - see module doc
/// for why this is a plain `std::fs` read rather than an asset-server
/// operation, and why that's fine specifically because `cargo run`'s
/// CWD is the repo root.
const CROPS_DIR: &str = "assets/crops";

#[derive(Debug, Clone)]
pub struct CropRosterEntry {
    /// Filename without extension (e.g. "tomato" for "tomato.json").
    /// Used as a fallback display name/dropdown label before the asset
    /// has finished loading, and as a sanity-check against `CropDef.id`
    /// once it has (see `validate_crop_defs` below).
    pub file_stem: String,
    pub handle: Handle<CropDef>,
}

/// Every crop JSON found under `assets/crops` at startup. Empty (not a
/// panic) if the directory is missing or unreadable - logged as an
/// error, not silently swallowed, since an empty roster means the Plant
/// dropdown has nothing in it.
#[derive(Resource, Debug, Clone, Default)]
pub struct CropRoster {
    pub entries: Vec<CropRosterEntry>,
}

pub struct CropRosterPlugin;

impl Plugin for CropRosterPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CropRoster>()
            .add_systems(Startup, discover_crop_roster)
            .add_systems(Update, validate_crop_defs);
    }
}

fn discover_crop_roster(asset_server: Res<AssetServer>, mut roster: ResMut<CropRoster>) {
    let read_dir = match fs::read_dir(CROPS_DIR) {
        Ok(rd) => rd,
        Err(e) => {
            error!(
                "Could not read {} ({}) - crop roster will be empty, nothing will be plantable",
                CROPS_DIR, e
            );
            return;
        }
    };

    for entry in read_dir.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(file_stem) = path.file_stem().and_then(|s| s.to_str()) else {
            warn!("Skipping crop file with non-UTF8 name: {:?}", path);
            continue;
        };

        // AssetServer wants a path relative to the assets root
        // (`crops/tomato.json`), not the filesystem path we just read
        // from (`assets/crops/tomato.json`) - build.rs's copy step is
        // what makes "crops/" resolve correctly at `target/<profile>/assets`.
        let asset_path = format!("crops/{}.json", file_stem);
        let handle: Handle<CropDef> = asset_server.load(&asset_path);

        info!("Discovered crop def: {}", asset_path);
        roster.entries.push(CropRosterEntry {
            file_stem: file_stem.to_string(),
            handle,
        });
    }

    if roster.entries.is_empty() {
        warn!(
            "{} contained no .json files - crop roster is empty",
            CROPS_DIR
        );
    }
}

/// Runs whenever a `CropDef` asset finishes loading, checking a handful
/// of invariants the JSON schema itself can't enforce (serde only
/// checks a field is *present and the right type*, not that
/// `min_temp <= max_temp` or similar). Every check here is a `warn!`,
/// not a rejection - an invalid crop still loads and can still be
/// planted, since refusing to load it outright would be a bigger,
/// less-forgiving behavior change than just surfacing the problem. This
/// satisfies "if it's missing something, write a warning" without
/// silently dropping a crop the person is actively iterating on.
fn validate_crop_defs(
    mut events: EventReader<AssetEvent<CropDef>>,
    crop_defs: Res<Assets<CropDef>>,
    roster: Res<CropRoster>,
) {
    for event in events.read() {
        let (AssetEvent::LoadedWithDependencies { id } | AssetEvent::Added { id }) = event else {
            continue;
        };
        let Some(def) = crop_defs.get(*id) else {
            continue;
        };

        let label = roster
            .entries
            .iter()
            .find(|e| &e.handle.id() == id)
            .map(|e| e.file_stem.as_str())
            .unwrap_or(def.id.as_str());

        if def.growth_stages.is_empty() {
            warn!("Crop '{}': growth_stages is empty - it can never mature", label);
        }
        if def.min_temp > def.max_temp {
            warn!("Crop '{}': min_temp > max_temp ({} > {})", label, def.min_temp, def.max_temp);
        }
        if def.ideal_temp_range.0 > def.ideal_temp_range.1 {
            warn!("Crop '{}': ideal_temp_range is inverted", label);
        }
        if def.ideal_temp_range.0 < def.min_temp || def.ideal_temp_range.1 > def.max_temp {
            warn!("Crop '{}': ideal_temp_range falls outside min_temp/max_temp", label);
        }
        if def.min_moisture > def.max_moisture {
            warn!(
                "Crop '{}': min_moisture > max_moisture ({} > {})",
                label, def.min_moisture, def.max_moisture
            );
        }
        if def.ideal_moisture_range.0 < def.min_moisture || def.ideal_moisture_range.1 > def.max_moisture {
            warn!("Crop '{}': ideal_moisture_range falls outside min_moisture/max_moisture", label);
        }
        let t = &def.quality_thresholds;
        if !(t.premium_max_stress < t.good_max_stress && t.good_max_stress < t.lethal_stress) {
            warn!(
                "Crop '{}': quality_thresholds should satisfy premium < good < lethal, got {}/{}/{}",
                label, t.premium_max_stress, t.good_max_stress, t.lethal_stress
            );
        }
        if def.base_yield == 0 {
            warn!("Crop '{}': base_yield is 0 - harvesting it will never produce anything", label);
        }
        if def.id != label {
            warn!(
                "Crop '{}': CropDef.id ('{}') doesn't match its filename - this is confusing but not fatal",
                label, def.id
            );
        }
    }
}
