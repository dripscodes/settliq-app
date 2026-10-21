#![no_std]

use soroban_sdk::{contract, contractimpl, contracttype, token, Address, Env, Vec};

// ─── Storage keys ────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Treasury,
    LpAddress,
    LpShareBps,
    AllowedTokens,
}

// ─── Events ──────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeDistributedEvent {
    pub token: Address,
    pub treasury: Address,
    pub lp_address: Address,
    pub treasury_amount: i128,
    pub lp_amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreasuryUpdatedEvent {
    pub treasury: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LpAddressUpdatedEvent {
    pub lp_address: Address,
}

// ─── Contract ────────────────────────────────────────────────────────────────

#[contract]
pub struct FeeDistributorContract;

#[contractimpl]
impl FeeDistributorContract {
    /// Initialize the fee distributor.
    ///
    /// * `admin`        — address allowed to update configuration.
    /// * `treasury`     — receives the non-LP share of fees.
    /// * `lp_address`   — receives the LP share of fees.
    /// * `lp_share_bps` — LP share in basis points (0–10_000).
    /// * `usdc_token`   — default allowed fee token (USDC).
    /// * `caller`       — retained for signature compatibility.
    pub fn __constructor(
        env: Env,
        admin: Address,
        treasury: Address,
        lp_address: Address,
        lp_share_bps: i128,
        usdc_token: Address,
        _caller: Address,
    ) {
        let contract_address = env.current_contract_address();
        assert!(treasury != lp_address, "treasury must differ from lp_address");
        assert!(admin != treasury, "admin must differ from treasury");
        assert!(admin != lp_address, "admin must differ from lp_address");
        assert!(treasury != contract_address, "treasury must differ from contract");
        assert!(lp_address != contract_address, "lp_address must differ from contract");
        assert!(lp_share_bps >= 0 && lp_share_bps <= 10_000, "invalid lp_share_bps");

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Treasury, &treasury);
        env.storage().instance().set(&DataKey::LpAddress, &lp_address);
        env.storage().instance().set(&DataKey::LpShareBps, &lp_share_bps);

        let mut tokens: Vec<Address> = Vec::new(&env);
        tokens.push_back(usdc_token);
        env.storage().instance().set(&DataKey::AllowedTokens, &tokens);
    }

    // ─── Admin setters ───────────────────────────────────────────────────────

    pub fn set_treasury(env: Env, caller: Address, treasury: Address) {
        caller.require_auth();
        Self::require_admin(&env, &caller);

        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        let lp_address: Address = env.storage().instance().get(&DataKey::LpAddress).unwrap();
        let contract_address = env.current_contract_address();
        assert!(treasury != lp_address, "treasury must differ from lp_address");
        assert!(treasury != admin, "treasury must differ from admin");
        assert!(treasury != contract_address, "treasury must differ from contract");

        env.storage().instance().set(&DataKey::Treasury, &treasury);
        env.events().publish(
            (Symbol::new(&env, "treasury_updated"),),
            TreasuryUpdatedEvent { treasury },
        );
    }

    pub fn set_lp_address(env: Env, caller: Address, lp_address: Address) {
        caller.require_auth();
        Self::require_admin(&env, &caller);

        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        let treasury: Address = env.storage().instance().get(&DataKey::Treasury).unwrap();
        let contract_address = env.current_contract_address();
        assert!(lp_address != treasury, "lp_address must differ from treasury");
        assert!(lp_address != admin, "lp_address must differ from admin");
        assert!(lp_address != contract_address, "lp_address must differ from contract");

        env.storage().instance().set(&DataKey::LpAddress, &lp_address);
        env.events().publish(
            (Symbol::new(&env, "lp_address_updated"),),
            LpAddressUpdatedEvent { lp_address },
        );
    }

    // ─── Distribution ────────────────────────────────────────────────────────

    pub fn distribute(env: Env, caller: Address, token_address: Address, amount: i128) {
        caller.require_auth();
        assert!(amount > 0, "amount must be positive");

        let allowed: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::AllowedTokens)
            .unwrap();
        assert!(allowed.contains(&token_address), "token not allowed");

        let treasury: Address = env.storage().instance().get(&DataKey::Treasury).unwrap();
        let lp_address: Address = env.storage().instance().get(&DataKey::LpAddress).unwrap();
        let lp_share_bps: i128 = env.storage().instance().get(&DataKey::LpShareBps).unwrap();

        let lp_amount = amount * lp_share_bps / 10_000;
        let treasury_amount = amount - lp_amount;

        let token_client = token::Client::new(&env, &token_address);
        if lp_amount > 0 {
            token_client.transfer(&caller, &lp_address, &lp_amount);
        }
        if treasury_amount > 0 {
            token_client.transfer(&caller, &treasury, &treasury_amount);
        }

        env.events().publish(
            (Symbol::new(&env, "fee_distributed"),),
            FeeDistributedEvent {
                token: token_address,
                treasury,
                lp_address,
                treasury_amount,
                lp_amount,
            },
        );
    }

    // ─── Token allowlist ─────────────────────────────────────────────────────

    pub fn add_allowed_token(env: Env, caller: Address, token_address: Address) {
        caller.require_auth();
        Self::require_admin(&env, &caller);

        let mut allowed: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::AllowedTokens)
            .unwrap();
        if !allowed.contains(&token_address) {
            allowed.push_back(token_address);
            env.storage().instance().set(&DataKey::AllowedTokens, &allowed);
        }
    }

    pub fn remove_allowed_token(env: Env, caller: Address, token_address: Address) {
        caller.require_auth();
        Self::require_admin(&env, &caller);

        let allowed: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::AllowedTokens)
            .unwrap();
        let mut updated: Vec<Address> = Vec::new(&env);
        for t in allowed.iter() {
            if t != token_address {
                updated.push_back(t);
            }
        }
        env.storage().instance().set(&DataKey::AllowedTokens, &updated);
    }

    pub fn get_allowed_tokens(env: Env) -> Vec<Address> {
        env.storage()
            .instance()
            .get(&DataKey::AllowedTokens)
            .unwrap()
    }

    // ─── Getters ─────────────────────────────────────────────────────────────

    pub fn get_treasury(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Treasury).unwrap()
    }

    pub fn get_lp_address(env: Env) -> Address {
        env.storage().instance().get(&DataKey::LpAddress).unwrap()
    }

    pub fn get_lp_share_bps(env: Env) -> i128 {
        env.storage().instance().get(&DataKey::LpShareBps).unwrap()
    }

    // ─── Internal helpers ────────────────────────────────────────────────────

    fn require_admin(env: &Env, caller: &Address) {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        assert!(caller == &admin, "caller is not admin");
    }
}
