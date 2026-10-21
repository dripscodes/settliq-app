#![cfg(test)]

use soroban_sdk::{
    testutils::{Address as _, Events as _, MockAuth, MockAuthInvoke},
    token::{Client as TokenClient, StellarAssetClient},
    Address, Env, IntoVal, Symbol,
};

use crate::{FeeDistributorContract, FeeDistributorContractClient};

struct Setup<'a> {
    env: Env,
    admin: Address,
    treasury: Address,
    lp: Address,
    caller: Address,
    token: TokenClient<'a>,
    token_address: Address,
    client: FeeDistributorContractClient<'a>,
}

fn setup(lp_share_bps: i128) -> Setup<'static> {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let lp = Address::generate(&env);
    let caller = Address::generate(&env);

    // Deploy a test token (Stellar Asset Contract) — used as USDC stand-in.
    let token_admin = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract_v2(token_admin.clone());
    let sac = StellarAssetClient::new(&env, &token_id.address());
    // Mint 1_000_000 to caller so distribute() can pull from them.
    sac.mint(&caller, &1_000_000);

    let contract_id = env.register(
        FeeDistributorContract,
        (&admin, &treasury, &lp, lp_share_bps, &token_id.address(), &caller),
    );

    let client = FeeDistributorContractClient::new(&env, &contract_id);
    let token = TokenClient::new(&env, &token_id.address());
    let token_address = token_id.address();

    Setup { env, admin, treasury, lp, caller, token, token_address, client }
}

// ─── Split-ratio tests (USDC, the default allowed token) ─────────────────────

#[test]
fn test_50_50_split() {
    let s = setup(5_000); // 50% LP
    s.client.distribute(&s.caller, &s.token_address, &10_000);
    assert_eq!(s.token.balance(&s.treasury), 5_000);
    assert_eq!(s.token.balance(&s.lp), 5_000);
}

#[test]
fn test_30_70_split() {
    let s = setup(3_000); // 30% LP, 70% treasury
    s.client.distribute(&s.caller, &s.token_address, &10_000);
    assert_eq!(s.token.balance(&s.lp), 3_000);
    assert_eq!(s.token.balance(&s.treasury), 7_000);
}

#[test]
fn test_zero_lp_share() {
    let s = setup(0); // 0% LP — all goes to treasury
    s.client.distribute(&s.caller, &s.token_address, &10_000);
    assert_eq!(s.token.balance(&s.treasury), 10_000);
    assert_eq!(s.token.balance(&s.lp), 0);
}

#[test]
fn test_hundred_percent_lp() {
    let s = setup(10_000); // 100% LP — nothing to treasury
    s.client.distribute(&s.caller, &s.token_address, &10_000);
    assert_eq!(s.token.balance(&s.lp), 10_000);
    assert_eq!(s.token.balance(&s.treasury), 0);
}

#[test]
fn test_rounding_remainder_goes_to_treasury() {
    let s = setup(3_333); // 33.33% LP
    s.client.distribute(&s.caller, &s.token_address, &10);
    // lp = 10 * 3333 / 10000 = 3 (truncated), treasury = 7
    assert_eq!(s.token.balance(&s.lp), 3);
    assert_eq!(s.token.balance(&s.treasury), 7);
}

// ─── Multi-token distribution (XLM and USDC) ─────────────────────────────────

