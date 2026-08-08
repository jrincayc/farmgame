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

`SoilState.fertility` was previously undefined behavior — the day-advance diagram (§1.1) named `farm::degrade_soil` but no formula existed, and the prototype spike left fertility static. This fills that gap.

**While a tile has a `PlantedCrop`:** each `DayAdvanced`, that tile's `SoilState.fertility` changes by `def.fertility_impact_per_day` (§2.4). Positive values (most crops) drain the soil; negative values (legumes, cover crops) replenish it. This is the same "declare tolerance/impact as data, let outcomes emerge from comparison" approach already used for climate suitability (§4.3a of the requirements doc) — crop rotation isn't a system to build, it's a consequence of players sequencing crops with complementary `fertility_impact_per_day` values.

**While a tile is fallow (no `PlantedCrop`):** fertility drifts back toward a baseline (e.g. 0.6) at a rate depending on `SoilType` (§2.3) — e.g. Loamy regenerates fastest, Sandy slowest. This is the first real use of `SoilType` beyond being an inert label.

```rust
fn degrade_soil(
    mut tiles: Query<(&mut SoilState, Option<&PlantedCrop>)>,
    crop_defs: Res<Assets<CropDef>>,
) {
    for (mut soil, planted) in &mut tiles {
        let delta = match planted {
            Some(crop) => crop_defs.get(&crop.def)
                .map(|def| def.fertility_impact_per_day)
                .unwrap_or(0.0),
            None => soil.soil_type.fallow_regen_rate(), // e.g. Loamy: +0.01, Sandy: +0.003
        };
        soil.fertility = (soil.fertility + delta).clamp(0.0, 1.0);
    }
}
```

Runs on `DayAdvanced`, ordered before `tick_growth` per §1.1's existing chain (fertility for the day should be settled before growth reads it).

Exact regen rates and `fertility_impact_per_day` values per crop are a playtesting question (§5), same as the quality-threshold and equipment-capacity numbers already flagged there.

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
    pub water_requirement: f32,
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
    "water_requirement": 0.6,
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

```rust
#[derive(Resource)]
pub struct Wallet {
    pub money: u32,
}

#[derive(Resource)]
pub struct Market {
    pub price_modifiers: HashMap<String, f32>, // crop id -> time_factor multiplier; all 1.0 for MVP
}

pub fn sale_value(crop_id: &str, base_sell_price: u32, quality: QualityGrade, market: &Market) -> f32 {
    let quality_multiplier = quality.multiplier();                       // Poor 0.5 / Good 1.0 / Premium 1.5
    let time_factor = market.price_modifiers.get(crop_id).copied().unwrap_or(1.0); // fixed 1.0 for MVP
    base_sell_price as f32 * quality_multiplier * time_factor
}
```

`Market::price_modifiers` exists as a real field from day one, just pinned to `1.0` everywhere for MVP. This is deliberate: the **stretch-goal futures market / variable pricing** (§7 of the requirements doc) becomes "make something else write into this map over time," not a new field or a rewrite of `sale_value`.

### 2.6 Equipment & Purchasable Structures (Resource + Component)

```rust
#[derive(Resource)]
pub struct Equipment {
    pub daily_capacity: HashMap<ActionKind, u32>, // modern machinery, e.g. Till (tractor plough) -> 80, Harvest (combine) -> 120
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionKind { Till, Plant, Water, Fertilize, Harvest }

// Stretch: purchasable structures (e.g. greenhouses) placed on tiles
#[derive(Component)]
pub struct Structure {
    pub kind: String,       // e.g. "greenhouse"
    pub tiles_covered: Vec<IVec2>,
}
```

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
    - client-side check: tile has SoilState, no existing PlantedCrop, player has seed_cost in Wallet
      (a soft check for player feedback — the real check happens at resolution time, §3.1a)
    - push PendingAction { tile, kind: Plant, crop_to_plant: Some(handle) } onto ActionQueue.pending
