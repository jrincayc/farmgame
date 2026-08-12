# Farming Simulation Game — Requirements Document

**Status:** Draft v1
**Target platform:** Linux desktop, built with cross-platform tools/language
**Genre:** Simulation-leaning farming game
**Audience/dev context:** Some coding experience, first game project

---

## 1. Vision

A farming game where the *systems* are the draw — soil, weather, crop growth, and resource management behave with enough depth that planning and optimizing feels meaningful, rather than just a cosmetic loop of "plant, wait, harvest." Closer to Sim Farm's economy/land-management simulation than a character-driven cozy farming game like Stardew Valley.

## 2. Core Pillars

These are the priorities that should guide every scope decision below. When two features conflict, whichever serves these pillars more wins.

1. **Systemic depth over content volume** — fewer crops/mechanics, modeled well, beats many crops modeled shallowly.
2. **Player agency through information** — the player should be able to learn the rules and predict outcomes (soil quality affects yield in a consistent, discoverable way), not be at the mercy of hidden RNG.
3. **Respect the player's time** — simulation doesn't mean tedium; ticks and automation should reduce busywork, not add it.

## 3. Tech Stack

**Chosen: Bevy (Rust).**

Bevy is a strong fit for this project specifically because of its ECS (Entity Component System) architecture — a farm tile, a crop, and soil state are naturally separate components, and simulation logic (growth, watering, soil degradation) becomes independent, testable systems that run over queries. That maps cleanly onto "systemic depth" being the core pillar.

Things to keep in mind given "some coding experience, new to games":
- Rust's ownership model is a real learning curve if you haven't used it before — budget time for that separately from "learning game dev."
- Bevy is cross-platform (Linux/Windows/Mac) and open source, so the platform requirement is satisfied by default.
- Bevy's ecosystem moves fast between versions; pin a version early and expect some API drift if you update later.
- Bevy doesn't hand you a built-in UI/editor the way Godot does — expect to use a crate like `bevy_egui` or Bevy's native UI for menus/HUD, and to build tooling (e.g., a crop data viewer) yourself if you want one.

**Data format: JSON**, used consistently for both crop data files and save files. Bevy's asset system doesn't require RON specifically — a custom `AssetLoader<CropDef>` backed by `serde_json` works the same way. Using one format everywhere (rather than RON for assets and something else for saves) keeps tooling simple: one parser, one set of editing/validation habits, and save files stay human-readable/diffable for debugging.

## 4. Functional Requirements — Core Systems

### 4.0 UI Appearance

- The UI (§4.6, bevy_egui) must support both a Light and a Dark visual theme, toggleable by the player at any time. Neither is a fallback for the other — both are first-class supported appearances (some players strongly prefer one, some the other). Default theme TBD, likely Light.

### 4.1 Time & Calendar
- Discrete day counter — no day/night cycle; a "day" is the atomic unit of time
- **Time control has three modes:**
  - **Paused:** simulation frozen; player can freely queue actions (see §4.6a) without anything resolving
  - **Step:** advance exactly one day, then return to Paused
  - **Timer:** day advances automatically on a real-time countdown, at a selectable speed (e.g., slow/normal/fast — minutes-per-day at each speed TBD via playtesting)
- Regardless of mode, a "day" only ever advances as one discrete event (`DayAdvanced`) — Timer mode just changes what *triggers* that event (a countdown) rather than changing how it's processed
- A year length (in days) is defined; the region's temperature curve (see 4.3a) runs across it
- "Seasons" (Spring/Summer/Fall/Winter) can still exist as a display/UI convenience — a rough label derived from the temperature curve — but they are not the mechanism crop viability is checked against; the underlying temperature values are
- No real-time render loop needed for simulation *state* — rendering/input still run every frame as usual, but soil/crops/weather/economy only change on `DayAdvanced`

### 4.2 Land & Soil
- Grid-based farmland (tiles). Grid dimensions are a **player-chosen parameter at world creation**, not a fixed constant — within a min/max bound (see §5 non-functional target and §9). This replaces an earlier assumption of a single fixed grid size; the MVP still ships with a sensible default so this doesn't block early implementation, but the underlying data (farm width/height) should be a resource read at world-gen time, not a compiled-in constant, so the player-facing size picker (stretch UI, if not MVP) doesn't require restructuring later.
- Soil quality attributes: fertility, moisture, possibly type (e.g., sandy/loamy/clay)
- Tilling, fertilizing, and soil degradation/regeneration over time
- Fertility changes over time based on what's planted (or not planted) on a tile — heavy feeders drain it, cover crops/legumes replenish it, fallow tiles slowly regenerate. Crop rotation is intentionally not a named system — it should fall out of these per-crop values the same way growing-season suitability falls out of comparing crop tolerance to regional climate (§4.3a). See design doc §2.3a for the mechanism.
- Irrigation or manual watering as a moisture mechanic

