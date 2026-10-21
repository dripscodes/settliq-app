#![no_std]

use soroban_sdk::{contract, contractimpl, contracttype, Address, Env};

/// Number of ledgers in the rolling 24h active-payment window.
///
/// Soroban targets ~5s ledger close times, so 24h ≈ 17_280 ledgers.
pub const ACTIVE_WINDOW_LEDGERS: u32 = 17_280;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformHealth {
    pub storage_ok: bool,
    pub stellar_ok: bool,
    pub partner_ok: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformStats {
    pub total_merchants: u32,
    pub total_payments: u32,
    pub total_settled_volume_usd: i128,
    pub active_payments_24h: u32,
    pub health: PlatformHealth,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerchantRecordedEvent {
    pub total_merchants: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaymentRecordedEvent {
    pub amount_usd: i128,
    pub settled: bool,
    pub total_payments: u32,
    pub total_settled_volume_usd: i128,
    pub active_payments_24h: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PartnerHealthUpdatedEvent {
    pub partner_ok: bool,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    TotalMerchants,
    TotalPayments,
    TotalSettledVolume,
    ActiveBucket,
    PartnerOk,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveBucket {
    pub bucket: u32,
    pub count: u32,
}

#[contract]
pub struct PlatformStatsContract;

#[contractimpl]
impl PlatformStatsContract {
    pub fn __constructor(env: Env, admin: Address) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::TotalMerchants, &0u32);
        env.storage().instance().set(&DataKey::TotalPayments, &0u32);
        env.storage()
            .instance()
            .set(&DataKey::TotalSettledVolume, &0i128);
        env.storage().instance().set(&DataKey::PartnerOk, &true);
    }

    pub fn record_merchant(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        let total: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TotalMerchants)
            .unwrap_or(0);
        let new_total = total.saturating_add(1);
        env.storage()
            .instance()
            .set(&DataKey::TotalMerchants, &new_total);

        env.events().publish(
            (soroban_sdk::symbol_short!("merchant"),),
            MerchantRecordedEvent {
                total_merchants: new_total,
            },
        );
    }

    pub fn record_payment(env: Env, admin: Address, amount_usd: i128, settled: bool) {
        Self::require_admin(&env, &admin);

        let total_payments: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TotalPayments)
            .unwrap_or(0);
        let new_total_payments = total_payments.saturating_add(1);
        env.storage()
            .instance()
            .set(&DataKey::TotalPayments, &new_total_payments);

        let total_volume: i128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalSettledVolume)
            .unwrap_or(0);
        let new_total_volume = if settled {
            total_volume.saturating_add(amount_usd)
        } else {
            total_volume
        };
        env.storage()
            .instance()
            .set(&DataKey::TotalSettledVolume, &new_total_volume);

        let active = Self::bump_active(&env);

        env.events().publish(
            (soroban_sdk::symbol_short!("payment"),),
            PaymentRecordedEvent {
                amount_usd,
                settled,
                total_payments: new_total_payments,
                total_settled_volume_usd: new_total_volume,
                active_payments_24h: active,
            },
        );
    }

    pub fn set_partner_ok(env: Env, admin: Address, ok: bool) {
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&DataKey::PartnerOk, &ok);

        env.events().publish(
            (soroban_sdk::symbol_short!("partner"),),
            PartnerHealthUpdatedEvent { partner_ok: ok },
        );
    }

    pub fn stats(env: Env) -> PlatformStats {
        let total_merchants: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TotalMerchants)
            .unwrap_or(0);
        let total_payments: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TotalPayments)
            .unwrap_or(0);
        let total_settled_volume_usd: i128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalSettledVolume)
            .unwrap_or(0);
        let partner_ok: bool = env
            .storage()
            .instance()
            .get(&DataKey::PartnerOk)
            .unwrap_or(true);

        PlatformStats {
            total_merchants,
            total_payments,
            total_settled_volume_usd,
            active_payments_24h: Self::current_active(&env),
            health: PlatformHealth {
                storage_ok: true,
                stellar_ok: true,
                partner_ok,
            },
        }
    }

    fn require_admin(env: &Env, admin: &Address) {
        let stored: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if &stored != admin {
            panic!("not admin");
        }
        admin.require_auth();
    }

    fn current_bucket(env: &Env) -> u32 {
        env.ledger().sequence() / ACTIVE_WINDOW_LEDGERS
    }

    fn current_active(env: &Env) -> u32 {
        let bucket = Self::current_bucket(env);
        match env
            .storage()
            .instance()
            .get::<DataKey, ActiveBucket>(&DataKey::ActiveBucket)
        {
            Some(active) if active.bucket == bucket => active.count,
            _ => 0,
        }
    }

    fn bump_active(env: &Env) -> u32 {
        let bucket = Self::current_bucket(env);
        let count = match env
            .storage()
            .instance()
            .get::<DataKey, ActiveBucket>(&DataKey::ActiveBucket)
        {
            Some(active) if active.bucket == bucket => active.count.saturating_add(1),
            _ => 1,
        };
        env.storage()
            .instance()
            .set(&DataKey::ActiveBucket, &ActiveBucket { bucket, count });
        count
    }
}

#[cfg(test)]
mod test;
