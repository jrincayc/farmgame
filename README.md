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

## Current status: Phase 6 (UI — egui panels + placeholder tile rendering)

`cargo run` now opens a window with a real UI: a top bar (day/season,
time control, theme toggle), a left panel (action queue), a right panel
(selected-tile inspector), and a bottom bar (wallet/inventory/sell).
The farm itself renders as a placeholder grid — flat colored squares
with a crop-initial text label per tile — clickable to select a tile;
this is explicitly not the real art pipeline (see the design doc's §10
note in the requirements doc), just enough for tile-targeted UI to be
meaningful. Dev keyboard controls from Phases 4–5 still work too.

Implementation proceeds in phases per the agreed plan:

1. **Time skeleton** — done. `Calendar`, `TimeControl`, `DayAdvanced`
2. **Climate + weather** — done. `Region`/`TempCurve`, `WeatherToday`, two-stage rainfall roll
3. **Farm tiles + soil** — done. `FarmConfig`, `Tile`/`SoilState`/`SoilType`, `degrade_soil`
4. **Crops** — done. Data-driven `CropDef` assets, growth tick, action queue, starter roster (Tomato, Wheat, Carrot, Green Bean)
5. **Economy** — done. `Market`/`sale_value`, instant sell flow (`SellRequested` event, not queued)
6. **UI** *(current)* — `bevy_egui` panels (time/theme/queue/inspector/economy), placeholder tile rendering + click-to-select
7. Save/load
8. Polish + balancing

*(Rendering/art pipeline — real sprites, growth-stage art, soil overlays, camera pan/zoom — is a newer, not-yet-numbered phase; see requirements doc §10.)*

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
├── economy/       # Wallet, Inventory, Market, sale_value, instant sell flow
│   ├── market.rs     # Market resource + sale_value() (design doc §2.5)
│   └── sell.rs        # SellRequested event + process_sell_requests
└── ui/            # egui panels + placeholder tile rendering (Phase 6)
    ├── mod.rs        # UiPlugin, SelectedTile/UiTheme resources
    ├── render.rs      # camera, placeholder tile sprites/labels, click-to-select
    └── panels.rs      # top bar, tile inspector, action queue, economy panels
assets/
└── crops/         # crop definition JSON files: tomato, wheat, carrot, green_bean
build.rs           # copies assets/ into target/<profile>/assets/
```
