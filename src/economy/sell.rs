//! Selling (design doc §2.5, §4/deferred-section update; requirements
//! §4.5) - **Phase 5.**
//!
//! **Resolved design question (left open at the end of Phase 4):**
//! selling does *not* go through `ActionQueue`/`Equipment` capacity.
//! Equipment capacity models physical fieldwork on tiles (tilling,
//! planting, harvesting) - selling already-harvested goods sitting in
//! `Inventory` isn't fieldwork, it's a transaction against a market, and
//! it has no tile to target in the first place. Harvesting is already
//! capacity-limited (`ActionKind::Harvest`), so goods don't reach
//! `Inventory` for free - the "respect the player's time" pillar (design
//! doc §2 pillar 3) is already served upstream of the sale itself.
//! Selling is therefore instant: fired via `SellRequested`, resolved the
//! same frame, not gated on `DayAdvanced`.
//!
//! **Sells all lots of a crop at once, not per-lot/per-quality.**
//! Selling individual lots (e.g. holding Premium stock, dumping Poor
//! stock) is a plausible stretch, but MVP doesn't need the extra
//! UI/complexity. `sell_crop` is written so a later per-lot sell is
//! additive - it would mean filtering which lots get drained before
//! calling roughly this same per-lot valuation loop, not a rewrite.

use bevy::prelude::*;

use super::market::{sale_value, Market};
use super::{HarvestedLot, Inventory, Wallet};
use crate::crops::CropDef;

/// Fired (e.g. from a future UI sell button, or dev controls for now)
/// to request selling every harvested lot currently held for one crop
/// id. Not part of `ActionQueue` - see module doc for why.
#[derive(Event, Debug, Clone)]
pub struct SellRequested {
    pub crop_id: String,
}

pub struct SellPlugin;

impl Plugin for SellPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Market>()
            .add_event::<SellRequested>()
            .add_systems(Update, process_sell_requests);
    }
}

/// Reacts to `SellRequested`, same frame, independent of `DayAdvanced` -
/// selling is a market transaction, not a day-ticked simulation step.
fn process_sell_requests(
    mut events: EventReader<SellRequested>,
    mut inventory: ResMut<Inventory>,
    mut wallet: ResMut<Wallet>,
    market: Res<Market>,
    crop_defs: Res<Assets<CropDef>>,
) {
    for ev in events.read() {
        let Some(lots) = inventory.harvested.remove(&ev.crop_id) else {
            warn!("SellRequested for '{}' with no harvested lots, ignoring", ev.crop_id);
            continue;
        };

        let Some(def) = find_crop_def(&crop_defs, &ev.crop_id) else {
            // Asset not resolvable (shouldn't happen if the crop was
            // ever successfully harvested, since harvest already needed
            // the def loaded) - put the lots back rather than silently
            // destroying inventory the player can't get back.
            warn!(
                "CropDef for '{}' not found, refusing to sell (lots preserved)",
                ev.crop_id
            );
            inventory.harvested.insert(ev.crop_id.clone(), lots);
            continue;
        };

        let time_factor = market.time_factor(&ev.crop_id);
        let total: f32 = lots
            .iter()
            .map(|lot: &HarvestedLot| sale_value(def.sell_price, lot.quality, time_factor) * lot.quantity as f32)
            .sum();
        let total = total.round().max(0.0) as u32;

        wallet.money += total;
        info!(
            "Sold {} lot(s) of '{}' for {} (time_factor {:.2})",
            lots.len(),
            ev.crop_id,
            total,
            time_factor
        );
    }
}

