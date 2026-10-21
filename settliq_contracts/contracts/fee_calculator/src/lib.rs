#![no_std]

mod test;

use soroban_sdk::{contract, contractimpl, contracttype, Address, Env, Vec};

const LEDGERS_PER_30_DAYS: u32 = 172_800;
/// Number of sub-window buckets used to approximate a rolling 30-day window.
/// Each bucket covers `LEDGERS_PER_30_DAYS / VOLUME_BUCKETS` ledgers (5 days).
const VOLUME_BUCKETS: u32 = 6;
/// Ledgers covered by a single sub-window bucket (5 days).
const LEDGERS_PER_BUCKET: u32 = LEDGERS_PER_30_DAYS / VOLUME_BUCKETS;
const BPS_DENOMINATOR: i128 = 10_000;

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct FeeTier {
    pub threshold_usdc: i128,
    pub fee_bps: u32,
}

/// Trailing 30-day volume tracked as a ring of `VOLUME_BUCKETS` sub-windows.
///
/// Instead of a single tumbling window that resets to zero every 30 days,
/// volume is accumulated into `VOLUME_BUCKETS` buckets of
/// `LEDGERS_PER_BUCKET` ledgers each. As time advances, buckets that fall
/// outside the trailing 30-day span are cleared, so the reported volume
/// decays smoothly (one bucket at a time) rather than cliff-resetting.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct MerchantVolume {
    /// Ledger at which the current bucket ring was started.
    pub window_start_ledger: u32,
    /// Total trailing volume across all live buckets (USDC).
    pub volume_usdc: i128,
    /// Per-bucket volume, indexed by `(bucket_index % VOLUME_BUCKETS)`.
    pub buckets: Vec<i128>,
    /// Ledger at which each bucket was last written, used to expire stale
    /// buckets. Indexed the same way as `buckets`.
    pub bucket_ledgers: Vec<u32>,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    SettlementCaller,
    Admin,
    FeeTiers,
    MinFeeStroops,
    MerchantVolume(Address),
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct TierAppliedEvent {
    pub merchant: Address,
    pub volume_usdc: i128,
    pub fee_bps: u32,
    pub fee: i128,
    pub net: i128,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct FeeTiersUpdatedEvent {
    pub admin: Address,
    pub tiers: Vec<FeeTier>,
}

#[contract]
pub struct FeeCalculatorContract;

#[contractimpl]
impl FeeCalculatorContract {
    pub fn __constructor(env: Env, admin: Address, tiers: Vec<FeeTier>) {
        Self::validate_tiers(&tiers);
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::FeeTiers, &tiers);
        env.storage().instance().set(&DataKey::MinFeeStroops, &0i128);
    }

    pub fn set_fee_tiers(env: Env, caller: Address, tiers: Vec<FeeTier>) {
        caller.require_auth();
        Self::require_admin(&env, &caller);
        Self::validate_tiers(&tiers);
        env.storage().instance().set(&DataKey::FeeTiers, &tiers);

        env.events().publish(
            ("FEE", "tiers_updated"),
            FeeTiersUpdatedEvent {
                admin: caller,
                tiers,
            },
        );
    }

    pub fn set_settlement_caller(env: Env, caller: Address, settlement_caller: Address) {
        caller.require_auth();
        Self::require_admin(&env, &caller);
        env.storage()
            .instance()
            .set(&DataKey::SettlementCaller, &settlement_caller);
    }

    pub fn get_settlement_caller(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::SettlementCaller)
    }

    pub fn set_min_fee_stroops(env: Env, caller: Address, min_fee_stroops: i128) {
        caller.require_auth();
        Self::require_admin(&env, &caller);
        if min_fee_stroops < 0 {
            panic!("min_fee_stroops must be >= 0");
        }
        env.storage()
            .instance()
            .set(&DataKey::MinFeeStroops, &min_fee_stroops);
    }

    pub fn get_min_fee_stroops(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::MinFeeStroops)
            .unwrap_or(0)
    }

    pub fn get_fee_tiers(env: Env) -> Vec<FeeTier> {
        // `__constructor` always validates and stores `FeeTiers`, so for any
        // live contract instance the tiers are guaranteed to be present.
        env.storage()
            .instance()
            .get(&DataKey::FeeTiers)
            .expect("tiers not initialized")
    }

    pub fn calculate_fee(
        env: Env,
        caller: Address,
        merchant: Address,
        amount: i128,
    ) -> (i128, i128, u32) {
        caller.require_auth();
        Self::require_authorized_caller(&env, &caller);

        if amount <= 0 {
            panic!("amount must be > 0");
        }

        let volume = Self::update_and_get_volume(&env, &merchant, amount);
        let fee_bps = Self::select_fee_bps(&env, volume);

        let mut fee = amount
            .checked_mul(fee_bps as i128)
            .expect("overflow")
            .checked_div(BPS_DENOMINATOR)
            .expect("division failure");

        let min_fee_stroops: i128 = env
            .storage()
            .instance()
            .get(&DataKey::MinFeeStroops)
            .unwrap_or(0);
        if fee < min_fee_stroops {
            fee = min_fee_stroops;
        }
        if fee > amount {
            fee = amount;
        }

        let net = amount.checked_sub(fee).expect("underflow");

        env.events().publish(
            ("FEE", "tier_applied"),
            TierAppliedEvent {
                merchant,
                volume_usdc: volume,
                fee_bps,
                fee,
                net,
            },
        );

        (fee, net, fee_bps)
    }

    /// Read-only quote: returns the fee breakdown for `amount` using the
    /// merchant's current trailing 30-day volume, without mutating any
    /// storage (issue #61).
    ///
    /// Safe for permissionless use by payment frontends and checkout APIs
    /// that need to display an estimated fee before committing to a settlement.
    /// The returned `(fee, net, fee_bps)` values are identical to what
    /// `calculate_fee` would produce for the same inputs, but no volume
    /// accumulation occurs.
    pub fn quote_fee(env: Env, merchant: Address, amount: i128) -> (i128, i128, u32) {
        if amount <= 0 {
            panic!("amount must be > 0");
        }

        // Read current trailing volume without mutating storage.
        let volume = Self::get_merchant_volume(env.clone(), merchant).volume_usdc;
        let fee_bps = Self::select_fee_bps(&env, volume);

        let mut fee = amount
            .checked_mul(fee_bps as i128)
            .expect("overflow")
            .checked_div(BPS_DENOMINATOR)
            .expect("division failure");

        let min_fee_stroops: i128 = env
            .storage()
            .instance()
            .get(&DataKey::MinFeeStroops)
            .unwrap_or(0);
        if fee < min_fee_stroops {
            fee = min_fee_stroops;
        }
        if fee > amount {
            fee = amount;
        }

        let net = amount.checked_sub(fee).expect("underflow");

        (fee, net, fee_bps)
    }

    /// Read-only query: returns the merchant's current trailing 30-day volume.
    ///
    /// Volume is tracked as `VOLUME_BUCKETS` sub-windows of
    /// `LEDGERS_PER_BUCKET` ledgers each. Buckets older than the trailing
    /// 30-day span are excluded from the returned total, so the value decays
    /// one bucket at a time instead of resetting to zero at a single boundary.
    /// This is computed in memory and not persisted; persistence happens in
    /// `update_and_get_volume` during `calculate_fee`.
    pub fn get_merchant_volume(env: Env, merchant: Address) -> MerchantVolume {
        let current_ledger = env.ledger().sequence();
        let key = DataKey::MerchantVolume(merchant);
        let data = env
            .storage()
            .persistent()
            .get::<DataKey, MerchantVolume>(&key)
            .unwrap_or_else(|| Self::empty_volume(&env, current_ledger));

        Self::prune_and_sum(&env, data, current_ledger)
    }

    /// Builds an empty bucket ring anchored at `start_ledger`.
    fn empty_volume(env: &Env, start_ledger: u32) -> MerchantVolume {
        let mut buckets = Vec::new(env);
        let mut bucket_ledgers = Vec::new(env);
        for _ in 0..VOLUME_BUCKETS {
            buckets.push_back(0i128);
            bucket_ledgers.push_back(start_ledger);
        }
        MerchantVolume {
            window_start_ledger: start_ledger,
            volume_usdc: 0,
            buckets,
            bucket_ledgers,
        }
    }

    /// Drops buckets that have fallen outside the trailing 30-day span and
    /// returns the volume with `volume_usdc` recomputed from live buckets.
    fn prune_and_sum(env: &Env, mut data: MerchantVolume, current_ledger: u32) -> MerchantVolume {
        // Backfill bucket vectors for any legacy single-window record.
        if data.buckets.len() != VOLUME_BUCKETS {
            let legacy_volume = data.volume_usdc;
            let legacy_start = data.window_start_ledger;
            data = Self::empty_volume(env, legacy_start);
            if legacy_volume > 0 {
                data.buckets.set(0, legacy_volume);
                data.bucket_ledgers.set(0, legacy_start);
            }
        }

        let mut total: i128 = 0;
        for i in 0..VOLUME_BUCKETS {
            let bucket_ledger = data.bucket_ledgers.get(i).unwrap();
            let age = current_ledger.saturating_sub(bucket_ledger);
            if age >= LEDGERS_PER_30_DAYS {
                // Bucket has aged out of the trailing window.
                data.buckets.set(i, 0i128);
            } else {
                total = total.saturating_add(data.buckets.get(i).unwrap());
            }
        }
        data.volume_usdc = total;
        data
    }

    fn require_admin(env: &Env, caller: &Address) {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if &admin != caller {
            panic!("Not admin");
        }
    }

    fn require_authorized_caller(env: &Env, caller: &Address) {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if caller == &admin {
            return;
        }
        if let Some(settlement_caller) = env
            .storage()
            .instance()
            .get::<DataKey, Address>(&DataKey::SettlementCaller)
        {
            if caller == &settlement_caller {
                return;
            }
        }
        panic!("Not authorized");
    }

    fn validate_tiers(tiers: &Vec<FeeTier>) {
        if tiers.len() == 0 {
            panic!("tiers must not be empty");
        }

        let mut prev_threshold = -1;
        let mut prev_bps = u32::MAX;

        for i in 0..tiers.len() {
            let tier = tiers.get(i).unwrap();
            if tier.threshold_usdc < 0 {
                panic!("threshold must be >= 0");
            }
            if tier.threshold_usdc <= prev_threshold {
                panic!("thresholds must be strictly increasing");
            }
            if tier.fee_bps > BPS_DENOMINATOR as u32 {
                panic!("fee_bps must be <= 10000");
            }
            if tier.fee_bps > prev_bps {
                panic!("fee_bps must be non-increasing");
            }
            prev_threshold = tier.threshold_usdc;
            prev_bps = tier.fee_bps;
        }
    }

    fn select_fee_bps(env: &Env, volume: i128) -> u32 {
        let tiers: Vec<FeeTier> = env
            .storage()
            .instance()
            .get(&DataKey::FeeTiers)
            .expect("tiers not initialized");

        let mut selected = tiers.get(0).unwrap().fee_bps;
        for i in 0..tiers.len() {
            let tier = tiers.get(i).unwrap();
            if volume >= tier.threshold_usdc {
                selected = tier.fee_bps;
            } else {
                break;
            }
        }
        selected
    }

    /// Records `amount` into the merchant's trailing 30-day volume and returns
    /// the updated total.
    ///
    /// Volume is bucketed into `VOLUME_BUCKETS` sub-windows of
    /// `LEDGERS_PER_BUCKET` ledgers. The current bucket is selected by
    /// `(current_ledger - window_start_ledger) / LEDGERS_PER_BUCKET`; when a
    /// new bucket is entered, the slot it reuses is cleared first so that
    /// volume from ~30 days ago rolls off gradually rather than all at once.
    fn update_and_get_volume(env: &Env, merchant: &Address, amount: i128) -> i128 {
        let current_ledger = env.ledger().sequence();
        let key = DataKey::MerchantVolume(merchant.clone());
        let data = env
            .storage()
            .persistent()
            .get::<DataKey, MerchantVolume>(&key)
            .unwrap_or_else(|| Self::empty_volume(env, current_ledger));

        let mut data = Self::prune_and_sum(env, data, current_ledger);

        let elapsed = current_ledger.saturating_sub(data.window_start_ledger);
        let bucket_index = (elapsed / LEDGERS_PER_BUCKET) % VOLUME_BUCKETS;

        // If this slot still holds volume from a previous cycle (i.e. it is
        // older than the trailing window), clear it before reuse.
        let slot_ledger = data.bucket_ledgers.get(bucket_index).unwrap();
        if current_ledger.saturating_sub(slot_ledger) >= LEDGERS_PER_30_DAYS {
            data.buckets.set(bucket_index, 0i128);
        }

        let slot_volume = data.buckets.get(bucket_index).unwrap();
        let new_slot_volume = slot_volume.saturating_add(amount);
        data.buckets.set(bucket_index, new_slot_volume);
        data.bucket_ledgers.set(bucket_index, current_ledger);

        // Recompute the trailing total from live buckets.
        let mut total: i128 = 0;
        for i in 0..VOLUME_BUCKETS {
            let bucket_ledger = data.bucket_ledgers.get(i).unwrap();
            if current_ledger.saturating_sub(bucket_ledger) < LEDGERS_PER_30_DAYS {
                total = total.saturating_add(data.buckets.get(i).unwrap());
            }
        }
        data.volume_usdc = total;

        env.storage().persistent().set(&key, &data);
        total
    }
}
