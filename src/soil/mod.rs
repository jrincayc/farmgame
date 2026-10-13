//! Soil definitions, water model helpers and soil layout generation
//! (design doc §6.1-§6.3, §6.7; requirements §4.2).
//!
//! Soils are data, not code: each `assets/soil/*.json` is a `SoilDef`,
//! discovered at startup into a `SoilRoster`. Unlike crops, the roster is
//! read synchronously with `std::fs` rather than through the asset
//! server, because tiles are spawned in `Startup` and need to know the
//! soils (to generate the layout and the initial water depth) before the
//! first frame - an async asset load would only add a "not loaded yet"
//! state to every consumer for no benefit.

use std::fs;

use bevy::prelude::*;
use serde::Deserialize;

use crate::crops::CropDef;

pub const SOILS_DIR: &str = "assets/soil";

/// Depth of the root zone the volumetric fractions in `SoilDef` are
/// converted over (design doc §6.2). A single global placeholder; root
/// depth varying by crop is a later refinement (§6.11).
pub const ROOT_ZONE_DEPTH_MM: f32 = 500.0;
/// Rain in mm for a day with `WeatherToday::rainfall == 1.0`.
pub const MAX_DAILY_RAIN_MM: f32 = 40.0;

#[derive(Debug, Clone, Deserialize)]
pub struct SoilDef {
    pub id: String,
    pub name: String,
    /// Path relative to the assets root, e.g. `soil/loamy.png`.
    pub sprite: String,
    /// Volumetric fractions.
    pub wilting_point: f32,
    pub field_capacity: f32,
    pub saturation: f32,
    /// Fraction of water above field capacity lost per day.
    pub drainage_rate: f32,
    /// At 25 °C (see `et_factor`).
    pub evaporation_mm_per_day: f32,
    pub fallow_regen_rate: f32,
    /// 0 (coarse) .. 1 (fine); only used to order soils for world
    /// generation.
    pub texture: f32,
}

impl SoilDef {
    pub fn wilting_mm(&self) -> f32 {
        self.wilting_point * ROOT_ZONE_DEPTH_MM
    }
    pub fn field_capacity_mm(&self) -> f32 {
        self.field_capacity * ROOT_ZONE_DEPTH_MM
    }
    pub fn saturation_mm(&self) -> f32 {
        self.saturation * ROOT_ZONE_DEPTH_MM
    }

    /// Drought axis: 0 at the wilting point, 1 at field capacity and above.
    pub fn available(&self, water_mm: f32) -> f32 {
        let range = (self.field_capacity - self.wilting_point).max(f32::EPSILON);
        (((water_mm / ROOT_ZONE_DEPTH_MM) - self.wilting_point) / range).clamp(0.0, 1.0)
    }

    /// Waterlogging axis: air-filled porosity as a volumetric fraction.
    pub fn air(&self, water_mm: f32) -> f32 {
        (self.saturation - water_mm / ROOT_ZONE_DEPTH_MM).max(0.0)
    }

    /// Adds water (rain or irrigation); anything above saturation is lost.
    pub fn add_water(&self, water_mm: f32, added_mm: f32) -> f32 {
        (water_mm + added_mm.max(0.0)).min(self.saturation_mm())
    }

    /// Water lost to drainage in one day: the excess over field capacity
    /// decays toward it by `drainage_rate`.
    pub fn drain(&self, water_mm: f32) -> f32 {
        let fc = self.field_capacity_mm();
        if water_mm > fc {
            water_mm - (water_mm - fc) * self.drainage_rate.clamp(0.0, 1.0)
        } else {
            water_mm
        }
    }
}

/// All soil definitions, sorted by `texture` so a soil's index is its
/// position along the coarse-to-fine axis. Tiles store that index; saves
/// store the string id.
#[derive(Resource, Debug, Clone)]
pub struct SoilRoster {
    pub defs: Vec<SoilDef>,
}

impl SoilRoster {
    pub fn from_defs(mut defs: Vec<SoilDef>) -> Self {
        defs.sort_by(|a, b| a.texture.total_cmp(&b.texture).then_with(|| a.id.cmp(&b.id)));
        Self { defs }
    }

    pub fn load_from_dir(dir: &str) -> Self {
        let mut defs = Vec::new();
        match fs::read_dir(dir) {
            Ok(read_dir) => {
                for entry in read_dir.filter_map(Result::ok) {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("json") {
                        continue;
                    }
                    let parsed = fs::read_to_string(&path)
                        .map_err(|e| e.to_string())
                        .and_then(|s| serde_json::from_str::<SoilDef>(&s).map_err(|e| e.to_string()));
                    match parsed {
                        Ok(def) => {
                            validate(&def, &path.display().to_string());
                            info!("Discovered soil def: {}", def.id);
                            defs.push(def);
                        }
                        Err(e) => error!("Could not load soil def {:?}: {}", path, e),
                    }
                }
            }
            Err(e) => error!("Could not read {} ({})", dir, e),
        }
        if defs.is_empty() {
            error!("No soil definitions found in {} - falling back to a built-in loam", dir);
            defs.push(fallback_loam());
        }
        Self::from_defs(defs)
    }

