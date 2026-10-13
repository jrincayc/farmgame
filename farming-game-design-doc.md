# Farming Simulation Game — Design Document (v1)

**Companion to:** `farming-game-requirements.md`
**Engine:** Bevy (Rust) `=0.16.0`, exact-pinned; ECS architecture
**UI:** `bevy_egui` `=0.34.1` (targets Bevy 0.16), supporting a Light/Dark theme toggle (requirements §4.0)
**Data format:** JSON, used for both crop definitions and save files
**Scope:** MVP-focused, with notes on where stretch goals (conditional scheduling, greenhouses, equipment upgrades) will hook in later

---

## 1. Architecture Overview

The game is organized as a set of Bevy **plugins**, each owning one system from the requirements doc. Plugins communicate primarily through **Resources** (shared global state) and **Events** (one-shot signals), not by directly calling into each other's code. This keeps each system swappable/testable in isolation — a stated non-functional requirement.

```
src/
├── main.rs
├── time/          # day counter, day-advance event
├── climate/        # region temperature curve, weather
├── farm/           # tile grid, soil state
├── crops/           # crop assets, growth systems, harvesting
├── economy/         # money, market prices, buy/sell
├── player/          # movement, tool interaction, inventory
├── save/             # serialize/deserialize game state
└── ui/                # HUD, menus (bevy_egui or native)
```

Each of `time`, `climate`, `farm`, `crops`, `economy`, `player`, `save` is a `Plugin` registered in `main.rs`. `ui` reads from the others but doesn't own simulation state.

### 1.1 Core loop, at a glance

Nothing in this game runs on a real-time simulation tick for *state* purposes. The entire simulation advances on a single event: **`DayAdvanced`**. Rendering, input, and camera movement still run every frame as normal Bevy `Update` systems — but soil, crops, weather, economy, and queued actions only change state in response to that one event. This directly reflects requirement §4.1 ("most systems can just be 'on day advance, do X'").

What *triggers* `DayAdvanced` depends on the current `TimeControl` mode (§2.1a): a manual **Step** press fires it once; **Timer** mode fires it automatically when a real-time countdown elapses; **Paused** mode never fires it, which is what makes queuing actions ahead of time (§2.7) meaningful — the player can queue a full day's plan, then Step or unpause to watch it resolve.

```
TimeControl countdown elapses (Timer)
  or player presses Step
        │
        ▼
  DayAdvanced event fired
        │
        ├──▶ climate::advance_day       (roll new weather, check frost calendar)
        ├──▶ farm::degrade_soil         (moisture drains, fertility drifts)
        ├──▶ crops::process_action_queue (resolve queued plant/water/fertilize/harvest, capacity-limited)
        ├──▶ crops::tick_growth         (advance growth_stage for all PlantedCrop)
        ├──▶ crops::check_failure       (kill crops outside viable temp range)
        ├──▶ economy::update_prices     (drift market prices — no-op for MVP's static formula)
        └──▶ time::increment_day        (day counter += 1)
```

Ordering matters here — e.g. weather for the *new* day should be rolled before the action queue and growth tick check against it, and queued waterings/fertilizing should land before growth tick reads soil state. Bevy's `SystemSet`/`.chain()` ordering will be used to pin this sequence explicitly rather than relying on registration order.

---

## 2. Core Data Structures

### 2.1 Time (Resource)

```rust
#[derive(Resource)]
pub struct Calendar {
    pub current_day: u32,      // absolute day count since game start
    pub year_length: u32,      // days per year, e.g. 112 (28/season × 4)
}

#[derive(Event)]
pub struct DayAdvanced {
    pub new_day: u32,
}
```

`Calendar` is intentionally dumb — it just counts. Anything that cares about season labels derives them from `current_day % year_length`, it isn't stored redundantly.

### 2.1a Time Control (Resource)

```rust
#[derive(Resource)]
pub struct TimeControl {
    pub mode: TimeMode,
}

pub enum TimeMode {
    Paused,
    Timer { speed: TimerSpeed, elapsed_ms: f32 },
    // Step has no persistent state — it's a one-shot system that fires DayAdvanced directly
}

pub enum TimerSpeed {
    Slow,   // e.g. 60s real-time per game day
    Normal, // e.g. 20s
    Fast,   // e.g. 5s
}
```

A `Timer`-mode system runs every frame, accumulates `elapsed_ms`, and fires `DayAdvanced` (then resets `elapsed_ms`) once the threshold for the current `TimerSpeed` is reached. `Paused` and the `Step` button both bypass that accumulation — `Step` just fires `DayAdvanced` directly, once, from a UI button handler, regardless of `TimeControl.mode`. Exact per-speed durations are a playtesting question, not an architecture one (see §5).

### 2.2 Region & Climate (Resource)

```rust
#[derive(Resource)]
pub struct Region {
    pub base_temp_curve: TempCurve,
    pub frost_days: HashSet<u32>,   // day-of-year values with frost risk
}

pub enum TempCurve {
    Sine { min: f32, max: f32, phase_offset: f32 }, // MVP: simple sine wave over the year
    // Preset { name: String } — later: swap in "tropical"/"cold" presets
}

impl TempCurve {
    pub fn temp_on(&self, day_of_year: u32, year_length: u32) -> f32 { /* ... */ }
}
```

`Region` is a single global resource for MVP (one climate, per §6 scope). It's already shaped so that a future multi-region game would just mean multiple `Region`-like entities keyed by farm location, rather than a rewrite.

**Stretch hook (greenhouses, §4.3a):** a `TileClimateModifier` component (see 2.3) will apply an offset to whatever `Region::base_temp_curve` returns for that tile, e.g. `effective_temp = region.temp_on(day) + modifier.offset`. Not implemented in MVP, but `temp_on()` is written as a pure function specifically so a modifier can wrap it without touching `Region` itself.

### 2.3 Farm Tiles & Soil (ECS Entities + Components)

Each tile is a Bevy **entity**. This is the core ECS design choice: a tile is not a struct in an array, it's an entity that soil, crop, and (later) building components can attach to independently.

**Grid size (requirements §4.2, §9):** farm dimensions are a resource read at world-gen time, not a compiled-in constant, so a future size-picker UI doesn't require restructuring:

```rust
#[derive(Resource, Debug, Clone)]
pub struct FarmConfig {
    pub width: u32,
    pub height: u32,
}

impl FarmConfig {
    pub const MIN_DIMENSION: u32 = 32;
    pub const MAX_DIMENSION: u32 = 200; // caps grid at 40,000 tiles

    /// MVP default: comfortably in "thousands of tiles" territory
    /// (requirements §5) without yet exercising the upper cap.
    pub fn default_mvp() -> Self {
        Self { width: 64, height: 64 }
    }
}
```

Tile-spawning code reads `FarmConfig` rather than hardcoding a loop bound; the eventual size-picker UI (stretch, if it doesn't make MVP) would just mean something writes into this resource before world-gen runs, instead of after — no change to how tiles are spawned.

```rust
#[derive(Component)]
pub struct Tile {
    pub coord: IVec2,
}

#[derive(Component)]
pub struct SoilState {
    pub fertility: f32,     // 0.0–1.0
    pub moisture: f32,      // 0.0–1.0
    pub soil_type: SoilType,
    // Added Phase 4: set true by ActionKind::Till, consumed (cleared)
    // by a successful ActionKind::Plant. A field needs re-tilling
    // between crops, not just once ever - gives Till a recurring
    // throughput role (requirements §4.6a) rather than a one-time
    // formality.
    pub tilled: bool,
}

pub enum SoilType { Sandy, Loamy, Clay }

// Stretch (§4.3a): attached only to tiles under a greenhouse/structure
#[derive(Component)]
pub struct TileClimateModifier {
    pub offset: f32,
    pub min_clamp: Option<f32>,
}
```

Querying "all tiles with soil but no crop" or "all planted, matured crops" becomes a normal Bevy query (`Query<(&Tile, &SoilState), Without<PlantedCrop>>`), rather than manual iteration/filtering logic.

**Planned change (§6.1–6.2, §6.5, §6.7):** `SoilType` as a closed enum and `SoilState.moisture` as a single 0.0–1.0 number are slated to be replaced by data-driven soil definitions and an explicit water-depth model, and a tile gains an optional field assignment. Nothing above changes until those stages land.

### 2.3a Soil Dynamics

`SoilState.fertility` was previously undefined behavior — the day-advance diagram (§1.1) named `farm::degrade_soil` but no formula existed, and the prototype spike left fertility static. This fills that gap. **Implemented in Phase 3** (fallow-tile half only — see below).

**While a tile has a `PlantedCrop`:** each `DayAdvanced`, that tile's `SoilState.fertility` changes by `def.fertility_impact_per_day` (§2.4). Positive values (most crops) drain the soil; negative values (legumes, cover crops) replenish it. This is the same "declare tolerance/impact as data, let outcomes emerge from comparison" approach already used for climate suitability (§4.3a of the requirements doc) — crop rotation isn't a system to build, it's a consequence of players sequencing crops with complementary `fertility_impact_per_day` values. **Implemented in Phase 4.**

**Phase 4 sign-convention correction:** the pseudocode below originally added `def.fertility_impact_per_day` to `soil.fertility` directly, with no negation. That contradicts this section's own stated convention — a positive ("draining") value would *increase* fertility, and the starter roster's one legume (a negative value, meant to demonstrate nitrogen-fixing replenishment) would have *decreased* it. Backwards on both counts, and it would have quietly undermined the "crop rotation is emergent" pitch this section leads with. The corrected formula negates the field: `delta = -def.fertility_impact_per_day`. A regression-style note lives alongside the fix in `farm::degrade_soil`.

**While a tile is fallow (no `PlantedCrop`):** fertility drifts back toward a baseline (e.g. 0.6) at a rate depending on `SoilType` (§2.3) — e.g. Loamy regenerates fastest, Sandy slowest. This is the first real use of `SoilType` beyond being an inert label.

```rust
fn degrade_soil(
    mut tiles: Query<(&mut SoilState, Option<&PlantedCrop>)>,
    crop_defs: Res<Assets<CropDef>>,
) {
    for (mut soil, planted) in &mut tiles {
        let delta = match planted {
            Some(crop) => crop_defs.get(&crop.def)
                // Negated: fertility_impact_per_day is documented as
                // "positive drains, negative replenishes" - so a
                // positive value must SUBTRACT from fertility, not add.
                // See the Phase 4 correction note above.
                .map(|def| -def.fertility_impact_per_day)
                .unwrap_or(0.0),
            None => {
                // Drift toward FALLOW_FERTILITY_BASELINE (e.g. 0.6),
                // not a flat additive constant - see correction note
                // below for why this matters.
                let rate = soil.soil_type.fallow_regen_rate(); // e.g. Loamy: 0.05, Sandy: 0.015
                (FALLOW_FERTILITY_BASELINE - soil.fertility) * rate
            }
        };
        soil.fertility = (soil.fertility + delta).clamp(0.0, 1.0);
    }
}
```

**Phase 3 correction:** an earlier draft of this pseudocode used a flat `soil.soil_type.fallow_regen_rate()` as the delta directly (e.g. "Loamy: +0.01, Sandy: +0.003"), independent of the tile's current fertility. That doesn't drift toward a baseline at all — it's a constant positive delta every day, so fertility would just climb until clamped at 1.0, contradicting this section's own prose. The corrected formula above — `delta = (baseline − fertility) × rate` — is what actually produces "drifts back toward a baseline and levels off there," and is what Phase 3 implements. A regression test (`fertility_settles_near_baseline_rather_than_maxing_out`) guards against reintroducing the flat-delta version.

Runs on `DayAdvanced`, ordered before `tick_growth` per §1.1's existing chain (fertility for the day should be settled before growth reads it).

Exact regen rates and `fertility_impact_per_day` values per crop are a playtesting question (§5), same as the quality-threshold and equipment-capacity numbers already flagged there.

**Moisture (Phase 3, new — not in the original design):** the design doc didn't previously specify a moisture formula beyond "affected by weather" (§3.4). Phase 3 implements it alongside fertility, in the same `degrade_soil` system: each `DayAdvanced`, `SoilState.moisture` changes by `weather.rainfall * RAINFALL_TO_MOISTURE - DAILY_EVAPORATION`, clamped to `[0.0, 1.0]`. `RAINFALL_TO_MOISTURE` (fraction of rolled rainfall that actually reaches the soil, vs. runoff) and `DAILY_EVAPORATION` (moisture lost on a dry day) are both placeholders alongside the other tunables flagged in §5. There's no watering/irrigation input yet — that's the action-queue `Water` action in Phase 4 (requirements §4.2's "irrigation or manual watering as a moisture mechanic").