/// Distribute fees denominated in a second token (XLM stand-in) after
/// registering it via `add_allowed_token`. Verifies that treasury and LP
/// receive the correct amounts in the XLM token — not the initially configured
/// USDC token — demonstrating fix for issue #63.
#[test]
fn test_distribute_xlm_fees() {
    let s = setup(5_000); // 50 / 50 split

    // Deploy a second asset to act as native XLM.
    let xlm_admin = Address::generate(&s.env);
    let xlm_id = s.env.register_stellar_asset_contract_v2(xlm_admin.clone());
    let xlm_sac = StellarAssetClient::new(&s.env, &xlm_id.address());
    // Mint XLM to the caller so distribute() can pull from them.
    xlm_sac.mint(&s.caller, &1_000_000);

    let xlm_token = TokenClient::new(&s.env, &xlm_id.address());

    // Allow the XLM token.
    s.client.add_allowed_token(&s.admin, &xlm_id.address());

    // Distribute XLM fees.
    s.client.distribute(&s.caller, &xlm_id.address(), &10_000);

    // Correct XLM balances.
    assert_eq!(xlm_token.balance(&s.treasury), 5_000);
    assert_eq!(xlm_token.balance(&s.lp), 5_000);

    // USDC balances are untouched.
    assert_eq!(s.token.balance(&s.treasury), 0);
    assert_eq!(s.token.balance(&s.lp), 0);
}

/// Distribute USDC fees first, then XLM fees in a subsequent call.
/// Both distributions should succeed and balances for each token should be
/// tracked independently.
#[test]
fn test_distribute_both_usdc_and_xlm() {
    let s = setup(5_000); // 50 / 50

    // Deploy XLM stand-in.
    let xlm_admin = Address::generate(&s.env);
    let xlm_id = s.env.register_stellar_asset_contract_v2(xlm_admin.clone());
    let xlm_sac = StellarAssetClient::new(&s.env, &xlm_id.address());
    xlm_sac.mint(&s.caller, &1_000_000);

    let xlm_token = TokenClient::new(&s.env, &xlm_id.address());

    // Register XLM as an allowed fee asset.
    s.client.add_allowed_token(&s.admin, &xlm_id.address());

    // First distribution: USDC.
    s.client.distribute(&s.caller, &s.token_address, &10_000);

    // Second distribution: XLM.
    s.client.distribute(&s.caller, &xlm_id.address(), &20_000);

    // USDC balances.
    assert_eq!(s.token.balance(&s.treasury), 5_000);
    assert_eq!(s.token.balance(&s.lp), 5_000);

    // XLM balances.
    assert_eq!(xlm_token.balance(&s.treasury), 10_000);
    assert_eq!(xlm_token.balance(&s.lp), 10_000);
}

/// `distribute` rejects a token address that has not been added to the
/// allowlist, even if the caller holds that token.
#[test]
#[should_panic(expected = "token not allowed")]
fn test_distribute_rejects_unapproved_token() {
    let s = setup(5_000);

    // Deploy a random unknown token.
    let unknown_admin = Address::generate(&s.env);
    let unknown_id = s.env.register_stellar_asset_contract_v2(unknown_admin.clone());
    let unknown_sac = StellarAssetClient::new(&s.env, &unknown_id.address());
    unknown_sac.mint(&s.caller, &10_000);

    // Attempt to distribute without adding to the allowlist — should panic.
    s.client.distribute(&s.caller, &unknown_id.address(), &10_000);
}

/// After adding and then removing a token, distribution for that token should
/// be rejected again.
#[test]
#[should_panic(expected = "token not allowed")]
fn test_distribute_rejects_removed_token() {
    let s = setup(5_000);

    let xlm_admin = Address::generate(&s.env);
    let xlm_id = s.env.register_stellar_asset_contract_v2(xlm_admin.clone());
    let xlm_sac = StellarAssetClient::new(&s.env, &xlm_id.address());
    xlm_sac.mint(&s.caller, &10_000);

    // Add then remove.
    s.client.add_allowed_token(&s.admin, &xlm_id.address());
    s.client.remove_allowed_token(&s.admin, &xlm_id.address());

    // Distribution should now fail.
    s.client.distribute(&s.caller, &xlm_id.address(), &10_000);
}

/// `get_allowed_tokens` returns the USDC address seeded in the constructor.
#[test]
fn test_get_allowed_tokens_contains_usdc_at_init() {
    let s = setup(5_000);
    let tokens = s.client.get_allowed_tokens();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens.get(0).unwrap(), s.token_address);
}