    pub fn get(&self, index: usize) -> &SoilDef {
        &self.defs[index.min(self.defs.len() - 1)]
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.defs.iter().position(|d| d.id.eq_ignore_ascii_case(id))
    }
}

fn fallback_loam() -> SoilDef {
    SoilDef {
        id: "loamy".into(),
        name: "Loam".into(),
        sprite: "soil/loamy.png".into(),
        wilting_point: 0.12,
        field_capacity: 0.28,
        saturation: 0.45,
        drainage_rate: 0.30,
        evaporation_mm_per_day: 1.0,
        fallow_regen_rate: 0.05,
        texture: 0.5,
    }
}

/// Warn-only checks, in the spirit of `crops::roster::validate_crop_defs`.
fn validate(def: &SoilDef, label: &str) {
    if !(0.0 <= def.wilting_point
        && def.wilting_point < def.field_capacity
        && def.field_capacity < def.saturation
        && def.saturation <= 1.0)
    {
        warn!(
            "Soil '{}': expected 0 <= wilting_point < field_capacity < saturation <= 1, got {}/{}/{}",
            label, def.wilting_point, def.field_capacity, def.saturation
        );
    }
    if !(0.0..=1.0).contains(&def.drainage_rate) {
        warn!("Soil '{}': drainage_rate should be in 0..=1, got {}", label, def.drainage_rate);
    }
}

/// Temperature scaling for evaporation and transpiration (design doc
/// §6.3). All `*_mm_per_day` values are "at 25 °C".
pub fn et_factor(temp: f32) -> f32 {
    (temp / 25.0).clamp(0.1, 1.5)
}

/// Water leaving the soil in one day to evaporation plus (if a living
/// crop is planted) transpiration. `crop` is the crop definition and its
/// current growth-stage index; pass `None` for bare soil or a dead crop.
/// Shared by `farm::degrade_soil` and the tile inspector so the two
/// can't drift apart.
pub fn daily_et_mm(soil: &SoilDef, crop: Option<(&CropDef, usize)>, temp: f32) -> f32 {
    let factor = et_factor(temp);
    let transpiration = crop
        .map(|(def, stage)| {
            let stage_factor = def.growth_stages.get(stage).map_or(1.0, |s| s.water_use_factor);
            def.daily_water_use_mm * stage_factor
        })
        .unwrap_or(0.0);
    (soil.evaporation_mm_per_day + transpiration) * factor
}

// ---------------------------------------------------------------------
// Soil layout (design doc §6.7)
// ---------------------------------------------------------------------

/// Lattice spacing of the base noise octave, in tiles - sets patch size.
const PATCH_SIZE_TILES: f32 = 18.0;