**Phase 8 update (recorded here after the fact; the paragraph above described the Phase 3 formula and had drifted from the code):** rainfall's contribution is softened — `weather.rainfall * RAINFALL_TO_MOISTURE * (1.0 - soil.moisture)` — so a day's rain adds less to an already-wet tile, while evaporation stays a flat subtraction (that flatness is what lets moisture recover instead of clinging just under the ceiling). `ActionKind::Water` applies the same `(1.0 - moisture)` softening to its own bump. See the doc comment on `farm::degrade_soil`.

**Planned (§6.2–6.3):** the single moisture number is slated to become a water depth, with a wilting point, field capacity and saturation per soil, plus drainage, runoff and temperature-scaled evapotranspiration. That also retires the `(1.0 - moisture)` softening, which was a patch for overshoot that a saturation cap and drainage handle physically.

### 2.4 Crops (Bevy Assets + Component)

This is the piece that has to be data-driven per the requirements doc. Crop *definitions* are assets loaded from data files; a planted crop is a component that references one.

```rust
#[derive(Asset, TypePath, Deserialize)]
pub struct CropDef {
    pub id: String,
    pub name: String,
    pub growth_stages: Vec<GrowthStage>,   // e.g. [Sprout(3d), Growing(5d), Mature(2d)]
    pub min_temp: f32,
    pub max_temp: f32,
    pub ideal_temp_range: (f32, f32),
    pub frost_tolerant: bool,
    // Phase 4 schema change: water_requirement (a single scalar) only
    // modeled under-watering. Real crops can also suffer from too much
    // water (root rot, waterlogging) - so moisture now mirrors the
    // temperature fields exactly: an absolute viable range plus a
    // narrower ideal range, both on SoilState.moisture's 0.0-1.0 scale.
    pub min_moisture: f32,
    pub max_moisture: f32,
    pub ideal_moisture_range: (f32, f32),
    pub fertility_requirement: f32,
    pub base_yield: u32,
    pub sell_price: u32,
    pub seed_cost: u32,
    pub quality_thresholds: QualityThresholds,  // maps worst_single_day_stress -> quality grade
    pub optimal_harvest_window_days: u32,  // days the crop can sit at full maturity before overripening starts
    pub overripe_stress_per_day: f32,      // stress added per day past that window (feeds the same accumulators as temp stress, see §3.2)
    pub fertility_impact_per_day: f32,     // see §2.3a; positive drains soil, negative (legumes etc.) replenishes it
}

#[derive(Deserialize)]
pub struct QualityThresholds {
    pub premium_max_stress: f32,   // worst_single_day_stress below this -> Premium (1.5x sell_price)
    pub good_max_stress: f32,      // below this -> Good (1.0x sell_price); above -> Poor (0.5x sell_price)
    pub lethal_stress: f32,        // worst_single_day_stress at/above this kills the crop outright
}

#[derive(Deserialize)]
pub struct GrowthStage {
    pub name: String,
    pub duration_days: u32,
}

#[derive(Component)]
pub struct PlantedCrop {
    pub def: Handle<CropDef>,
    pub planted_day: u32,
    pub current_stage: usize,
    pub days_in_stage: u32,
    pub cumulative_stress: f32,       // sums a bit each day outside ideal_temp_range/under-watered; drives YIELD
    pub worst_single_day_stress: f32, // max single-day deviation seen (frost, drought spike, etc.); drives QUALITY
    pub dead: bool,                   // crop hit lethal_stress; stays on the tile as a visible dead state until the player clears it
}
```

A crop that dies isn't despawned immediately (an earlier draft of §3.2 implied this). Per Pillar 2 (agency through information), the player needs a legible signal that something died and roughly why — an instantly-vanishing entity doesn't give them that. Dead crops sit on the tile as a visible, distinct state (`dead: true`) until explicitly cleared by the player, at which point the tile returns to bare soil.

**Why two numbers instead of one:** in practice, crop yield tracks *cumulative* average conditions across the growing cycle, while crop quality is driven more by *acute* stress at a single bad moment (one frost night, one heat spike during fruit-set). Splitting them gives the player two genuinely different things to manage rather than one blended difficulty knob — a field that's mostly fine but survives one frost night ends up full-yield/poor-quality; a field that's chronically a bit under-watered but never hits a crisis point ends up low-yield/decent-quality. That's a real tradeoff, and it stays legible to the player (§2 pillar: agency through information) because each number has one clear cause.

