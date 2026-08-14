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

**Phase 4 addition — `Clear`:** §2.4's dead-crop state ("stays until explicitly cleared by the player") needed an `ActionKind` to resolve through. Given its own capacity bucket rather than folding into `Till`, since clearing debris and tilling a field are different equipment in practice.

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

### 2.8 Save Data (Serialized snapshot)

Rather than trying to serialize live ECS entities directly (fragile across versions), MVP save/load works through an explicit **snapshot struct** that systems populate from/apply to the world, written to disk as JSON via `serde_json::to_string_pretty` / `from_str`:

```rust
#[derive(Serialize, Deserialize)]
pub struct SaveGame {
    pub calendar: Calendar,
    pub wallet: Wallet,
    pub equipment: Equipment,
    pub tiles: Vec<SavedTile>,
}

#[derive(Serialize, Deserialize)]
pub struct SavedTile {
    pub coord: IVec2,
    pub soil: SoilState,
    pub planted_crop: Option<SavedCrop>,
}

#[derive(Serialize, Deserialize)]
pub struct SavedCrop {
    pub crop_id: String,   // re-resolved to a Handle<CropDef> on load via asset server
    pub planted_day: u32,
    pub current_stage: usize,
    pub days_in_stage: u32,
    pub cumulative_stress: f32,
    pub worst_single_day_stress: f32,
}
```

This indirection is deliberate: it means save-file format is decoupled from internal ECS layout, so refactoring components later doesn't corrupt old saves as badly (a stated non-functional requirement). Queued-but-unresolved `ActionQueue` entries are intentionally **not** persisted for MVP — simplest to just require the queue be empty (or accept it's cleared) on save; worth revisiting once scheduling (stretch) exists, since standing rules *would* need to survive a save/load.

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

`stress_factor` tapers to zero as `cumulative_stress` approaches `YIELD_STRESS_DIVISOR` (a §5-style placeholder). `soil_factor` reflects the tile's fertility/moisture *at harvest time*, but is floored at `MIN_SOIL_YIELD_FACTOR` (also a placeholder, e.g. 0.3) rather than being able to zero out yield on its own — a crop that otherwise grew well on mediocre-but-viable soil shouldn't harvest nothing just because the tile wasn't pristine. `yield_amount` is `(def.base_yield as f32 * yield_factor(...)).round() as u32`.

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

**Placeholder tile rendering (`ui/render.rs`):** a fixed 2D camera framed to the default farm size (no pan/zoom - deferred to a future rendering/art-pipeline phase, see requirements doc §10), one flat-colored `Sprite` per tile (color by `SoilType`, darker overlay when tilled, distinct color when the planted crop is dead) plus a `Text2d` label (crop initial C/G/T/W, `.` for tilled-bare, `X` for dead, blank otherwise). Explicitly not the real art pipeline — just enough to make tile-targeted UI meaningful. Click-to-select (left mouse button, ignored if egui itself wants the pointer that frame) writes a `SelectedTile` resource every other panel reads.

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

## 4. What's Deliberately Deferred (per stretch goals)

- **Conditional scheduling (§4.7):** distinct from the MVP action queue (§2.7) — a future `ScheduledAction` component (`{ trigger: Condition, action: ActionKind }`) would be checked in the same `DayAdvanced` pass and, when triggered, simply push a `PendingAction` onto the existing `ActionQueue` rather than bypassing it. So scheduling builds *on top of* the queue instead of needing a separate resolution path.
- **Equipment upgrades:** `Equipment::daily_capacity` (§2.6) is written as a `HashMap` specifically so a purchase can just increase a value in it — no structural change needed when upgrades are added. **Implemented (fixed, non-purchasable) in Phase 4.**
- **Greenhouses (§4.3a):** `TileClimateModifier` is defined in §2.3, and `Structure` (§2.6) is shaped to spawn one per covered tile, but neither is wired into a purchase flow or the growth-tick lookup yet. Growth tick already has the comment marking where it plugs in.
- **Futures market / variable pricing (§4.5):** `Market::price_modifiers` (§2.5) already exists and is read by `sale_value()`; only something writing non-1.0 values into it needs to be built. **Phase 4 note:** `Wallet`/`Inventory` themselves are implemented now as stubs (§2.5). **Phase 5:** `Market`/`sale_value()`/the sell flow are now implemented (instant, not queued — see §2.5's resolved design question); the futures-market stretch goal remains exactly "write non-1.0 values into `price_modifiers`," unchanged by Phase 5's work.
- **Multi-region:** `Region` as a singleton `Resource` is an MVP simplification; the `temp_on()` function itself doesn't assume singleton-ness, so multi-region later means changing *how Region is looked up* (per-farm entity vs. global resource), not the climate math.
- **Real rendering / art pipeline:** Phase 6 shipped only placeholder tile visuals (flat colored squares, text labels) — see requirements doc §10 for the decisions already locked in ahead of that phase (data-driven growth-stage art, layered soil overlays, camera pan/zoom) versus what's still open (overripe-specific art, exact overlay presentation).

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

---

*Companion to the requirements doc — update both together as design decisions solidify during prototyping.*