/// Inventory keys lots by plain `String` crop id (design doc §2.5's own
/// note on why - outlives any particular `Handle<CropDef>`, survives
/// save/load as a string), so looking up a `CropDef` by id means
/// scanning loaded assets rather than a direct handle lookup. Fine at
/// this scale (a handful of crop types, per requirements §6).
fn find_crop_def<'a>(crop_defs: &'a Assets<CropDef>, crop_id: &str) -> Option<&'a CropDef> {
    crop_defs.iter().map(|(_, def)| def).find(|def| def.id == crop_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crops::{CropDef, QualityGrade, QualityThresholds};

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(bevy::asset::AssetPlugin::default());
        app.init_asset::<CropDef>();
        app.init_resource::<Wallet>();
        app.init_resource::<Inventory>();
        app.add_plugins(SellPlugin);
        app
    }

    fn dummy_crop_def(id: &str, sell_price: u32) -> CropDef {
        CropDef {
            id: id.to_string(),
            name: id.to_string(),
            growth_stages: vec![],
            min_temp: 0.0,
            max_temp: 30.0,
            ideal_temp_range: (10.0, 20.0),
            frost_tolerant: false,
            min_moisture: 0.0,
            max_moisture: 1.0,
            ideal_moisture_range: (0.3, 0.6),
            fertility_requirement: 0.0,
            base_yield: 10,
            sell_price,
            seed_cost: 1,
            quality_thresholds: QualityThresholds {
                premium_max_stress: 0.5,
                good_max_stress: 2.0,
                lethal_stress: 8.0,
            },
            optimal_harvest_window_days: 1,
            overripe_stress_per_day: 1.0,
            fertility_impact_per_day: 0.0,
        }
    }

    #[test]
    fn selling_drains_inventory_and_credits_wallet() {
        let mut app = test_app();

        let handle = {
            let mut crop_defs = app.world_mut().resource_mut::<Assets<CropDef>>();
            crop_defs.add(dummy_crop_def("tomato", 10))
        };
        // Keep the handle alive for the duration of the test so the
        // asset isn't dropped before process_sell_requests looks it up.
        app.insert_resource(KeepAlive(handle));

        {
            let mut inventory = app.world_mut().resource_mut::<Inventory>();
            inventory.add("tomato".to_string(), 5, QualityGrade::Good); // 5 * 10 * 1.0 = 50
            inventory.add("tomato".to_string(), 2, QualityGrade::Premium); // 2 * 15 = 30
        }

        app.world_mut().send_event(SellRequested {
            crop_id: "tomato".to_string(),
        });
        app.update();

        assert_eq!(app.world().resource::<Wallet>().money, STARTING_MONEY_TEST + 80);
        assert!(app
            .world()
            .resource::<Inventory>()
            .harvested
            .get("tomato")
            .map(|lots| lots.is_empty())
            .unwrap_or(true));
    }

    #[test]
    fn selling_with_no_lots_is_a_no_op() {
        let mut app = test_app();
        let starting = app.world().resource::<Wallet>().money;

        app.world_mut().send_event(SellRequested {
            crop_id: "tomato".to_string(),
        });
        app.update();

        assert_eq!(app.world().resource::<Wallet>().money, starting);
    }

    #[test]
    fn selling_with_unresolvable_crop_def_preserves_inventory() {
        let mut app = test_app();
        {
            let mut inventory = app.world_mut().resource_mut::<Inventory>();
            inventory.add("mystery_crop".to_string(), 3, QualityGrade::Good);
        }
        let starting = app.world().resource::<Wallet>().money;

        app.world_mut().send_event(SellRequested {
            crop_id: "mystery_crop".to_string(),
        });
        app.update();

        assert_eq!(app.world().resource::<Wallet>().money, starting);
        assert_eq!(
            app.world()
                .resource::<Inventory>()
                .harvested
                .get("mystery_crop")
                .map(|lots| lots.len()),
            Some(1)
        );
    }

    #[derive(Resource)]
    struct KeepAlive(Handle<CropDef>);

    // Wallet::default() starting balance, duplicated here rather than
    // importing the private constant - test-local constant so this
    // module doesn't depend on economy::mod's internals.
    const STARTING_MONEY_TEST: u32 = 1000;
}