fn hash2(seed: u64, x: i32, y: i32) -> f32 {
    let mut h = seed
        ^ (x as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    h ^= h >> 33;
    h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    h ^= h >> 33;
    h = h.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    h ^= h >> 33;
    (h >> 40) as f32 / (1u64 << 24) as f32
}

/// Smooth 2D value noise in 0..1.
fn value_noise(seed: u64, x: f32, y: f32) -> f32 {
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let smooth = |t: f32| t * t * (3.0 - 2.0 * t);
    let (sx, sy) = (smooth(fx), smooth(fy));
    let (xi, yi) = (x0 as i32, y0 as i32);
    let a = hash2(seed, xi, yi);
    let b = hash2(seed, xi + 1, yi);
    let c = hash2(seed, xi, yi + 1);
    let d = hash2(seed, xi + 1, yi + 1);
    let top = a + (b - a) * sx;
    let bottom = c + (d - c) * sx;
    top + (bottom - top) * sy
}

/// One continuous texture field: two octaves of value noise, with a
/// light domain warp so patches aren't blobby.
fn texture_noise(seed: u64, x: f32, y: f32) -> f32 {
    let (px, py) = (x / PATCH_SIZE_TILES, y / PATCH_SIZE_TILES);
    let wx = px + (value_noise(seed ^ 0xA1, px * 2.0, py * 2.0) - 0.5) * 0.8;
    let wy = py + (value_noise(seed ^ 0xB2, px * 2.0, py * 2.0) - 0.5) * 0.8;
    let base = value_noise(seed, wx, wy);
    let detail = value_noise(seed ^ 0xC3, wx * 2.3, wy * 2.3);
    base * 0.75 + detail * 0.25
}

/// Soil index (into `roster`) for every tile, row-major from (0,0).
/// The noise field is split into the roster's soils, ordered by texture,
/// at quantiles - so the proportions hold on any farm size and neighbouring
/// textures grade into one another. The two ends of the texture range get
/// weight 1 and the soils between them weight 2 (25/50/25 for the starter
/// three).
pub fn generate_soil_map(seed: u64, width: u32, height: u32, roster: &SoilRoster) -> Vec<u8> {
    let n = (width * height) as usize;
    let soils = roster.defs.len();
    if soils <= 1 {
        return vec![0; n];
    }

    let noise: Vec<f32> = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| texture_noise(seed, x as f32, y as f32))
        .collect();

    let weights: Vec<f32> = (0..soils).map(|i| if i == 0 || i == soils - 1 { 1.0 } else { 2.0 }).collect();
    let total: f32 = weights.iter().sum();

    let mut sorted = noise.clone();
    sorted.sort_by(f32::total_cmp);
    let mut thresholds = Vec::with_capacity(soils - 1);
    let mut acc = 0.0;
    for w in &weights[..soils - 1] {
        acc += w / total;
        thresholds.push(sorted[((acc * n as f32) as usize).min(n - 1)]);
    }

    noise
        .iter()
        .map(|v| thresholds.iter().filter(|t| v >= t).count() as u8)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster() -> SoilRoster {
        SoilRoster::load_from_dir(SOILS_DIR)
    }

    #[test]
    fn starter_soils_load_ordered_by_texture() {
        let r = roster();
        let ids: Vec<&str> = r.defs.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["sandy", "loamy", "clay"]);
    }

    #[test]
    fn loam_has_most_accessible_water_and_clay_drains_slowest() {
        let r = roster();
        let access = |id: &str| {
            let d = r.get(r.index_of(id).unwrap());
            d.field_capacity_mm() - d.wilting_mm()
        };
        assert!(access("loamy") > access("clay") && access("clay") > access("sandy"));
        let rate = |id: &str| r.get(r.index_of(id).unwrap()).drainage_rate;
        assert!(rate("sandy") > rate("loamy") && rate("loamy") > rate("clay"));
    }

    #[test]
    fn air_at_field_capacity_matches_design_doc() {
        let r = roster();
        let air = |id: &str| {
            let d = r.get(r.index_of(id).unwrap());
            d.air(d.field_capacity_mm())
        };
        assert!((air("sandy") - 0.28).abs() < 1e-4);
        assert!((air("loamy") - 0.17).abs() < 1e-4);
        assert!((air("clay") - 0.12).abs() < 1e-4);
    }

    #[test]
    fn rain_beyond_saturation_is_runoff() {
        let r = roster();
        let d = r.get(1);
        assert_eq!(d.add_water(d.saturation_mm() - 1.0, 40.0), d.saturation_mm());
    }

    #[test]
    fn drainage_is_faster_on_sand_than_clay() {
        let r = roster();
        let (sand, clay) = (r.get(0), r.get(2));
        let sand_left = sand.drain(sand.saturation_mm()) - sand.field_capacity_mm();
        let clay_left = clay.drain(clay.saturation_mm()) - clay.field_capacity_mm();
        let sand_frac = sand_left / (sand.saturation_mm() - sand.field_capacity_mm());
        let clay_frac = clay_left / (clay.saturation_mm() - clay.field_capacity_mm());
        assert!(sand_frac < clay_frac);
        // Below field capacity nothing drains.
        assert_eq!(sand.drain(sand.field_capacity_mm() - 5.0), sand.field_capacity_mm() - 5.0);
    }

    #[test]
    fn et_scales_with_temperature() {
        let r = roster();
        let d = r.get(1);
        assert!(daily_et_mm(d, None, 30.0) > daily_et_mm(d, None, 25.0));
        assert!(daily_et_mm(d, None, 25.0) > daily_et_mm(d, None, -5.0));
        assert!((daily_et_mm(d, None, 25.0) - d.evaporation_mm_per_day).abs() < 1e-5);
    }

    #[test]
    fn soil_map_has_requested_proportions_and_is_patchy() {
        let r = roster();
        let (w, h) = (64u32, 64u32);
        let map = generate_soil_map(7, w, h, &r);
        let count = |i: u8| map.iter().filter(|&&s| s == i).count() as f32 / map.len() as f32;
        assert!((count(0) - 0.25).abs() < 0.02);
        assert!((count(1) - 0.50).abs() < 0.02);
        assert!((count(2) - 0.25).abs() < 0.02);

        // Patchy: most horizontal neighbours share a soil, and sand rarely
        // touches clay.
        let (mut same, mut sand_clay, mut pairs) = (0, 0, 0);
        for y in 0..h {
            for x in 0..w - 1 {
                let (a, b) = (map[(y * w + x) as usize], map[(y * w + x + 1) as usize]);
                pairs += 1;
                same += (a == b) as u32;
                sand_clay += (a.abs_diff(b) == 2) as u32;
            }
        }
        assert!(same as f32 / pairs as f32 > 0.8);
        assert!((sand_clay as f32) < 0.01 * pairs as f32);
    }

    #[test]
    fn soil_map_is_deterministic_per_seed_and_varies_between_seeds() {
        let r = roster();
        assert_eq!(generate_soil_map(3, 40, 40, &r), generate_soil_map(3, 40, 40, &r));
        assert_ne!(generate_soil_map(3, 40, 40, &r), generate_soil_map(4, 40, 40, &r));
    }
}