/// `get_allowed_tokens` reflects additions.
#[test]
fn test_get_allowed_tokens_reflects_additions() {
    let s = setup(5_000);

    let xlm_admin = Address::generate(&s.env);
    let xlm_id = s.env.register_stellar_asset_contract_v2(xlm_admin.clone());

    s.client.add_allowed_token(&s.admin, &xlm_id.address());

    let tokens = s.client.get_allowed_tokens();
    assert_eq!(tokens.len(), 2);
    assert!(tokens.contains(&xlm_id.address()));
}

/// `fee_distributed` event includes the token address used for the transfer.
#[test]
fn test_distribute_event_includes_token_address() {
    let s = setup(5_000);
    s.client.distribute(&s.caller, &s.token_address, &10_000);
    let events = s.env.events().all();
    let last = events.last().unwrap();
    let event: crate::FeeDistributedEvent = last.2.into_val(&s.env);
    assert_eq!(event.token, s.token_address);
    assert_eq!(event.treasury, s.treasury);
    assert_eq!(event.lp_address, s.lp);
    assert_eq!(event.treasury_amount, 5_000);
    assert_eq!(event.lp_amount, 5_000);
}

// ─── Setter address-invariant tests (issue #94) ──────────────────────────────

/// `set_treasury` must reject setting treasury equal to the current lp_address.
#[test]
#[should_panic(expected = "treasury must differ from lp_address")]
fn test_set_treasury_rejects_lp_address() {
    let s = setup(5_000);
    s.client.set_treasury(&s.admin, &s.lp);
}

/// `set_treasury` must reject setting treasury equal to the admin.
#[test]
#[should_panic(expected = "treasury must differ from admin")]
fn test_set_treasury_rejects_admin() {
    let s = setup(5_000);
    s.client.set_treasury(&s.admin, &s.admin);
}

/// `set_treasury` must reject setting treasury equal to the contract address.
#[test]
#[should_panic(expected = "treasury must differ from contract")]
fn test_set_treasury_rejects_contract_address() {
    let s = setup(5_000);
    let contract_address = s.client.address.clone();
    s.client.set_treasury(&s.admin, &contract_address);
}

/// `set_lp_address` must reject setting lp_address equal to the current treasury.
#[test]
#[should_panic(expected = "lp_address must differ from treasury")]
fn test_set_lp_address_rejects_treasury() {
    let s = setup(5_000);
    s.client.set_lp_address(&s.admin, &s.treasury);
}

/// `set_lp_address` must reject setting lp_address equal to the admin.
#[test]
#[should_panic(expected = "lp_address must differ from admin")]
fn test_set_lp_address_rejects_admin() {
    let s = setup(5_000);
    s.client.set_lp_address(&s.admin, &s.admin);
}

/// `set_lp_address` must reject setting lp_address equal to the contract address.
#[test]
#[should_panic(expected = "lp_address must differ from contract")]
fn test_set_lp_address_rejects_contract_address() {
    let s = setup(5_000);
    let contract_address = s.client.address.clone();
    s.client.set_lp_address(&s.admin, &contract_address);
}

/// Valid distinct addresses succeed and emit update events.
#[test]
fn test_set_treasury_and_lp_address_valid() {
    let s = setup(5_000);

    let new_treasury = Address::generate(&s.env);
    s.client.set_treasury(&s.admin, &new_treasury);
    assert_eq!(s.client.get_treasury(), new_treasury);

    let new_lp = Address::generate(&s.env);
    s.client.set_lp_address(&s.admin, &new_lp);
    assert_eq!(s.client.get_lp_address(), new_lp);

    let events = s.env.events().all();
    let treasury_event: crate::TreasuryUpdatedEvent =
        events.get(events.len() - 2).unwrap().2.into_val(&s.env);
    assert_eq!(treasury_event.treasury, new_treasury);

    let lp_event: crate::LpAddressUpdatedEvent =
        events.get(events.len() - 1).unwrap().2.into_val(&s.env);
    assert_eq!(lp_event.lp_address, new_lp);
}