### 4.3 Crops

**Requirement: crops must be data-driven, not hardcoded.** Adding a new crop should mean adding a new JSON data entry, not writing new code or branching logic. This is a core architectural requirement, not just a nice-to-have — it directly serves the "systemic depth" pillar, since it's what makes balancing and expanding the crop roster cheap.

**Design note on climate:** crop-to-season suitability should be *derived*, not stored as a static list. A crop doesn't inherently belong to "spring" — it tolerates a range of conditions, and whether that range lines up with spring, summer, or twice a year depends on the region's climate. Storing `valid_seasons: [Spring]` on the crop would hardcode an assumption that breaks the moment you add a second climate/region, and would require new season-like values for new locations. Instead, crops declare their tolerance, and the *region* (see 4.3a) declares its conditions over time; suitability and harvest-cycles-per-year fall out of comparing the two.

**Design note on yield vs. quality:** these are deliberately two separate outcomes, not one blended "how well did it grow" score. Yield tracks *cumulative* average conditions across the growing cycle (chronically mediocre care → lower yield). Quality is driven by the single *worst* day of stress (one bad frost night → capped quality, even if every other day was great). This mirrors how real crops behave, and it gives the player two distinct things to read and optimize for — see the design doc for the implementation split. Overripening (leaving a mature crop unharvested past its optimal window) is not a distinct failure mode from temperature stress — a crop left too long accrues stress through the exact same accumulators, it's just a different *source* of stress, keeping the growth-tick logic generic rather than adding a parallel "ripeness" system.

Suggested crop data schema (fields to define per crop):

