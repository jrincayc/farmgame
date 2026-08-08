# Farm Sim (working title)

A farming simulation, built solo in Bevy (Rust) as both a shippable
game and a Rust/Bevy learning project. See
`farming-game-design-doc.md` and `farming-game-requirements.md` for
the full design.

## Build & run

```sh
cargo check   # fast type-check without producing a binary
cargo run     # build + run the dev binary
cargo test    # run unit tests (currently: time module)
```

Requires the exact-pinned Bevy `0.16.0` and `bevy_egui` `0.34.1` from
`Cargo.toml` — don't `cargo update` these without a deliberate version
bump + review, since Bevy's API drifts significantly between minor
versions.

## Current status: Phase 1 (time skeleton)

No rendering of the farm yet. `cargo run` opens a mostly-empty window;
the time/calendar system is wired and drivable via temporary keyboard
controls (see console output on launch) so `DayAdvanced` firing can be
verified before climate/farm/crops build on top of it.

Implementation proceeds in phases per the agreed plan:

1. **Time skeleton** *(current)* — `Calendar`, `TimeControl`, `DayAdvanced`
2. Climate + weather
3. Farm tiles + soil
4. Crops (data-driven assets, growth tick, action queue)
5. Economy
6. UI (bevy_egui, light/dark toggle)
7. Save/load
8. Polish + balancing

## Project layout

```
src/
├── main.rs
├── time/          # day counter, DayAdvanced event, TimeControl
├── climate/       # (Phase 2)
├── farm/          # (Phase 3)
├── crops/         # (Phase 4)
├── economy/       # (Phase 5)
└── ui/            # (Phase 6)
assets/
└── crops/         # crop definition JSON files (Phase 4+)
build.rs           # copies assets/ into target/<profile>/assets/
```
