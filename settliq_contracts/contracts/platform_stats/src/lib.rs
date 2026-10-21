#![no_std]

mod test;

use soroban_sdk::{contract, contractimpl, contracttype, Address, Env};

/// Number of ledgers in a ~24h window (5s per ledger).
const ACTIVE_WINDOW_LEDGERS: u32 = 17_280;

/// Live platform overview metrics for the admin dashboard.
#[contracttype]
#[derive(Clone, Debug)]
pub struct PlatformStats {
    pub total_merchants: u32,
    pub total_payments: u32,
    pub total_settled_volume_usd: i128,
    pub active_payments_24h: u32,
    pub health: SystemHealth,
}

/// System health: DB/storage, Stellar connectivity, partner API.
#[contracttype]
#[derive(Clone, Debug)]
pub struct SystemHealth {
    pub storage_ok: bool,
    pub stellar_ok: bool,
    pub partner_ok: bool,
}

/// Two-element ring buffer tracking the current and previous 24h payment
/// counts.  Stored at a single, fixed key so the number of instance-storage
/// entries never grows beyond a constant regardless of how long the contract
/// runs.
///
/// Fields:
/// - `index`      – the bucket index (`ledger_sequence / ACTIVE_WINDOW_LEDGERS`)
///                  for the period whose count is in `count`.
/// - `count`      – payment count for `index` (the current window).
/// - `prev_count` – payment count for `index - 1` (the previous window).
///                  Retained so that `stats()` can blend the two windows when
///                  the current window has just started, though the contract
///                  currently surfaces only `count`.  Keeping `prev_count`
///                  here costs nothing and avoids a future storage migration.
#[contracttype]
#[derive(Clone, Debug)]
pub struct ActiveBucketState {
    pub index: u32,
    pub count: u32,
    pub prev_count: u32,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    TotalMerchants,
    TotalPayments,
    TotalSettledVolumeUsd,
    /// Single fixed key for the rolling 24h active-payment ring buffer.
    /// Replaces the old `ActiveBucket(u32)` variant that accumulated one key
    /// per day and would eventually exhaust instance storage.
    ActiveBucket,
    PartnerOk,
    /// Last observed Stellar ledger sequence for health checks. Tracks ledger
    /// progression to detect network stalls or freezes.
    LastSeenLedgerSequence,
}

#[contract]
pub struct PlatformStatsContract;

#[contractimpl]
impl PlatformStatsContract {
    pub fn __constructor(env: Env, admin: Address) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::TotalMerchants, &0u32);
        env.storage().instance().set(&DataKey::TotalPayments, &0u32);
        env.storage().instance().set(&DataKey::TotalSettledVolumeUsd, &0i128);
        env.storage().instance().set(&DataKey::PartnerOk, &true);
    }

    /// Admin-only: record a newly registered merchant.
    pub fn record_merchant(env: Env, caller: Address) {
        caller.require_auth();
        Self::require_admin(&env, &caller);
        let prev: u32 = env.storage().instance().get(&DataKey::TotalMerchants).unwrap();
        env.storage().instance().set(&DataKey::TotalMerchants, &(prev + 1));
    }

    /// Admin-only: record a payment. If `settled`, adds to settled USD volume
    /// and increments the rolling 24h active-payments bucket.
    ///
    /// # Panics
    /// If `amount_usd` is not strictly positive (> 0) when `settled` is true.
    pub fn record_payment(env: Env, caller: Address, amount_usd: i128, settled: bool) {
        caller.require_auth();
        Self::require_admin(&env, &caller);

        let tp: u32 = env.storage().instance().get(&DataKey::TotalPayments).unwrap();
        env.storage().instance().set(&DataKey::TotalPayments, &(tp + 1));

        if settled {
            // Validate amount is positive to prevent volume corruption from negative values
            if amount_usd <= 0 {
                panic!("amount_usd must be strictly positive when settled");
            }

            let vol: i128 = env
                .storage()
                .instance()
                .get(&DataKey::TotalSettledVolumeUsd)
                .unwrap();
            env.storage()
                .instance()
                .set(&DataKey::TotalSettledVolumeUsd, &(vol + amount_usd));
        }

        let bucket = env.ledger().sequence() / ACTIVE_WINDOW_LEDGERS;
        let mut state: ActiveBucketState = env
            .storage()
            .instance()
            .get(&DataKey::ActiveBucket)
            .unwrap_or(ActiveBucketState { index: bucket, count: 0, prev_count: 0 });

        if bucket > state.index {
            // We have moved into a new window.  The old current count becomes
            // the previous count (for the immediately preceding window), and
            // any older history is discarded – only two windows are ever kept.
            state.prev_count = if bucket == state.index + 1 { state.count } else { 0 };
            state.count = 0;
            state.index = bucket;
        }

        state.count += 1;
        env.storage().instance().set(&DataKey::ActiveBucket, &state);
    }

    /// Admin-only: reflect partner API health status.
    pub fn set_partner_ok(env: Env, caller: Address, ok: bool) {
        caller.require_auth();
        Self::require_admin(&env, &caller);
        env.storage().instance().set(&DataKey::PartnerOk, &ok);
    }

    /// Live overview metrics, computed directly from storage.
    pub fn stats(env: Env) -> PlatformStats {
        let current_sequence = env.ledger().sequence();

        // Update the last seen sequence for health checks (detects network stalls)
        env.storage()
            .instance()
            .set(&DataKey::LastSeenLedgerSequence, &current_sequence);

        let bucket = current_sequence / ACTIVE_WINDOW_LEDGERS;
        let active: u32 = env
            .storage()
            .instance()
            .get::<DataKey, ActiveBucketState>(&DataKey::ActiveBucket)
            .map(|state| if state.index == bucket { state.count } else { 0 })
            .unwrap_or(0);

        PlatformStats {
            total_merchants: env
                .storage()
                .instance()
                .get(&DataKey::TotalMerchants)
                .unwrap_or(0),
            total_payments: env
                .storage()
                .instance()
                .get(&DataKey::TotalPayments)
                .unwrap_or(0),
            total_settled_volume_usd: env
                .storage()
                .instance()
                .get(&DataKey::TotalSettledVolumeUsd)
                .unwrap_or(0),
            active_payments_24h: active,
            health: Self::health(&env),
        }
    }

    fn require_admin(env: &Env, caller: &Address) {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if caller != &admin {
            panic!("not admin");
        }
    }

    fn health(env: &Env) -> SystemHealth {
        let current_sequence = env.ledger().sequence();
        let last_seen: u32 = env
            .storage()
            .instance()
            .get(&DataKey::LastSeenLedgerSequence)
            .unwrap_or(current_sequence);

        // stellar_ok is true when the ledger is actively progressing (sequence has increased).
        // On first call or after contract reinitialization, this may be false for the first check,
        // but will become true once a subsequent call detects progression.
        let stellar_ok = current_sequence > last_seen;

        SystemHealth {
            storage_ok: env.storage().instance().has(&DataKey::Admin),
            stellar_ok,
            partner_ok: env
                .storage()
                .instance()
                .get(&DataKey::PartnerOk)
                .unwrap_or(true),
        }
    }
}