```

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

```rust
fn tick_growth(
    mut crops: Query<(Entity, &mut PlantedCrop, &Tile)>,
    soils: Query<&SoilState>,
    region: Res<Region>,
    calendar: Res<Calendar>,
    crop_defs: Res<Assets<CropDef>>,
) {
    for (entity, mut crop, tile) in &mut crops {
        if crop.dead {
            continue; // stays on the tile until the player clears it - see §2.4
        }

        let def = crop_defs.get(&crop.def).unwrap();
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

        // Overripening: only applies once a crop has completed its final
        // stage and sat there past optimal_harvest_window_days. Feeds the
        // exact same accumulators as temperature stress - "left to rot" is
        // not a separate mechanic, it's another source of daily stress.
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
        let today_stress = temp_stress + overripe_days as f32 * def.overripe_stress_per_day;

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
    sale_value_per_unit = sale_value(def.id, def.sell_price, quality, &market)   // §2.5
    → add `yield_amount` units of crop.id (tagged with `quality`) to player Inventory
    → despawn PlantedCrop component from tile (soil remains)
```

Harvesting late (after `optimal_harvest_window_days` has elapsed since maturity) isn't blocked — it's already reflected in `worst_single_day_stress` and `cumulative_stress` by the time the player harvests, per §3.2. A tomato left two extra days will simply grade lower (or die) by the time it's picked; there's no separate "too late to harvest" rule to implement.

`yield_factor(...)` and `quality_grade(...)` are two separate pure functions, deliberately not combined into one "overall harvest quality" score — yield is forgiving and cumulative (rewards mostly-good care), quality is spiky and threshold-based (punishes the single worst moment). Keeping them as two functions with two different shapes is what makes each legible on its own; a player looking at a "full yield, Poor quality" harvest can reason directly to "yield's fine, but something spiked hard once."

### 3.4 Weather (on `DayAdvanced`, before growth tick)

MVP weather is simple: roll a bounded random value per day (affecting `SoilState.moisture` deltas), check `Region.frost_days` for frost risk. No separate weather "system" beyond a `WeatherToday` resource that other systems read.

```rust
#[derive(Resource)]
pub struct WeatherToday {
    pub rainfall: f32,      // 0.0–1.0, added to soil moisture that day
    pub is_frost: bool,
}
```

---

## 4. What's Deliberately Deferred (per stretch goals)

- **Conditional scheduling (§4.7):** distinct from the MVP action queue (§2.7) — a future `ScheduledAction` component (`{ trigger: Condition, action: ActionKind }`) would be checked in the same `DayAdvanced` pass and, when triggered, simply push a `PendingAction` onto the existing `ActionQueue` rather than bypassing it. So scheduling builds *on top of* the queue instead of needing a separate resolution path.
- **Equipment upgrades:** `Equipment::daily_capacity` (§2.6) is written as a `HashMap` specifically so a purchase can just increase a value in it — no structural change needed when upgrades are added.
- **Greenhouses (§4.3a):** `TileClimateModifier` is defined in §2.3, and `Structure` (§2.6) is shaped to spawn one per covered tile, but neither is wired into a purchase flow or the growth-tick lookup yet. Growth tick already has the comment marking where it plugs in.
- **Futures market / variable pricing (§4.5):** `Market::price_modifiers` (§2.5) already exists and is read by `sale_value()`; only something writing non-1.0 values into it needs to be built.
- **Multi-region:** `Region` as a singleton `Resource` is an MVP simplification; the `temp_on()` function itself doesn't assume singleton-ness, so multi-region later means changing *how Region is looked up* (per-farm entity vs. global resource), not the climate math.

---

## 5. Open Implementation Questions

- Asset hot-reloading: Bevy supports this out of the box for `Asset` types — worth enabling during development so crop `.json` tweaks don't require recompiling, given how central iterating on crop data will be to balancing.
- Tile grid size for MVP: now targeting thousands of tiles rather than hundreds (§5 non-functional target, driven by modern-equipment scale — §2.6), specifically so equipment capacity is a real constraint. A `HashMap<IVec2, Entity>` lookup is still fine at that scale in Bevy — this isn't a "need spatial partitioning" threshold, just worth confirming during prototyping that per-tile queries (e.g. `tick_growth` iterating every `PlantedCrop`) stay comfortably within the frame budget at a few thousand entities, since that wasn't validated at the smaller original target.
- Exact multiplier values for quality grades (0.5x/1.0x/1.5x) and the stress thresholds themselves are placeholders — real values only make sense after playtesting a few crops.
- Same for `Equipment::daily_capacity` starting values and `TimerSpeed` real-time durations — both playtesting questions, not architecture ones (mirrors the open questions in the requirements doc).
- **Resolved:** `ActionQueue` player reordering/cancellation (requirements doc §9) — `PendingAction` carries a stable `id` from the start (§2.7), so the data model supports this even though the MVP UI itself is FIFO-display-only.
- **Resolved:** grid size (requirements doc §4.2, §9) — `FarmConfig { width, height }` (§2.3) is a resource, not a compiled-in constant, bounded by `FarmConfig::MIN_DIMENSION`/`MAX_DIMENSION`, with an MVP default of 64×64. A player-facing size picker is stretch UI on top of the same resource.
- **Resolved:** Bevy version — pinned to `=0.16.0` exactly, with `bevy_egui = "=0.34.1"` as the matching UI integration version.
- Asset path resolution: Bevy's default `AssetPlugin` looks for files under a single root (`target/<profile>/assets/`), with no built-in multi-location fallback. Confirmed during the prototype spike that this trips people up (assets copied to the project root aren't found until also copied under `target/debug/assets/`). **Resolved for now:** option (a), a `build.rs` that copies `assets/` into `target/<profile>/assets/` automatically on every build — implemented in Phase 1. Option (b), a custom `AssetSource`/`AssetReader` trying multiple candidate roots, remains a possible future upgrade once install-location behavior matters for a release build.

---

*Companion to the requirements doc — update both together as design decisions solidify during prototyping.*
