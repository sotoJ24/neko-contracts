#![cfg(test)]
extern crate std;

use soroban_sdk::{
    symbol_short,
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    Address, Env, Symbol,
};

use crate::neko_pool;
use crate::{NekoAdapter, NekoAdapterClient};

/// Import neko-oracle WASM for test setup.
/// Build first: cargo build --target wasm32v1-none --release -p neko-oracle
mod neko_oracle {
    soroban_sdk::contractimport!(
        file = "../../target/wasm32v1-none/release/neko_oracle.wasm"
    );
}

// ============================================================================
// Helpers
// ============================================================================

/// Asset symbol used in all tests
const CETES: fn(&Env) -> Symbol = |_e| symbol_short!("CETES");

fn create_token<'a>(env: &'a Env, admin: &Address) -> (TokenClient<'a>, StellarAssetClient<'a>) {
    let sac = env.register_stellar_asset_contract_v2(admin.clone());
    let token = TokenClient::new(env, &sac.address());
    let token_admin = StellarAssetClient::new(env, &sac.address());
    (token, token_admin)
}

fn create_neko_oracle(env: &Env) -> Address {
    let asset_cetes = neko_oracle::Asset::Other(symbol_short!("CETES"));
    let asset_usdc = neko_oracle::Asset::Other(symbol_short!("USDC"));
    let assets = soroban_sdk::vec![env, asset_cetes.clone(), asset_usdc.clone()];
    let admin = Address::generate(env);
    env.register(
        neko_oracle::WASM,
        (admin, assets, asset_usdc, 14u32, 300u32),
    )
}

fn create_lending_pool(env: &Env, token: &Address, oracle_addr: &Address) -> Address {
    use neko_pool::AssetType;

    // Pool deploy runs `__constructor` inside `register` (nested invocation); admin.require_auth
    // needs non-root auth recording to pass through nested calls.
    env.mock_all_auths_allowing_non_root_auth();

    let lending_admin = Address::generate(env);
    let treasury = Address::generate(env);
    let reflector = create_neko_oracle(env);
    let pool_id = env.register(
        neko_pool::WASM,
        (neko_pool::PoolInitConfig {
            admin: lending_admin.clone(),
            treasury,
            neko_oracle: oracle_addr.clone(),
            reflector_oracle: reflector,
            backstop_take_rate: 500_000u32,
            reserve_factor: 1_000_000u32,
            origination_fee_rate: 40_000u32,
            liquidation_fee_rate: 100_000u32,
        },),
    );
    let pool = neko_pool::Client::new(env, &pool_id);

    pool.set_token_contract(&CETES(env), token, &AssetType::Rwa);

    let params = neko_pool::InterestRateParams {
        target_util: 7_500_000,
        max_util: 9_500_000,
        r_base: 100_000,
        r_one: 500_000,
        r_two: 5_000_000,
        r_three: 15_000_000,
        reactivity: 200,
        l_factor: 10_000_000,
        supply_cap: 0,
        enabled: true,
    };
    pool.queue_set_reserve_params(&CETES(env), &params);
    pool.apply_queued_reserve_params(&CETES(env));

    pool.set_pool_state(&neko_pool::PoolState::Active);

    pool_id
}

fn create_adapter<'a>(
    env: &'a Env,
    admin: &Address,
    vault: &Address,
    lending_pool: &Address,
    deposit_token: &Address,
) -> NekoAdapterClient<'a> {
    let contract_id = env.register(NekoAdapter, ());
    let client = NekoAdapterClient::new(env, &contract_id);
    client.initialize(admin, vault, lending_pool, deposit_token, &CETES(env));
    client
}

// ============================================================================
// Tests
// ============================================================================

#[test]
fn test_adapter_initialize() {
    let env = Env::default();
    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let (token, _) = create_token(&env, &admin);
    let oracle = create_neko_oracle(&env);
    let pool = create_lending_pool(&env, &token.address, &oracle);

    let adapter = create_adapter(&env, &admin, &vault, &pool, &token.address);

    assert_eq!(adapter.get_vault(), vault);
    assert_eq!(adapter.get_lending_pool(), pool);
}

#[test]
fn test_adapter_balance_starts_zero() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let (token, _) = create_token(&env, &admin);
    let oracle = create_neko_oracle(&env);
    let pool = create_lending_pool(&env, &token.address, &oracle);

    let adapter = create_adapter(&env, &admin, &vault, &pool, &token.address);

    // No deposit yet → balance = 0
    assert_eq!(adapter.a_balance(&vault), 0i128);
}

#[test]
fn test_adapter_deposit_updates_balance() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let (token, token_admin) = create_token(&env, &admin);

    // Mint tokens to the adapter (vault normally does this before calling a_deposit)
    let oracle = create_neko_oracle(&env);
    let pool = create_lending_pool(&env, &token.address, &oracle);
    let adapter = create_adapter(&env, &admin, &vault, &pool, &token.address);

    let amount = 100_0000000i128; // 100 CETES
    token_admin.mint(&adapter.address, &amount);

    // a_deposit: adapter deposits into lending pool
    let balance_after = adapter.a_deposit(&amount, &vault);

    // Balance should reflect the deposited amount (initially 1:1 b_rate)
    assert!(balance_after > 0);
    assert!(balance_after <= amount); // may be slightly less due to rounding
}

#[test]
fn test_adapter_withdraw_returns_tokens() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let (token, token_admin) = create_token(&env, &admin);
    let oracle = create_neko_oracle(&env);
    let pool = create_lending_pool(&env, &token.address, &oracle);
    let adapter = create_adapter(&env, &admin, &vault, &pool, &token.address);

    let amount = 100_0000000i128;
    token_admin.mint(&adapter.address, &amount);

    // Deposit first
    adapter.a_deposit(&amount, &vault);

    let vault_balance_before = token.balance(&vault);

    // Withdraw half
    let withdraw_amount = 50_0000000i128;
    let actual = adapter.a_withdraw(&withdraw_amount, &vault);

    // Vault should have received tokens
    assert!(actual > 0);
    assert_eq!(token.balance(&vault), vault_balance_before + actual);
}

#[test]
fn test_adapter_apy_and_harvest() {
    let env = Env::default();
    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let (token, _) = create_token(&env, &admin);
    let oracle = create_neko_oracle(&env);
    let pool = create_lending_pool(&env, &token.address, &oracle);

    let adapter = create_adapter(&env, &admin, &vault, &pool, &token.address);

    // APY is 0 for now (placeholder)
    assert_eq!(adapter.a_get_apy(), 0u32);

    let (reward_token, amount) = adapter.a_harvest(&vault);
    assert_eq!(reward_token, token.address);
    assert_eq!(amount, 0i128);
}
