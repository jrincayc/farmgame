# Farm Sim (working title)

A farming simulation, built solo in Bevy (Rust) as both a shippable
game and a Rust/Bevy learning project. See
`farming-game-design-doc.md` and `farming-game-requirements.md` for
the full design.

## Build & run

```sh
cargo check   # fast type-check without producing a binary
cargo run     # build + run the dev binary
cargo test    # run unit tests (time, farm modules)
```

Requires the exact-pinned Bevy `0.16.0` and `bevy_egui` `0.34.1` from
`Cargo.toml` — don't `cargo update` these without a deliberate version
bump + review, since Bevy's API drifts significantly between minor
versions.

## Current status: Phase 4 (crops + action queue)

No rendering of the farm yet, and no real UI (that's Phase 6). `cargo
run` opens a mostly-empty window; time/climate/farm/crops are all wired
and drivable via temporary keyboard controls (see console output on
launch) so the full plant → grow → harvest loop can be exercised and
verified before the real UI replaces these dev controls.

**Compile note:** the Phase 4 batch was packaged without running
`cargo check` (per a request to skip compiling in the sandbox, since
it's slow there) — `src/crops/crop_def.rs`'s `CropDefLoader` is the
highest-risk file if something doesn't build; see the doc comment on
that struct for the likely fixes.

Implementation proceeds in phases per the agreed plan:

1. **Time skeleton** — done. `Calendar`, `TimeControl`, `DayAdvanced`
2. **Climate + weather** — done. `Region`/`TempCurve`, `WeatherToday`, two-stage rainfall roll
3. **Farm tiles + soil** — done. `FarmConfig`, `Tile`/`SoilState`/`SoilType`, `degrade_soil`
4. **Crops** *(current)* — data-driven `CropDef` assets, growth tick, action queue, starter roster (Tomato, Wheat, Carrot, Green Bean)
5. Economy (Market, sale_value, selling flow — Wallet/Inventory already stubbed in Phase 4)
6. UI (bevy_egui, light/dark toggle)
7. Save/load
8. Polish + balancing

## Project layout

```
src/
├── main.rs
├── time/          # day counter, DayAdvanced event, TimeControl
├── climate/       # region temp curve, weather roll
├── farm/          # tile grid, soil state, fertility/moisture
├── crops/         # crop assets, growth tick, action queue, equipment
│   ├── crop_def.rs   # CropDef asset + JSON loader, quality/yield functions
│   ├── growth.rs     # PlantedCrop, tick_growth (daily stress + stage advance)
│   ├── queue.rs       # ActionKind, ActionQueue, process_action_queue
│   └── equipment.rs   # Equipment (daily capacity per action kind)
├── economy/       # Wallet, Inventory (stub - full Market is Phase 5)
└── ui/            # (Phase 6)
assets/
└── crops/         # crop definition JSON files: tomato, wheat, carrot, green_bean
build.rs           # copies assets/ into target/<profile>/assets/
```