| Field | Purpose |
|---|---|
| `id` / `name` | Identifier and display name |
| `growth_stages` | List of stages (e.g., sprout → growing → mature), each with a day-duration |
| `min_temp` / `max_temp` | Temperature range the crop can grow in at all |
| `ideal_temp_range` | Narrower range for full/ideal yield (yield tapers outside this, fails outside min/max) |
| `frost_tolerant` | Whether the crop survives a frost event |
| `water_requirement` | **Superseded (Phase 4):** see `min_moisture`/`max_moisture`/`ideal_moisture_range` below. A single scalar only modeled under-watering; over-watering (waterlogging, root rot) is a real failure mode too, so moisture is now specified the same way temperature is — an absolute viable range plus a narrower ideal range. |
| `min_moisture` / `max_moisture` | Absolute moisture range (on `SoilState.moisture`'s 0.0–1.0 scale) the crop survives at all |
| `ideal_moisture_range` | Narrower range for full/ideal yield and quality; tapers/penalizes outside it the same way `ideal_temp_range` does |
| `fertility_requirement` | Minimum soil fertility to grow well |
| `base_yield` | Amount produced at harvest under ideal conditions |
| `yield_modifiers` | How cumulative (average, ongoing) conditions scale total yield quantity |
| `quality_thresholds` | How the single worst day of stress determines produce quality (Poor/Good/Premium), which multiplies `sell_price` |
| `sell_price` | Base market value (before quality multiplier) |
| `seed_cost` | Cost to purchase seed |
| `optimal_harvest_window_days` | How long a crop can sit at full maturity before it starts overripening |
| `overripe_stress_per_day` | Stress added per day past that window — feeds the same yield/quality accumulators as temperature stress, not a separate mechanic |
| `fertility_impact_per_day` | How much this crop drains (positive) or replenishes (negative) tile fertility each day it's planted — see §4.2 |

Implementation note for Bevy: this maps well onto Bevy's asset system — crops as an `Asset` type loaded from data files, with a `Crop` component on planted-tile entities referencing the asset. Growth/yield logic becomes generic systems that read from whatever crop asset is referenced, rather than per-crop code paths.

- Planting, growth ticking (on day-advance, not real-time), harvesting
- Failure states (crop dies from neglect, wrong temperature/season, frost, etc.) — also driven by the same data, not special-cased per crop

### 4.3a Region & Climate

To make crop suitability emergent rather than declared, the farm's location needs its own simple climate model:

- Each region/save defines a **temperature curve across the year** (e.g., a value per day or per season-block, could start as a simple sine-like curve with a min/max and could later support presets like "temperate," "tropical," "cold")
- Optionally, a **frost calendar** — days where frost risk is possible, relevant to `frost_tolerant`
- On planting, the game checks the crop's temperature range against the upcoming forecasted days (do conditions stay viable long enough for `growth_stages` to complete?) rather than checking against a fixed season label
- This naturally allows a warm region to fit two growing windows for a fast crop in a year, while a cold region fits one or none, **without any special-casing in the crop or region code** — it's just a consequence of comparing two data curves
- **Tile-level climate modifiers:** structures like greenhouses apply a local offset/clamp to the region's base temperature curve for the tiles they cover (e.g., "+8°C, never below 10°C"). This should be modeled as a modifier applied on top of the base curve — the same comparison logic (crop tolerance vs. effective conditions) still applies, just against a modified curve rather than the raw regional one. Keeps the "no special-casing" property intact rather than needing a separate "is this a greenhouse tile" branch in the growth-check code.
- **Purchasable:** greenhouses (and other tile-climate structures) are bought with in-game money and built onto chosen tiles, using the same purchase flow as equipment (§4.5) — not unlocked for free or via progression.

This is more setup than a flat season enum, but it's what actually earns "data-driven" as a real property of the system rather than just a file-format detail.

### 4.4 Weather
- Randomized but bounded weather per day (affects moisture, growth rate)
- Seasonal weather patterns (frost risk in fall/winter, drought risk in summer)

### 4.5 Economy
- Selling harvested goods: for MVP, price is **static per crop but formula-shaped**: `sale_value = sell_price * quality_multiplier * time_factor`. `quality_multiplier` comes from the harvest's quality grade (§4.3); `time_factor` is fixed at `1.0` for MVP but exists as a real term in the formula so variable/seasonal pricing later (stretch) is a matter of changing what feeds that term, not restructuring the sale calculation.
- Buying seeds, tools/equipment, and structures (e.g., greenhouses) — a single purchase flow: cost deducted from `Wallet`, item added to player's owned equipment or built onto a chosen tile
- Basic money/inventory tracking

**Stretch (post-MVP):** a futures market and genuinely variable/fluctuating sell prices (supply-and-demand style, similar to Sim Farm's economic layer) — see §7.

### 4.6 Player & Farm Management
- Player movement and interaction with tiles/tools
- Inventory system
- Save/load game state

### 4.6a Equipment & Action Queue

This is the mechanic that makes "Paused" mode meaningful rather than just a menu state:

- While paused (or at any time), the player can **queue** actions — plant, water, fertilize, harvest — on any number of tiles/fields, without them resolving immediately
- Equipment is **modern (~2000s USA) farm machinery**, not gardening hand tools — tractor-pulled ploughs, planters, sprayers/irrigation rigs, combine harvesters. This is a deliberate scale choice (see below), and it also means starting equipment is already fairly capable; upgrades (stretch, §7) are about bigger/faster machinery, not a jump from "hand tools" to "machinery"
- Each **equipment type** defines a **daily capacity**: the max number of tile-actions of its kind it can perform in a single day-advance (e.g., a starting tractor-pulled plough tills ~80 tiles/day; a starting combine harvester harvests ~120 tiles/day — illustrative, to be tuned)
- **Farm size must be large enough that this throughput is meaningful.** Real combine harvesters clear hundreds of acres a day — if the whole farm is only a "few hundred tiles" (the original non-functional target), a single day of harvesting empties it and the capacity limit never actually constrains anything. MVP farm size is being scaled up specifically so equipment throughput stays a real planning constraint (see §5, §6)
- On `DayAdvanced` (whether triggered by Step or by Timer), the game processes queued actions up to each action type's capacity limit, in queue order; anything beyond capacity simply carries over and resolves on the next day-advance
- This makes equipment a real throughput constraint rather than a cosmetic upgrade — a larger farm genuinely needs better equipment, not just more player clicking, which reinforces "respect the player's time" (§2) by letting them plan a day's work in one pass while paused rather than micromanaging tile-by-tile in real time
- MVP ships with one fixed, default equipment set (fixed capacities, not purchasable); purchasable equipment upgrades that increase capacity are a stretch goal (§7)

### 4.7 Scheduling & Automation

Distinct from the action queue in §4.6a: the queue handles "do these specific actions soon, throughput-limited," while scheduling handles "do this action automatically when some future condition is met" — a fuller automation layer, and out of MVP scope.

- Actions (harvest, planting, watering, fertilizing) should eventually support two modes:
  - **Manual/queued (MVP, §4.6a):** player queues a tile/field action; it resolves as soon as equipment capacity allows
  - **Scheduled (stretch):** player sets a standing rule ahead of time — e.g., "harvest field 3 whenever a crop is mature" or "harvest on day N" — that keeps re-triggering automatically, rather than being a one-off queued action
- Scheduled rules should be defined per-field (or per-tile-group) rather than globally, so different parts of the farm can run on different plans
- This is a natural fit for day-advance-driven simulation (§4.1): scheduled rules can just be checked as part of the "on day advance" system pass, alongside growth/weather updates and the action queue
- Worth deciding later whether scheduling conditions are limited to a fixed set (maturity reached, specific day) or become a small rule system (condition + action pairs) — starting with the fixed set keeps scope sane, but the underlying design (checked each day-advance) should not preclude expanding it later

## 5. Non-Functional Requirements

- **Cross-platform:** must build and run on Linux; should not use Linux-only or Windows-only dependencies
- **Performance:** target smooth simulation of a farm of realistic size for modern equipment (on the order of a few thousand tiles, sized so default equipment capacity §4.6a takes multiple days to clear the farm — not "a few hundred," which real machinery would clear in a single day) without frame drops
- **Persistence:** reliable save/load with no data corruption on crash
- **Maintainability:** systems should be modular (e.g., a "Crop" resource/class independent of rendering) so new crops/mechanics can be added without rewrites

## 6. MVP Scope (suggested first milestone)

To keep this achievable as a first game project:
- 1 farm plot area, grid-based — sized in the thousands of tiles (see §5), not hundreds, so modern equipment (§4.6a) doesn't trivially clear it in a day
- 3–4 crops, in one region climate (just enough to prove the temperature-driven viability system) — **Resolved (Phase 4):** Tomato, Wheat, Carrot, and Green Bean (the roster's legume — its negative `fertility_impact_per_day` is what demonstrates soil-replenishing crop rotation as an emergent property of the data, per §4.2, rather than a named system)
- Basic soil moisture + fertility simulation
- Simple single-season day-counter loop (no day/night)
- Time control: Paused / Step / Timer (fixed set of speeds)
- Action queue (§4.6a) with one fixed, non-purchasable default equipment set
- Plant → grow → harvest → sell loop, with static-formula sell pricing (quality-multiplied, `time_factor` fixed at 1.0)
- Manual save/load (JSON)

Explicitly **out of scope for MVP:** NPCs/relationships, multiplayer, livestock, combat, crafting, multiple seasons/years, weather beyond basic rain, conditional/rule-based scheduling (§4.7), purchasable equipment upgrades, greenhouses/tile climate modifiers (§4.3a), futures market/variable pricing.

## 7. Stretch Goals (post-MVP)

- Conditional/rule-based scheduling (§4.7) — action queue only for MVP
- Purchasable equipment upgrades (increased daily action capacity, §4.6a)
- Greenhouses and other tile-level climate modifiers (§4.3a) — MVP uses the region's base climate only
- Futures market and variable/fluctuating sell prices (§4.5) — static formula-based pricing for MVP
- Additional seasons and year-over-year progression
- Livestock
- Weather events (drought, frost) with strategic tool counters
- NPCs or light narrative layer

## 8. Project Context

- **Team:** Solo project.
- **Inspiration:**  — a systems/economy-driven farm simulator (weather, land, market pricing) rather than a character/relationship-driven farm game. This reinforces the pillars in §2: depth of simulation over breadth of content or narrative.

## 9. Open Questions

- What are reasonable default equipment capacities for MVP (tiles/day for till, water, fertilize, harvest), and what farm-grid size makes that throughput take multiple days rather than one? These two numbers need to be picked together — likely a playtesting question rather than a pure design one. **Phase 4 status:** placeholder starting values are now in code (`Equipment::default()` — Till/Plant 80, Water/Fertilize 150, Harvest 120, Clear 100 per day), matched against the existing 64×64 default farm size. Still unvalidated by actual play.
- **Resolved:** queued actions (§4.6a) should be reorderable/cancelable in principle — `PendingAction`s carry a stable ID from the start (see design doc §2.7) so the underlying data supports this. The UI for reordering/canceling is deferred; MVP UI can ship FIFO-only display while the data model is already ready for it.
- **Resolved:** farm grid size is a player-chosen parameter at world creation, within a min/max bound (§4.2), rather than a fixed constant. MVP still ships with a default size; the picker UI itself may be deferred if it doesn't fit MVP timeline, but the underlying `FarmConfig`-style resource is sized-aware from the start.
- Timer mode speed options: how many speed tiers, and what's the real-time-minutes-per-game-day at each?

---

*This is a living document — expect it to change as we prototype and learn what's actually fun.*