Crop files live under `assets/crops/*.json` (Bevy's asset server loads these via a custom `AssetLoader<CropDef>` backed by `serde_json`). Adding a crop is: write a new `.json` file. No Rust changes, no recompile of game logic — satisfies the requirements doc's core architectural constraint directly.

Example `assets/crops/tomato.json`:
```json
{
    "id": "tomato",
    "name": "Tomato",
    "growth_stages": [
        { "name": "sprout", "duration_days": 3 },
        { "name": "growing", "duration_days": 6 },
        { "name": "mature", "duration_days": 2 }
    ],
    "min_temp": 10.0,
    "max_temp": 35.0,
    "ideal_temp_range": [18.0, 28.0],
    "frost_tolerant": false,
    "min_moisture": 0.2,
    "max_moisture": 0.9,
    "ideal_moisture_range": [0.4, 0.7],
    "fertility_requirement": 0.4,
    "base_yield": 12,
    "sell_price": 8,
    "seed_cost": 15,
    "quality_thresholds": {
        "premium_max_stress": 0.5,
        "good_max_stress": 2.0,
        "lethal_stress": 8.0
    },
    "optimal_harvest_window_days": 2,
    "overripe_stress_per_day": 1.5,
    "fertility_impact_per_day": 0.015
}
```

(Tomato as a short-window, steep-decay crop; a wheat-style crop would use a longer window and gentler per-day stress.)

**Planned schema change (§6.4):** the three moisture fields (`min_moisture`, `max_moisture`, `ideal_moisture_range`) are slated to be replaced by a drought pair and a waterlogging pair, and `CropDef` gains `daily_water_use_mm`, with an optional per-stage `water_use_factor` on `GrowthStage`.

### 2.5 Economy (Resource)

**Phase 4 note:** `Wallet` is implemented now, as a stub — Phase 4's `Plant` action needs somewhere to deduct `seed_cost` from, and `Harvest` needs an `Inventory` (§2.4's harvested-goods store, not shown separately here since it's a straightforward `HashMap<String, Vec<HarvestedLot>>` keyed by crop id) to add quality-tagged yield to. `Market` and `sale_value()` below are still Phase 5 — there's no selling flow yet, just the wallet/inventory bookkeeping the queue needs to run.

**Phase 5 — implemented, and the pending design question is resolved.** The end of Phase 4 flagged an open question: does selling go through `ActionQueue`/`Equipment` capacity (a `Sell` `ActionKind`) or is it instant? **Resolved: instant, not queued.** Equipment capacity (§2.6) models physical fieldwork *on tiles* — tilling, planting, harvesting. Selling already-harvested goods sitting in `Inventory` isn't fieldwork; it has no tile to target at all, and it's a transaction against a market rather than labor. Harvesting is already capacity-limited (`ActionKind::Harvest`), so the "respect the player's time" pillar (§2 pillar 3) is already served upstream of the sale itself — folding `Sell` into the queue would conflate two different constraints (equipment-days vs. money) for no gameplay benefit at MVP scope. Selling is fired via a `SellRequested { crop_id }` event and resolved the same frame it's sent, not gated on `DayAdvanced`.

**Phase 5 — sells all lots of a crop at once, not per-lot/per-quality.** Selling individual lots (e.g. holding Premium stock while dumping Poor stock) is a plausible stretch, but MVP doesn't need the extra UI/complexity `Inventory::harvested` already keeps lots separate per harvest (rather than merging into one running total), specifically so a later per-lot sell is additive — filtering which lots get drained before the same per-lot valuation loop, not a rewrite.

```rust
#[derive(Resource)]
pub struct Wallet {
    pub money: u32,
}

#[derive(Resource, Default)]
pub struct Market {
    pub price_modifiers: HashMap<String, f32>, // crop id -> time_factor multiplier; absent = 1.0 for MVP
}

impl Market {
    pub fn time_factor(&self, crop_id: &str) -> f32 {
        self.price_modifiers.get(crop_id).copied().unwrap_or(1.0)
    }
}

pub fn sale_value(base_sell_price: u32, quality: QualityGrade, time_factor: f32) -> f32 {
    base_sell_price as f32 * quality.multiplier() * time_factor  // Poor 0.5x / Good 1.0x / Premium 1.5x
}

#[derive(Event)]
pub struct SellRequested { pub crop_id: String }

// process_sell_requests (Update, not DayAdvanced-gated): for each
// SellRequested, drain all Inventory lots for crop_id, sum
// sale_value(def.sell_price, lot.quality, market.time_factor(crop_id)) * lot.quantity
// across them, credit the rounded total to Wallet.money. Looks up
// CropDef by scanning Assets<CropDef> for a matching `id` field, since
// Inventory only stores the id string (§2.4/§2.8), not a Handle.
```

`Market::price_modifiers` exists as a real field from day one, just pinned to `1.0` everywhere for MVP (via `time_factor()`'s `unwrap_or(1.0)` default, rather than requiring every crop id to be pre-populated). This is deliberate: the **stretch-goal futures market / variable pricing** (§7 of the requirements doc) becomes "make something else write into this map over time," not a new field or a rewrite of `sale_value`.

### 2.6 Equipment & Purchasable Structures (Resource + Component)

```rust
#[derive(Resource)]
pub struct Equipment {
    pub daily_capacity: HashMap<ActionKind, u32>, // modern machinery, e.g. Till (tractor plough) -> 80, Harvest (combine) -> 120
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionKind { Till, Plant, Water, Fertilize, Harvest, Clear }

// Stretch: purchasable structures (e.g. greenhouses) placed on tiles
#[derive(Component)]
pub struct Structure {
    pub kind: String,       // e.g. "greenhouse"
    pub tiles_covered: Vec<IVec2>,
}
```

**Phase 4 addition — `Clear`:** §2.4's dead-crop state ("stays on the tile until explicitly cleared by the player") needed an `ActionKind` to resolve through. Given its own capacity bucket rather than folding into `Till`, since clearing debris and tilling a field are different equipment in practice.

For MVP, `Equipment::daily_capacity` is a fixed set of starting values, not modified by purchases — purchasable upgrades (raising these numbers) and purchasable `Structure`s (like greenhouses, which also spawn a `TileClimateModifier` per covered tile) are both stretch goals sharing one purchase flow: deduct `seed_cost`-equivalent from `Wallet`, then either bump a capacity value or spawn a `Structure` + tile modifiers.

Equipment is modeled as modern (~2000s USA) machinery — tractor-pulled ploughs, planters, sprayer/irrigation rigs, combine harvesters — not gardening hand tools. That's a scale decision with a real consequence: the farm's tile grid (§5 open questions) has to be large enough (thousands of tiles, not hundreds) that a combine's daily capacity doesn't just clear the whole farm in one `DayAdvanced`. Otherwise the action-queue/capacity mechanic in §2.7 has nothing left to constrain.

### 2.7 Action Queue (Resource)

This is what makes `Paused` mode useful rather than just a menu state — the player queues work while paused, then lets `DayAdvanced` resolve as much of it as `Equipment` capacity allows.

```rust
#[derive(Resource, Default)]
pub struct ActionQueue {
    pub pending: VecDeque<PendingAction>,
}

/// Counter backing `PendingAction::id`. A plain incrementing `u64` is
/// enough — IDs only need to be unique and stable for the lifetime of
/// the queue entry, not globally meaningful.
#[derive(Resource, Default)]
pub struct NextActionId(pub u64);

pub struct PendingAction {
    pub id: u64,   // stable handle for future reorder/cancel UI — see note below
    pub tile: Entity,
    pub kind: ActionKind,
    pub crop_to_plant: Option<Handle<CropDef>>, // only relevant for ActionKind::Plant
}
```

**Stable IDs (requirements §9, resolved):** `PendingAction` carries an `id` from day one, even though MVP's queue UI is FIFO-display-only with no reorder/cancel controls. This is deliberate scope-splitting: the *data* supports player reordering/cancellation now (a `retain`/`remove` on `pending` filtered by `id` is trivial), so building that UI later is additive — it doesn't require migrating `PendingAction` or touching `process_action_queue`. Enqueuing a new action means reading `NextActionId`, using its value as the new action's `id`, then incrementing it.

Processing (`crops::process_action_queue`, run early in the `DayAdvanced` chain, per §1.1): for each `ActionKind`, pop up to `Equipment.daily_capacity[kind]` matching entries off the front of `pending` and apply them; anything left in the queue simply waits for the next `DayAdvanced`. FIFO order per action kind keeps this predictable and legible to the player (§2 pillar) — "I queued these first, they happen first" — rather than needing a priority system for MVP. (A future reorder feature would let the player change that order before resolution; it wouldn't change how resolution itself works.)

**Phase 8 note:** `PendingAction` also carries `water_intensity: f32` (default `1.0`; the "½ Water" button queues `0.5`), a multiplier on the Water action's moisture bump, ignored for every other kind.

**Planned change (§6.6):** `PendingAction.tile: Entity` becomes a `TileOrField` target resolved at processing time, each action tracks which tiles it has already visited, and `water_intensity` becomes `water_depth_mm`.

### 2.8 Save Data (Serialized snapshot) — **implemented, Phase 7**

Rather than trying to serialize live ECS entities directly (fragile across versions), save/load works through an explicit **snapshot struct** (`save::SaveGame`) that systems populate from/apply to the world, written to disk as JSON via `serde_json::to_string_pretty` / `from_str`, one file per save under `save/<name>.json` (requirements §4.6, resolved: multiple named saves, repo-root `save/` directory rather than an OS user-data dir, so it's trivially `.gitignore`-able as a whole).

```rust
#[derive(Serialize, Deserialize)]
struct SaveGame {
    version: u32,
    calendar: Calendar,
    wallet: Wallet,
    equipment: SavedEquipment,     // see note below on why this isn't Equipment directly
    market: Market,
    ui_theme: UiTheme,
    farm_config: FarmConfig,
    inventory: Inventory,
    tiles: Vec<SavedTile>,
    action_queue: Vec<SavedAction>, // see note below — persisted, contrary to this section's original plan
    next_action_id: u64,
}

#[derive(Serialize, Deserialize)]
struct SavedTile {
    coord: IVec2,
    soil: SoilState,
    planted_crop: Option<SavedCrop>,
}

#[derive(Serialize, Deserialize)]
struct SavedCrop {
    crop_id: String,   // re-resolved to a Handle<CropDef> on load via CropRoster
    planted_day: u32,
    current_stage: usize,
    days_in_stage: u32,
    cumulative_stress: f32,
    worst_single_day_stress: f32,
    dead: bool,
}

#[derive(Serialize, Deserialize)]
struct SavedAction {
    id: u64,
    tile_coord: IVec2,           // re-resolved to an Entity via the rebuilt TileMap
    kind: ActionKind,
    crop_to_plant: Option<String>, // re-resolved via CropRoster, same as SavedCrop
}
```

This indirection is deliberate: it means save-file format is decoupled from internal ECS layout, so refactoring components later doesn't corrupt old saves as badly (a stated non-functional requirement).

**Revision from this section's original plan — `ActionQueue` *is* persisted.** The paragraph originally here said queued-but-unresolved actions weren't saved, on the reasoning that requiring an empty queue at save time was simplest for MVP. That was revisited before implementation: mid-plan saves (a paused game with several actions already queued) are common enough in how this gets played that dropping the queue on every save/load would be a real loss, not just a simplification. `PendingAction.tile` (an `Entity`) and `.crop_to_plant` (a `Handle<CropDef>`) don't survive serialization as-is, so `SavedAction` mirrors the same "store an id, re-resolve on load" pattern `SavedCrop` already used for `PlantedCrop.def`. A queued action referencing a tile or crop that no longer resolves after a load (crop JSON deleted, farm rebuilt at a different size than the save assumed — the latter shouldn't happen since the farm *is* rebuilt at the saved size, but is guarded regardless) is dropped with a `warn!`, not treated as a fatal load error.

**`equipment: SavedEquipment`, not `Equipment` directly:** `Equipment::daily_capacity` is a `HashMap<ActionKind, u32>`. Serializing an enum-keyed `HashMap` straight to JSON depends on serde_json's map-key handling for non-string keys, which wasn't worth relying on unverified (no `cargo check` in this workflow — see the project's stated compile-risk convention). `SavedEquipment` instead stores the same data as `Vec<(ActionKind, u32)>`, which is unambiguous regardless.

**What else changed from the original sketch:** `market: Market`, `ui_theme: UiTheme`, `farm_config: FarmConfig`, and `inventory: Inventory` weren't in this section's original `SaveGame` draft — `Inventory` in particular was a real gap (harvested-but-unsold goods would have vanished on load). All four are plain data with no `Entity`/`Handle` fields, so each derives `Serialize`/`Deserialize` directly on the live resource type rather than needing its own mirror struct — see each type's own Phase 7 doc-comment note (`economy::Wallet`/`Inventory`, `economy::Market`, `ui::UiTheme`, `farm::FarmConfig`, `farm::SoilState`) for the specifics. `TimeControl`/`TimeMode` are deliberately **not** persisted — `load_game` always resumes in `Paused`, regardless of what mode was active at save time, so a loaded game never starts ticking days via a Timer the player hasn't just set. `climate::Region` also isn't persisted — it's a single fixed MVP climate (requirements §6), not per-save data.

**Planned additions (§6.8):** per-tile soil id and water depth, per-tile field assignment plus the field registry, and `SavedAction` targets, visited tiles and water depth, each with a migration path for existing saves.

---

## 3. System Flow Detail

### 3.1 Planting (queued action)

```
Player selects tile + crop, presses "queue plant" →
    - client-side check: tile has SoilState, is tilled, no existing PlantedCrop, player has seed_cost in Wallet
      (a soft check for player feedback — the real check happens at resolution time, §3.1a)
    - push PendingAction { tile, kind: Plant, crop_to_plant: Some(handle) } onto ActionQueue.pending
```

**Phase 4 addition — planting requires a tilled tile:** resolution now also checks `soil.tilled`, set by a prior `ActionKind::Till` and consumed (cleared to `false`) by a successful Plant. A field needs re-tilling between crops, not just once ever — this is what gives `Till` a real, recurring throughput role (requirements §4.6a) rather than being a one-time formality with no ongoing equipment demand.

Money is **not** deducted at queue time — only at resolution, so a player can queue more plantings than their current wallet supports and it'll simply fail/skip at resolution if funds run out by then (rather than pre-committing funds to actions that might get reordered or canceled while still paused, per the open question in the requirements doc).

### 3.1a Action Queue Processing (on `DayAdvanced`, before growth tick)

```rust
fn process_action_queue(
    mut queue: ResMut<ActionQueue>,
    equipment: Res<Equipment>,
    mut tiles: Query<(&mut SoilState, Option<&PlantedCrop>)>,
    mut wallet: ResMut<Wallet>,
    crop_defs: Res<Assets<CropDef>>,
    calendar: Res<Calendar>,
    mut commands: Commands,
) {
    let mut done_today: HashMap<ActionKind, u32> = HashMap::new();

    queue.pending.retain(|action| {
        let cap = equipment.daily_capacity.get(&action.kind).copied().unwrap_or(0);
        let done = done_today.entry(action.kind).or_insert(0);
        if *done >= cap {
            return true; // leave in queue for tomorrow
        }
        // re-validate at resolution time (funds, tile state may have changed since queued)
        // and apply the action — plant / water / fertilize / harvest as appropriate
        *done += 1;
        false // remove from queue, it resolved
    });
}
```

This system owns the "equipment capacity throttles the day" rule from §4.6a of the requirements doc in one place — every action type shares the same throttling logic, generic over `ActionKind`, rather than each action having its own bespoke daily-limit check.

### 3.2 Growth tick (on `DayAdvanced`, after action queue processing)

**Phase 4 correction — frost was dead data:** this section's original pseudocode computed `temp_stress` from `Region::temp_on` alone and never referenced `WeatherToday.is_frost`, despite `frost_tolerant` existing specifically to interact with frost. A frost event is a sharp, discrete overnight cold snap layered on top of the smooth seasonal curve (§3.4) — a day can sit comfortably within a crop's viable temperature range on the curve and still frost that night. Without checking `is_frost`, `frost_tolerant` had no effect on anything. The pseudocode below adds a flat stress hit on a frost day for non-frost-tolerant crops.

**Phase 4 addition — moisture stress mirrors temperature stress:** per the schema change in §2.4, moisture now has its own min/max/ideal range, checked the same way temperature is. A scale factor (`MOISTURE_STRESS_SCALE`) converts the 0.0–1.0 moisture range into stress units comparable to temperature's degree-Celsius range — without it, moisture deviations would always be negligible next to temperature ones.

**Phase 4 addition — fertility gate:** soil below `fertility_requirement` now adds a mild, "outside ideal but viable" stress, the same treatment as being outside a crop's ideal (but still viable) temperature or moisture range.

```rust
fn tick_growth(
    mut crops: Query<(Entity, &mut PlantedCrop, &Tile)>,
    soils: Query<&SoilState>,
    region: Res<Region>,
    calendar: Res<Calendar>,
    weather: Res<WeatherToday>,
    crop_defs: Res<Assets<CropDef>>,
) {
    for (entity, mut crop, tile) in &mut crops {
        if crop.dead {
            continue; // stays on the tile until the player clears it - see §2.4
        }

        let def = crop_defs.get(&crop.def).unwrap();
        let soil = soils.get(entity).unwrap(); // PlantedCrop and SoilState live on the same tile entity
        let temp = region.temp_on(calendar.current_day, calendar.year_length);
        // effective_temp would include TileClimateModifier here, once greenhouses exist

        let temp_stress = if temp < def.min_temp || temp > def.max_temp {
            // outside absolute viable range — severe, single-day stress
            (def.min_temp - temp).max(temp - def.max_temp)
        } else if !(def.ideal_temp_range.0..=def.ideal_temp_range.1).contains(&temp) {
            0.25 // mild, outside ideal but still viable
        } else {
            0.0
        };

        // Frost: a discrete event, not read from the smooth temp curve
        // at all - see correction note above.
        let frost_stress = if weather.is_frost && !def.frost_tolerant {
            FROST_STRESS // flat placeholder, e.g. 6.0
        } else {
            0.0
        };

        // Moisture: same shape as temp_stress, scaled onto comparable units.
        let moisture_stress = if soil.moisture < def.min_moisture || soil.moisture > def.max_moisture {
            (def.min_moisture - soil.moisture).max(soil.moisture - def.max_moisture) * MOISTURE_STRESS_SCALE
        } else if !(def.ideal_moisture_range.0..=def.ideal_moisture_range.1).contains(&soil.moisture) {
            0.25
        } else {
            0.0
        };

        let fertility_stress = if soil.fertility < def.fertility_requirement {
            0.25 // mild, same treatment as outside-ideal temp/moisture
        } else {
            0.0
        };

        // Overripening: only applies once a crop has completed its final
        // stage and sat there past optimal_harvest_window_days. Feeds the
        // exact same accumulators as every other stress source - "left to
        // rot" is not a separate mechanic, it's another source of daily
        // stress.
        let is_at_final_stage = !def.growth_stages.is_empty()
            && crop.current_stage == def.growth_stages.len() - 1
            && crop.days_in_stage >= def.growth_stages[crop.current_stage].duration_days;
        let overripe_days = if is_at_final_stage {
            crop.days_in_stage.saturating_sub(
                def.growth_stages[crop.current_stage].duration_days + def.optimal_harvest_window_days,
            )
        } else {
            0
        };
        let today_stress = temp_stress + frost_stress + moisture_stress + fertility_stress
            + overripe_days as f32 * def.overripe_stress_per_day;

        crop.cumulative_stress += today_stress;                                  // feeds YIELD
        crop.worst_single_day_stress = crop.worst_single_day_stress.max(today_stress); // feeds QUALITY

        if crop.worst_single_day_stress >= def.quality_thresholds.lethal_stress {
            crop.dead = true; // crop dies but stays visible on the tile - see §2.4; player clears it explicitly
            continue;
        }

        // Guard against indexing past the last stage: once current_stage is
        // the final index, the crop just sits there, accumulating
        // days_in_stage - which is exactly what the overripening check above
        // is reading. An earlier version of this pseudocode incremented
        // current_stage unconditionally here, which panics on the next tick
        // once a crop finishes its last stage.
        crop.days_in_stage += 1;
        if crop.current_stage + 1 < def.growth_stages.len() {
            let stage = &def.growth_stages[crop.current_stage];
            if crop.days_in_stage >= stage.duration_days {
                crop.current_stage += 1;
                crop.days_in_stage = 0;
            }
        }
    }
}
```

This is one system, generic over every crop type — the whole point of the data-driven design. There's no `if crop.id == "tomato"` branch anywhere. Note `today_stress` deliberately feeds *both* accumulators: a single frost night both nudges cumulative yield loss up a little *and* sets the quality ceiling for the whole harvest, which matches how it'd actually work — one bad night doesn't erase all your averaged-out good care, but it does cap how good the final produce can be. Overripening (sitting unharvested past `optimal_harvest_window_days`) is not a distinct failure mode — it's just another source of `today_stress`, so it's read by the same two accumulators rather than needing a parallel "ripeness" system.

### 3.3 Harvesting (queued action)

```
Player selects a tile with PlantedCrop where current_stage is the last growth
stage AND days_in_stage >= growth_stages[current_stage].duration_days (i.e.
the final stage has actually completed, not merely been entered), presses
"queue harvest" →
    push PendingAction { tile, kind: Harvest, crop_to_plant: None } onto ActionQueue.pending

At resolution time (§3.1a), for each harvest action that fits within capacity:
    yield_amount = def.base_yield * yield_factor(crop.cumulative_stress, soil.fertility, soil.moisture)
    quality = quality_grade(crop.worst_single_day_stress, def.quality_thresholds)
    → add `yield_amount` units of crop.id (tagged with `quality`) to player Inventory
    → despawn PlantedCrop component from tile (soil remains)
```

**Phase 4 note — no sale at harvest:** an earlier draft of this pseudocode computed `sale_value_per_unit` at harvest time using `Market` (§2.5). That's a Phase 5 concern — Phase 4 only stocks `Inventory` with quality-tagged lots; converting them to money is a separate, later action once `Market`/`sale_value()` exist. Keeping harvest and selling as two distinct steps also matches how the requirements doc's own MVP loop reads: "plant → grow → harvest → sell," not "plant → grow → harvest-and-sell."

**Phase 4 addition — `yield_factor(...)` was previously named but never defined.** Implemented as two independent [0,1]-clamped factors multiplied together:

```rust
fn yield_factor(cumulative_stress: f32, fertility: f32, moisture: f32) -> f32 {
    let stress_factor = (1.0 - cumulative_stress / YIELD_STRESS_DIVISOR).clamp(0.0, 1.0);
    let soil_factor = ((fertility + moisture) / 2.0).clamp(MIN_SOIL_YIELD_FACTOR, 1.0);
    stress_factor * soil_factor
}
```

`stress_factor` tapers to zero as `cumulative_stress` approaches `YIELD_STRESS_DIVISOR` (a §5-style placeholder). `soil_factor` reflects the tile's fertility/moisture *at harvest time*, but is floored at `MIN_SOIL_YIELD_FACTOR` (also a placeholder, e.g. 0.3) rather than being able to zero out yield on its own — a crop that otherwise grew well on mediocre-but-viable soil shouldn't harvest nothing just because the tile wasn't pristine. `yield_amount` is `(def.base_yield as f32 * yield_factor(...)).round() as u32`. **Planned (§6.4):** the `moisture` input to `soil_factor` has to change when moisture becomes a water depth.

**Dead crops use `Clear`, not `Harvest`:** a `PlantedCrop` with `dead: true` can't be queued for `Harvest` (resolution rejects it, per §3.1a) — it goes through `ActionKind::Clear` instead, which removes the `PlantedCrop` component with no yield, matching §2.4's "stays until explicitly cleared" behavior.

Harvesting late (after `optimal_harvest_window_days` has elapsed since maturity) isn't blocked — it's already reflected in `worst_single_day_stress` and `cumulative_stress` by the time the player harvests, per §3.2. A tomato left two extra days will simply grade lower (or die) by the time it's picked; there's no separate "too late to harvest" rule to implement.

`yield_factor(...)` and `quality_grade(...)` are two separate pure functions, deliberately not combined into one "overall harvest quality" score — yield is forgiving and cumulative (rewards mostly-good care), quality is spiky and threshold-based (punishes the single worst moment). Keeping them as two functions with two different shapes is what makes each legible on its own; a player looking at a "full yield, Poor quality" harvest can reason directly to "yield's fine, but something spiked hard once."

### 3.4 Weather (on `DayAdvanced`, before growth tick)

**Implemented in Phase 2.** MVP weather: check `Region.frost_days` for frost risk (with a per-risk-day probability roll, not automatic — `FROST_CHANCE_ON_RISK_DAY`, a §5-style placeholder), and roll rainfall (affecting `SoilState.moisture` deltas). No separate weather "system" beyond a `WeatherToday` resource that other systems read.

**Rainfall correction (Phase 2):** an earlier draft rolled a single continuous `0.0..=1.0` value every day, which meant every day got *some* rainfall, just sometimes a trace amount. Real precipitation is intermittent, not "a little every day," so this is a two-stage roll instead: first, does it rain at all today (`RAIN_CHANCE_PER_DAY`, another placeholder — most days come out dry); only if that passes does it roll the `0.0..=1.0` amount. Otherwise `rainfall` is `0.0`.

```rust
#[derive(Resource)]
pub struct WeatherToday {
    pub rainfall: f32,      // 0.0–1.0, added to soil moisture that day; 0.0 on non-rain days
    pub is_frost: bool,
}
```

---

---

## 3.5 UI (Phase 6)

`ui::UiPlugin` wires `bevy_egui` and a placeholder rendering layer, per §1's original note that `ui/` reads from the other modules but doesn't own simulation state — every panel below is read/write access to resources/events other plugins already own (`ActionQueue`, `Wallet`, `Inventory`, `TimeControl`, `SellRequested`), not new simulation logic.

**Placeholder tile rendering (`ui/render.rs`):** a 2D camera framed to the default farm size, with pan (arrow keys, middle-click-drag, or Ctrl/Cmd+left-click-drag) and zoom (scroll wheel or top-bar buttons), clamped to the farm's extent and bounded between 'whole farm visible' and 16x that. Note (post-Phase-6b): camera pan/zoom was originally deferred to the future rendering/art-pipeline phase (§10) on the reasoning that building controls against placeholder art would be throwaway work. In practice, a fixed camera on a farm of thousands of tiles made playtesting genuinely painful (tiles easy to miss entirely), so pan/zoom was pulled forward and implemented against the placeholder rendering. The camera logic operates on Camera/Transform/Projection, not on what's drawn, so it's expected to carry over largely intact once real sprites land, rather than being thrown away, one flat-colored `Sprite` per tile (color by `SoilType`, darker overlay when tilled, distinct color when the planted crop is dead) plus a `Text2d` label (crop initial C/G/T/W, `.` for tilled-bare, `X` for dead, blank otherwise). Explicitly not the real art pipeline — just enough to make tile-targeted UI meaningful. Click-to-select (left mouse button, ignored if egui itself wants the pointer that frame) writes a `SelectedTile` resource every other panel reads.

**Panels (`ui/panels.rs`):**
- Top bar: day/season readout (season is a derived display label per §2.1, not a stored value), time control (Step/Pause/Timer speed buttons), theme toggle.
- Right side panel: selected-tile inspector — soil type/fertility/moisture/tilled state, and if planted, crop name/stage/days-in-stage/cumulative + worst stress. Dead crops get an explicit red label, not just the tile's red color, per §2.4's "death must be visible" principle.
- Left side panel: action queue — buttons to queue Till/Water/Fertilize/Harvest/Clear on the selected tile, a crop-picker dropdown + Plant button, and a scrollable FIFO-order display of `ActionQueue.pending` (id + kind). No reorder/cancel controls (requirements §4.6a/§9, deliberately deferred) even though `PendingAction::id` already supports it.
- Bottom bar: wallet balance, inventory aggregated per crop id, a Sell button per crop id firing `SellRequested` (§2.5's Phase 5 instant-sell flow, unchanged by this phase).

**Theme (`UiTheme` resource):** light/dark toggle via `egui::Visuals::light()`/`dark()`, applied every frame from the resource rather than only on toggle. Default Light (requirements §4.0). Not yet persisted across runs — Phase 7 (save/load) can read this resource directly once it exists.

Dev keyboard controls from Phases 4-5 (`main.rs`) are kept alongside the new UI for fast manual testing, not removed — the UI is the primary way to play from this phase on, but the keyboard path costs nothing to leave in place.

---

---

## 3.6 Crop Roster Discovery (Phase 6b - post-Phase-6 follow-up)

Two bugs found during Phase 6 playtesting, both closed together:

1. **Hardcoded crop lists.** `ui::panels`' Plant dropdown and `ui::render`'s tile-letter lookup both hardcoded the four starter crops by id/path, contradicting requirements §4.3's "adding a crop should mean adding a JSON entry, not writing new code." Fixed by `crops::roster::CropRoster` — a Startup system scans `assets/crops/*.json` directly (via `std::fs::read_dir`, not Bevy's asset virtual filesystem, which dropped folder-loading in 0.14) and loads a `Handle<CropDef>` per file found. Both UI call sites now read this resource instead of a constant.
2. **`CropDef` assets unloading between harvest and sell.** `process_sell_requests` (§2.5) looks up a `CropDef` by scanning `Assets<CropDef>` for a matching id — but before this fix, the only strong `Handle<CropDef>` in the whole app was the one inside `PlantedCrop`, and both `Harvest` and `Clear` remove that component, dropping the handle. Bevy's asset server unloads an asset once its last strong handle drops, so by the time a harvested lot reached the sell flow, its `CropDef` could already be gone from `Assets<CropDef>` — sell silently failed (lots preserved, per `sell.rs`'s existing "don't destroy inventory" guard, but nothing was creditable). `CropRoster` incidentally fixes this too: it holds one strong handle per discovered crop for the whole app lifetime, independent of what's planted/harvested, so `Assets<CropDef>` never empties out mid-session.

`CropRoster` also runs light post-load validation (`validate_crop_defs`, reacting to `AssetEvent<CropDef>`) — sanity checks like `min_temp <= max_temp`, `ideal_*_range` falling inside the absolute range, and `quality_thresholds` ordering (`premium < good < lethal`), each logged as a `warn!` rather than rejecting the crop outright. This satisfies "warn if a crop JSON is missing/wrong" without making an in-progress, slightly-malformed crop file un-loadable while someone's actively editing it.

---

## 3.7 Save/Load (Phase 7)

`save::SavePlugin` adds three events (`SaveRequested`, `LoadRequested`, `DeleteSaveRequested { name }`) and three `Update`-scheduled systems reacting to them — not run every frame, only on request. `ui::panels::save_load_panel` is the sole UI entry point: a floating egui window (not docked alongside the other four panels, so it doesn't compete for their space) with a name field + Save button, and a live-scanned list of `save/*.json` files each with Load/Delete buttons.

**Load is a full farm rebuild, not a partial patch.** `load_game` despawns every current tile entity (`farm::despawn_all_tiles`), overwrites `FarmConfig` with the save's, respawns the grid at that size (`farm::spawn_tile_grid` — factored out of `farm::spawn_tiles` in this phase specifically so Startup and a mid-session load share the same grid-construction code rather than two copies of it), then walks the save's tile list re-inserting `SoilState` and, where present, `PlantedCrop`. This is what makes loading a save with a *different* farm size than whatever's currently running work correctly, rather than only supporting same-size loads.

**Crop re-resolution:** neither `PlantedCrop.def` (`Handle<CropDef>`) nor `PendingAction.tile`/`.crop_to_plant` (`Entity`, `Handle<CropDef>`) survive serialization — see §2.8's updated notes for the mirror-struct pattern (`SavedCrop`, `SavedAction`) that stores plain ids/coordinates instead and re-resolves them via `CropRoster`/the freshly-rebuilt `TileMap` on load, tile-by-tile and action-by-action, warning and skipping (not failing the whole load) on anything that no longer resolves.

**A save file is a point-in-time snapshot of *state*, not of *scheduling intent*.** Time control mode is explicitly excluded: `TimeControl`/`TimeMode` aren't part of `SaveGame` at all, and `load_game` unconditionally sets `TimeControl.mode = TimeMode::Paused` after loading, regardless of what mode was active when the save was made. This was a late addition once `ActionQueue` persistence was decided — a loaded save resuming with a Timer already counting down felt like the kind of surprise a save/load feature shouldn't introduce.

---

## 3.8 Stress Diagnostics & Harvest Readiness (Phase 8, first polish batch)

Playtesting turned up a real "player agency through information" gap
(§2 pillar 2): the tile inspector showed `cumulative_stress`/
`worst_single_day_stress` as raw numbers, with no way to tell *why* a
crop was stressed — too hot, too cold, too wet, too dry, starved soil,
or some combination — or which direction to correct. Separately, the
inspector gave no direct signal for harvest readiness; the player had
to compare `days_in_stage` against the stage's `duration_days` by hand.

**Design constraint (explicitly requested):** this must not duplicate
the stress-comparison logic that already lives in `growth::tick_growth`
— one function, called by both the simulation and the display, DRY.

**Implementation:** `crop_def::compute_daily_stress(def, temp, is_frost,
moisture, fertility, overripe_days) -> StressBreakdown` is now the
single place that decides what counts as stress. It returns not just
the stress values but a `RangeStatus` (`BelowMin` / `AboveMax` /
`BelowIdeal` / `AboveIdeal` / `Ideal`) per temp/moisture axis, so a
caller can render "too cold" vs "a bit cold" vs "ideal" without
re-deriving the comparison. `growth::tick_growth` calls it and sums
`.total` into the accumulators exactly as before (the stress *values*
are unchanged — this was a refactor of where the logic lives, not a
rebalance). `ui::panels::tile_inspector_panel` calls the exact same
function, purely for display, passing in today's actual weather/soil
state (via `Region`/`Calendar`/`WeatherToday`/`SoilState`, all of which
it already has access to or can trivially take as system params).

`overripe_days` — needed by `compute_daily_stress` but dependent on
`PlantedCrop`'s stage/days-in-stage state, which `crop_def.rs`
deliberately doesn't reference — is factored into `growth::overripe_days
(crop, def) -> u32`, callable from both `tick_growth` and the inspector.
Two related helpers went in alongside it, closing the harvest-readiness
gap the same way: `growth::is_mature(crop, def) -> bool` (previously
inlined once, in `queue::resolve_action`'s `Harvest` handler — now
shared with the inspector) and `growth::days_until_mature(crop, def)
-> Option<u32>` (new — walks the remaining growth stages to report a
countdown when not yet mature).

The inspector now shows, per selected planted tile: harvest readiness
("Ready to harvest!" or "N day(s) until mature"), then today's
temperature/moisture/fertility each with an ideal-range comparison and
a plain-language status, a frost callout when relevant, an overripe
callout when relevant, and today's total stress — in addition to the
existing cumulative/worst-day/quality figures. See `ui::panels`'
`temp_status_label`/`moisture_status_label` for the `RangeStatus`-to-
wording mapping (display-only, doesn't affect simulation).

**Planned (§6.4):** `compute_daily_stress` takes `moisture` today; it is slated to take a drought value and a waterlogging value instead, and to gain a shared evapotranspiration helper alongside it (§6.3), so the inspector can keep showing exactly what the simulation uses.

## 3.9 Toggleable Save/Load Window (Phase 8)

The save/load window (§3.7) was always open — a floating egui window
with `default_open(true)` and nothing to dismiss it, which sat on
screen for the whole session and got in the way during normal play.
Closed by adding `ui::SaveLoadWindowOpen(bool)` (default `false`),
toggled by a new "Save / Load" button in the top bar
(`panels::top_bar`). `save_load_panel` now passes `&mut open.0` to
`egui::Window::open(...)`, which gives the window egui's own close (×)
button wired to the same resource — so either the top-bar toggle or the
window's own chrome closes it, no separate plumbing needed for the two
paths. Not persisted in `save::SaveGame` — whether this particular
window happens to be open isn't game state.

## 4. What's Deliberately Deferred (per stretch goals)

- **Conditional scheduling (§4.7):** distinct from the MVP action queue (§2.7) — a future `ScheduledAction` component (`{ trigger: Condition, action: ActionKind }`) would be checked in the same `DayAdvanced` pass and, when triggered, simply push a `PendingAction` onto the existing `ActionQueue` rather than bypassing it. So scheduling builds *on top of* the queue instead of needing a separate resolution path.
- **Equipment upgrades:** `Equipment::daily_capacity` (§2.6) is written as a `HashMap` specifically so a purchase can just increase a value in it — no structural change needed when upgrades are added. **Implemented (fixed, non-purchasable) in Phase 4.**
- **Greenhouses (§4.3a):** `TileClimateModifier` is defined in §2.3, and `Structure` (§2.6) is shaped to spawn one per covered tile, but neither is wired into a purchase flow or the growth-tick lookup yet. Growth tick already has the comment marking where it plugs in.
- **Futures market / variable pricing (§4.5):** `Market::price_modifiers` (§2.5) already exists and is read by `sale_value()`; only something writing non-1.0 values into it needs to be built. **Phase 4 note:** `Wallet`/`Inventory` themselves are implemented now as stubs (§2.5). **Phase 5:** `Market`/`sale_value()`/the sell flow are now implemented (instant, not queued — see §2.5's resolved design question); the futures-market stretch goal remains exactly "write non-1.0 values into `price_modifiers`," unchanged by Phase 5's work.
- **Multi-region:** `Region` as a singleton `Resource` is an MVP simplification; the `temp_on()` function itself doesn't assume singleton-ness, so multi-region later means changing *how Region is looked up* (per-farm entity vs. global resource), not the climate math.
- **Real rendering / art pipeline:** Phase 6 shipped only placeholder tile visuals (flat colored squares, text labels) — see requirements doc §10 for the decisions already locked in ahead of that phase (data-driven growth-stage art, layered soil overlays, camera pan/zoom) versus what's still open (overripe-specific art, exact overlay presentation).
- **Irrigation rig geometry (center pivot, wheel line) and other water-model refinements:** see §6.9 and §6.11. The planned water model deliberately leaves room for them without building them.

---

## 5. Open Implementation Questions

- Asset hot-reloading: Bevy supports this out of the box for `Asset` types — worth enabling during development so crop `.json` tweaks don't require recompiling, given how central iterating on crop data will be to balancing.
- Tile grid size for MVP: now targeting thousands of tiles rather than hundreds (§5 non-functional target, driven by modern-equipment scale — §2.6), specifically so equipment capacity is a real constraint. A `HashMap<IVec2, Entity>` lookup is still fine at that scale in Bevy — this isn't a "need spatial partitioning" threshold, just worth confirming during prototyping that per-tile queries (e.g. `tick_growth` iterating every `PlantedCrop`) stay comfortably within the frame budget at a few thousand entities, since that wasn't validated at the smaller original target.
- Exact multiplier values for quality grades (0.5x/1.0x/1.5x) and the stress thresholds themselves are placeholders — real values only make sense after playtesting a few crops.
- Same for `Equipment::daily_capacity` starting values and `TimerSpeed` real-time durations — both playtesting questions, not architecture ones (mirrors the open questions in the requirements doc).
- **New (Phase 4):** `YIELD_STRESS_DIVISOR`, `MIN_SOIL_YIELD_FACTOR`, `MOISTURE_STRESS_SCALE`, `FROST_STRESS`, `FERTILITY_DEFICIT_STRESS`/moisture's equivalent, and the `Water`/`Fertilize` per-action bump amounts are all §5-style placeholders introduced this phase — same "playtesting question, not architecture question" category as everything else in this list, just not yet tuned against real crop runs.
- **New (Phase 4) — Bevy's exact `AssetLoader` trait signature:** `CropDefLoader` (crops/crop_def.rs) is written from memory against the pattern Bevy's own `custom_asset` example uses, but wasn't compiled before packaging (per your request to skip `cargo check` on this batch). If it doesn't compile as-is, the doc comment on `CropDefLoader` in that file lists the two most likely fixes (BoxedFuture vs. native `async fn`, and the `read_to_end` import). Worth checking this file first if `cargo check` fails anywhere in `crops/`.
- **Resolved:** `ActionQueue` player reordering/cancellation (requirements doc §9) — `PendingAction` carries a stable `id` from the start (§2.7), so the data model supports this even though the MVP UI itself is FIFO-display-only.
- **Resolved:** grid size (requirements doc §4.2, §9) — `FarmConfig { width, height }` (§2.3) is a resource, not a compiled-in constant, bounded by `FarmConfig::MIN_DIMENSION`/`MAX_DIMENSION`, with an MVP default of 64×64. A player-facing size picker is stretch UI on top of the same resource.
- **Resolved:** Bevy version — pinned to `=0.16.0` exactly, with `bevy_egui = "=0.34.1"` as the matching UI integration version.
- Asset path resolution: Bevy's default `AssetPlugin` looks for files under a single root (`target/<profile>/assets/`), with no built-in multi-location fallback. Confirmed during the prototype spike that this trips people up (assets copied to the project root aren't found until also copied under `target/debug/assets/`). **Resolved for now:** option (a), a `build.rs` that copies `assets/` into `target/<profile>/assets/` automatically on every build — implemented in Phase 1. Option (b), a custom `AssetSource`/`AssetReader` trying multiple candidate roots, remains a possible future upgrade once install-location behavior matters for a release build.
- **New (planned soil/water/fields work):** the open questions for that work are collected in §6.11 rather than repeated here.

---

## 6. Soil Definitions, Water Model, and Fields (all five stages implemented)

*Status: stages 1–4 of §6.10 (soil definitions, water model, transpiration, soil distribution + soil-type overlay) are implemented in `src/soil/mod.rs`, `farm`, `crops` and `save`; stage 5 (fields: `src/field/mod.rs`, `TileOrField` queue targets with visited sets in `crops::queue`, paint tool and outlines in `ui`) is implemented too. Field names are chosen as "Paint mode" / "Unassigned" in the UI (§6.11's naming question). Deviation from §6.1: `SoilRoster` is read synchronously with `std::fs` at plugin build rather than via an async asset loader, since tiles are spawned in `Startup` and need the soils immediately. Original status note: Sections 2–3 above still describe the code as it currently stands; this section describes where it is going and why. Phase numbers are deliberately unassigned. Every number below is a §5-style placeholder for playtesting.*

**Why this is needed.** Three things in the current design are thin. `SoilType` only changes `fallow_regen_rate` (§2.3a), so soil is almost cosmetic. Moisture is one 0.0–1.0 number that crops compare against ranges, so soils can't differ in how much water they hold or how fast they lose it, and crops don't consume any water at all (`tick_growth` only reads `SoilState`; the only moisture inputs are rain, a flat evaporation constant and the `Water` action). And with a farm of thousands of tiles, single-tile actions are the wrong grain for the player. This section adds data-driven soils, a water model that distinguishes drought from waterlogging, crop transpiration, patchy soil layout, and persistent multi-tile fields.

### 6.1 Soil definitions (data-driven)

`SoilType` stops being a closed enum. Each soil is a `SoilDef` asset loaded from `assets/soil/*.json`, discovered at startup the same way crops are (§3.6: `std::fs::read_dir`, one permanent strong handle per file held in a `SoilRoster`, for the same asset-unloading reason). Adding a soil type means adding a JSON file and a sprite — no code, matching the requirement already in place for crops (requirements §4.3).

```json
{
    "id": "loamy",
    "name": "Loam",
    "sprite": "soil/loamy.png",
    "wilting_point": 0.12,
    "field_capacity": 0.28,
    "saturation": 0.45,
    "drainage_rate": 0.30,
    "evaporation_mm_per_day": 1.0,
    "fallow_regen_rate": 0.05,
    "texture": 0.5
}
```

`wilting_point`, `field_capacity` and `saturation` are volumetric fractions (the usual soil-science tables). `drainage_rate` is the fraction of water above field capacity lost per day. `evaporation_mm_per_day` is defined at 25 °C (§6.3). `fallow_regen_rate` moves over from the `SoilType` method of the same name. `texture` (0 coarse … 1 fine) is used only to order soils for world generation (§6.7).

Placeholder starter values, textbook-shaped:

| Soil | wilting | field cap. | saturation | accessible (fc − wp) | drainage/day | evap mm/day | fallow regen | texture |
|---|---|---|---|---|---|---|---|---|
| Sandy | 0.06 | 0.12 | 0.40 | 0.06 | 0.60 | 1.5 | 0.015 | 0.0 |
| Loamy | 0.12 | 0.28 | 0.45 | 0.16 | 0.30 | 1.0 | 0.05 | 0.5 |
| Clay | 0.25 | 0.38 | 0.50 | 0.13 | 0.08 | 0.7 | 0.03 | 1.0 |

Sandy loses water fastest, clay slowest, and loam stores the most plant-available water. Clay holds more water in total than loam, but much of it is held too tightly to be usable — which is why the wilting point is an explicit parameter rather than folded into a capacity.

Tiles carry a small numeric soil id (an index into `SoilRoster`) rather than a `String` per tile; saves store the string id. Soil sprites come from each definition's `sprite` path, replacing the hardcoded `load_soil_sprites` / `SoilSpriteRoster` map keyed by `SoilType`.

### 6.2 Water model

`SoilState.moisture` becomes `SoilState.water_mm`: the water depth in the root zone, bounded by the soil's wilting point and saturation. The JSON fractions convert to depths through one global constant, `ROOT_ZONE_DEPTH_MM` (about 500), so rain, irrigation and evapotranspiration are all plain millimetres. At 500 mm, accessible water is roughly 30 mm for sand, 80 for loam and 65 for clay. (Root depth varying by crop or growth stage is a later refinement; §6.11.)

**Why not a per-soil moisture cap on the existing 0–1 scale.** An earlier idea was to give each soil a maximum moisture. That would silently change what crop moisture ranges mean — a crop with an ideal range of 0.6–0.8 could never reach it on a soil capped at 0.5. The fix is not a cap but a different state: store the water depth, and derive normalized values from it, so crop data stays soil-independent while soils differ in how many millimetres a given value represents.

Two derived values, computed by one method on `SoilState` given its `SoilDef`, and called by both `tick_growth` and the inspector:

- **available** = `((water − wilting) / (field_capacity − wilting)).clamp(0, 1)` — the drought axis.
- **air** = `saturation − water`, as a volumetric fraction — the waterlogging axis.

**Why waterlogging is measured as air-filled porosity.** Roots are damaged when water has displaced the air in the pores, so the quantity that matters is how much air is left, not how much water there is or what percentage of saturation it represents. With the starter numbers the air left at field capacity is 0.28 for sand, 0.17 for loam and 0.12 for clay, so clay is already marginal on air when fully drained, and it then drains slowly, extending the exposure. Two effects stack, and a percent-of-saturation measure would have hidden the pore-space difference.

**Daily update**, in `degrade_soil`, per `DayAdvanced`, per tile, using that day's temperature (§6.3):

1. **Rain:** add `weather.rainfall × MAX_DAILY_RAIN_MM` (about 40). Anything above saturation is runoff and is lost. This replaces `RAINFALL_TO_MOISTURE` and the Phase 8 `(1.0 − moisture)` softening.
2. **Drainage:** water above field capacity decays toward it by `drainage_rate` per day.
3. **Evaporation:** subtract `evaporation_mm_per_day × et_factor(temp)`.
4. **Transpiration:** if a living crop is planted, subtract `daily_water_use_mm × stage water_use_factor × et_factor(temp)`. Dead crops use none.
5. **Floor** at the wilting point. Evapotranspiration stops there; evaporation below the wilting point is ignored for simplicity.

The queue's `Water` action still runs after `degrade_soil` and before `tick_growth` (§1.1). It adds its depth the same way rain does (capped at saturation, excess lost), so an over-application shows up as waterlogging that same day and drains on the next one.

One consequence of that ordering: `tick_growth` sees the state after the day's drainage. Sand drains most of a heavy rain within the same update, so it rarely shows a post-rain peak; clay stays near saturation for days.

### 6.3 Evapotranspiration and temperature

Soil evaporation and crop transpiration both scale with temperature:

```rust
fn et_factor(temp: f32) -> f32 {
    (temp / 25.0).clamp(0.1, 1.5)
}
```

All `*_mm_per_day` values in JSON are therefore "at 25 °C", so authors reason about one number per soil or crop. On the default −5…32 °C curve the factor runs from the 0.1 floor in deep winter to about 1.3 at peak summer.

Two implementation notes. First, `degrade_soil` currently does `events.read().count()` and ignores which day each event is for; it needs to collect each event's `new_day`, as `tick_growth` does, and take `Region` and `Calendar`, so a multi-day catch-up uses each day's own temperature. Second, the calculation lives in one shared `daily_et_mm(soil, crop, stage, temp)` function called by both `degrade_soil` and the inspector, in the same spirit as `compute_daily_stress` (§3.8). The inspector can then show today's water use and a "days of water left at today's rate" figure (`(water − wilting) / daily_et`).

Scale check, from field capacity with about 4 mm/day of crop use plus soil evaporation: roughly 16 dry days on loam, 14 on clay and 5 on sand. If the game year is much shorter than 365 days (`Calendar::year_length`), the ET and drainage rates need to scale with it.

Behavioral consequences worth watching in playtesting: heat stress and drought stress now compound in summer, because hot days dry soil faster, so irrigation capacity matters most when it is hottest; and in cold wet seasons ET is near zero, so a winter crop on clay can waterlog.

### 6.4 Crop schema and stress

`CropDef` changes:

- **Removed:** `min_moisture`, `max_moisture`, `ideal_moisture_range`.
- **Added (drought axis):** `min_available_water`, `ideal_available_water`.
- **Added (waterlogging axis):** `min_air_porosity`, `ideal_air_porosity`.
- **Added (transpiration):** `daily_water_use_mm`, and an optional `water_use_factor` on each `GrowthStage` (serde default `1.0`), so sprouts sip and mid-growth peaks, all in JSON.

`compute_daily_stress` takes `available` and `air` in place of `moisture`. Both axes reuse `range_status` with an infinite maximum, so they can only come out `BelowMin`, `BelowIdeal` or `Ideal`, and `RangeStatus` itself is unchanged. The inspector's status labels need separate wording per axis ("too dry" / "a bit dry" versus "waterlogged" / "soggy"). Stress is scaled per axis: the drought deficit on the 0–1 available scale as before, and the waterlogging deficit normalized by its threshold, `(min_air − air) / min_air`, because air fractions sit around 0.1 and `MOISTURE_STRESS_SCALE` assumes a 0–1 range.

Placeholder example: a tomato with `min_air_porosity` 0.08 and `ideal_air_porosity` 0.15 would be mildly stressed on clay even at field capacity (air 0.12), fine on loam (0.17) and sand (0.28), and badly stressed on any soil at or near saturation.

Waterlogging is a per-day stress for now. Real root damage takes time, and a single post-rain day on well-drained soil would be unfairly punished by the worst-single-day quality axis. Slow clay drainage already produces multi-day exposure on its own; if playtesting shows spikes are too harsh, the fix is a "days below threshold" counter on the same stress path, not a new mechanic.

Other places that need updating when the schema changes:

- `yield_factor`'s `soil_factor = (fertility + moisture) / 2` (§3.3) uses moisture at harvest time. Replace `moisture` with the available-water fraction, and revisit during tuning.
- `validate_crop_defs` (§3.6) checks that the ideal range falls inside the absolute range for moisture; it needs the equivalent checks for the new pairs.
- The four starter crop JSON files need hand conversion.

### 6.5 Fields

A field is a named, persistent group of tiles. Each tile carries `field: Option<FieldId>`; `None` means unassigned. The per-tile value is the source of truth.

- **Registry.** A `FieldRegistry` resource maps `FieldId` to `{ name, color }`. IDs are stable and never reused within a session; names are unique, enforced in the UI only. Renaming edits the registry only, so saves and queued actions never depend on a string that can change.
- **Index.** A `HashMap<FieldId, BTreeSet<…>>` of each field's tiles, kept in sync by the paint tool and rebuilt from the per-tile values on load, so resolving a field never means scanning every tile on a multi-thousand-tile farm.
- **Paint tool.** A brush model: pick an active field (or "Unassigned", which acts as the eraser) and drag a rectangle. Assigning a tile to a field removes it from its previous one, so there is no separate removal step. Fields are arbitrary tile sets painted in rectangular drags, not rectangles only. Create, rename and delete live in a small panel; deleting a field sets its tiles back to `None`. **Naming:** "zone" is a familiar convention for the assigning side, but avoid "Clear" for the unassigning side — `ActionKind::Clear` already means clearing a dead crop (§2.6). "Release" or "Unassigned" avoids the collision.
- **Tractor order.** When a field is worked, its tiles are visited in boustrophedon order: rows numbered from the top of the farm starting at 1, odd rows worked right to left and even rows left to right (the tractor enters at the top and moves left, then down, then right, and so on). Parity is taken from the **farm-global row number**, not the row's position within the field, so painting a tile above the field mid-job doesn't flip every row's direction. The order is derived when needed from the field's tiles, never stored. Tile coordinates use Bevy's y-up axis, so the implementation must convert to rows counted from the top.
- **Rendering.** Draw field boundaries as outlines on edges where neighbouring tiles' field ids differ, rather than tinting tiles, because a tint would clash with the moisture/fertility tint (`soil_tint()`). Toggleable, alongside a soil-type overlay as its sibling; with patchy soil (§6.7) the player needs both when deciding where to draw a field. The drawing mechanism is left open, given the per-tile child-entity layering already used for soil, tilled overlay and crop.
- **Inspector.** A field-level summary: tile count, share of tiles below the drought threshold, share waterlogged, and millimetres needed to reach field capacity (mean, min, max), since mixed soils within a field respond differently to the same action.

### 6.6 Action queue with field targets

`PendingAction.tile: Entity` becomes `target: TileOrField`, either a single tile or a field, resolved when the action is processed rather than when it is queued. A field's tiles are looked up through the field index in tractor order (§6.5).

**Per-action visited set.** Every `PendingAction` carries the set of tiles it has already visited. Each day's processing walks the target's tiles in tractor order, skips visited ones, and for each remaining tile either applies the action (consuming equipment capacity) or, if it does not apply there (untilled for Plant, no mature crop for Harvest, and so on), marks the tile visited and counts it as skipped (consuming no capacity). The action completes when every tile currently in the field is visited. The queue panel shows live counts: applied, skipped, remaining.

*Correction from an earlier draft of this design:* completion was first sketched as purely derived from tile state ("done when no tile still needs the action"). That works for Till, Plant, Harvest and Clear, whose effect makes the action inapplicable afterwards, but not for Water or Fertilize, which add an amount and leave nothing in tile state saying "already done" — a field Water action would never finish. A visited set handles every kind uniformly and also settles whether a Plant action should wait for tiles tilled later the same day (it does not; they are skipped and reported).

Consequences of resolving at processing time:

- **Live membership.** Tiles added to a field mid-job are picked up, and tiles removed from it are ignored.
- **Deleted or emptied fields.** An action whose field no longer exists is dropped with a visible notice, never a panic or a hang; an empty field completes immediately.
- **Determinism.** Visited set plus deterministic order means saving and loading mid-job resumes the same way.
- **Single tiles.** A `Tile` target is the one-tile case of the same logic.

**Water in millimetres.** `water_intensity` becomes `water_depth_mm`. The UI offers a depth control in 5 mm steps with 10/20/30 presets, replacing the "½ Water" button. The depth is applied to every visited tile and capped at saturation. Equipment capacity remains per tile, so a deep soak is no slower than a light one; measuring capacity in millimetres × tiles is a possible later refinement (§6.9).

### 6.7 Soil distribution

Soils are currently laid out in columns. Replace this with one continuous texture field generated from seeded 2D noise (simplex or Perlin), thresholded into the available soils ordered by their `texture` value, so sandy tends to border loam and loam to border clay, and sandy-against-clay boundaries are rare. A new soil type slots in by its `texture` value with no code change. Target proportions set the thresholds (roughly 25/50/25 coarse/medium/fine for the starter three), and one frequency parameter sets patch size: tens to low hundreds of tiles for a multi-thousand-tile farm, with optional domain warping to avoid blobby results.

- The seed lives in `FarmConfig` with a serde default, so existing saves still load. Saves already store each tile's soil (`SavedTile.soil`), so loading never regenerates soil and old column-layout saves keep working unchanged.
- Adding a field to `FarmConfig` breaks tests that build it as a struct literal (`FarmConfig { width, height }`); they need `..Default::default()` or an updated constructor.
- `starting_soil_types_are_varied_across_a_small_farm` builds a 33×33 farm and asserts more than one soil type appears. With patches larger than the farm that can fail, so it should use a fixed seed known to produce variety, or test the generator on a larger grid.

### 6.8 Save format changes

- `SavedTile` / `SoilState`: a soil id string (legacy `"Sandy"` / `"Loamy"` / `"Clay"` map to the new ids on load), `water_mm` in place of `moisture` with a load-time mapping from old values rather than discarding existing saves, and the tile's field assignment.
- A `FieldRegistry` snapshot (fields plus the next id counter) in `SaveGame`.
- `SavedAction`: the target (a tile coordinate or a field id), the visited-tile list, and `water_depth_mm`. For old saves, map `water_intensity` 1.0 to 20 mm and 0.5 to 10 mm, with serde defaults for missing fields.
- `FarmConfig`: the noise seed, with a serde default.

### 6.9 Irrigation scope

For now, irrigation is a `Water` action targeting a field with a depth, applied to the whole field. Real rigs differ — a center pivot covers a circle, a wheel line covers one row at a time — and their coverage does not follow field boundaries. Keep `TileOrField` resolving to "a set of tiles in an order" without assuming the target is a field, so a coverage-shaped target can be added later. Equipment capacity measured in volume (mm × tiles) rather than tiles would also make a deep soak cost more throughput than a light one, and suits those rigs; deferred.

### 6.10 Stages and tests

Each stage is testable on its own:

1. **Soil definitions.** `SoilDef` JSON, roster and sprites, with behavior identical to today; save migration for the soil id.
2. **Water model.** `water_mm`, drainage, runoff, derived available/air values, the new crop schema and stress function, the `Water` action in millimetres, soil evaporation with temperature scaling, the inspector, the `yield_factor` update, the `validate_crop_defs` update, and conversion of the four starter crops.
3. **Transpiration.** `daily_water_use_mm` and per-stage factors, including the dead-crop check.
4. **Soil distribution.** Noise generation and the soil-type overlay.
5. **Fields.** Data model, registry, paint tool and boundary overlay first; then `TileOrField` queue targets with visited-set semantics.

Two existing tests reference `soil.moisture` directly and must be rewritten in stage 2: `moisture_stays_within_bounds_over_many_days` becomes "water stays between wilting point and saturation", and `rain_contribution_shrinks_as_moisture_approaches_saturation` becomes "rain on saturated soil adds nothing". New tests should cover drainage by soil type, runoff at saturation, ET scaling with temperature, dead crops using no water, tractor order (including parity from the global row), the visited-set completion rule for both state-derived and amount-adding actions, and deleted-field handling.

### 6.11 Open questions

- Name for the field-assignment tool pair ("zone" / "release"?) — see the `Clear` collision in §6.5.
- Whether waterlogging needs a duration counter (§6.4); decide after playtesting.
- Fixed `ROOT_ZONE_DEPTH_MM` versus root depth by crop or growth stage.
- Refinements deliberately left out of the first version: ET tapering as soil dries, canopy shading of soil evaporation, soil-dependent infiltration limits (clay absorbing rain more slowly), and capacity measured in volume.
- Whether world-generation proportions of soil types should be configurable per farm.
- Presentation of the soil-type and field-boundary overlays.
- Tractor-order row numbering and Bevy's y-up coordinates — confirm the convention when implementing.

---

*Companion to the requirements doc — update both together as design decisions solidify during prototyping.*
