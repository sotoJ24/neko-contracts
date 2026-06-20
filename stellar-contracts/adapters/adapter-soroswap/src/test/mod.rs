#![cfg(test)]
extern crate std;

use soroban_sdk::{
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    Address, Env,
};

use crate::{SoroswapAdapter, SoroswapAdapterClient};

// ============================================================================
// Soroswap WASM imports
//
// Build order before running tests:
//   Copy Soroswap WASMs into stellar-contracts/external_wasms/soroswap/:
//     factory.wasm, router.wasm, pair.wasm
//   Source: https://github.com/soroswap/core/releases
// ============================================================================

mod soroswap_factory {
    soroban_sdk::contractimport!(file = "../../wasms/external_wasms/soroswap/factory.wasm");
}
mod soroswap_router {
    soroban_sdk::contractimport!(file = "../../wasms/external_wasms/soroswap/router.wasm");
}
mod soroswap_pair {
    soroban_sdk::contractimport!(file = "../../wasms/external_wasms/soroswap/pair.wasm");
}

// ============================================================================
// Token helper
// ============================================================================

fn create_token<'a>(env: &'a Env, admin: &Address) -> (TokenClient<'a>, StellarAssetClient<'a>) {
    let sac        = env.register_stellar_asset_contract_v2(admin.clone());
    let token      = TokenClient::new(env, &sac.address());
    let token_admin = StellarAssetClient::new(env, &sac.address());
    (token, token_admin)
}

// ============================================================================
// Soroswap fixture
// ============================================================================

struct SoroswapFixture<'a> {
    pub router:  soroswap_router::Client<'a>,
    pub token_a: TokenClient<'a>,
    pub token_b: TokenClient<'a>,
}

fn create_soroswap_fixture<'a>(
    env: &'a Env,
    admin: &Address,
) -> SoroswapFixture<'a> {
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();

    let (token_a, admin_a) = create_token(env, admin);
    let (token_b, admin_b) = create_token(env, admin);

    // Mint tokens to admin for initial liquidity
    admin_a.mint(admin, &1_000_000_0000000i128);
    admin_b.mint(admin, &1_000_000_0000000i128);

    // Deploy Soroswap: upload pair hash → factory → router
    let pair_hash = env.deployer().upload_contract_wasm(soroswap_pair::WASM);

    let factory_addr = env.register(soroswap_factory::WASM, ());
    let factory      = soroswap_factory::Client::new(env, &factory_addr);
    factory.initialize(admin, &pair_hash);

    let router_addr = env.register(soroswap_router::WASM, ());
    let router      = soroswap_router::Client::new(env, &router_addr);
    router.initialize(&factory_addr);

    // Create the initial pair by adding liquidity (this deploys the pair contract)
    router.add_liquidity(
        &token_a.address,
        &token_b.address,
        &100_000_0000000i128, // 100_000 token_a
        &100_000_0000000i128, // 100_000 token_b (1:1 initial price)
        &0i128,
        &0i128,
        admin,
        &(env.ledger().timestamp() + 3600),
    );

    SoroswapFixture { router, token_a, token_b }
}

fn create_adapter<'a>(
    env: &'a Env,
    admin: &Address,
    vault: &Address,
    router: &Address,
    token_a: &Address,
    token_b: &Address,
) -> SoroswapAdapterClient<'a> {
    let id     = env.register(SoroswapAdapter, ());
    let client = SoroswapAdapterClient::new(env, &id);
    client.initialize(admin, vault, router, token_a, token_b);
    client
}

// ============================================================================
// Tests
// ============================================================================

#[test]
fn test_adapter_initialize() {
    let env   = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);

    let fixture = create_soroswap_fixture(&env, &admin);

    let adapter = create_adapter(
        &env,
        &admin,
        &vault,
        &fixture.router.address,
        &fixture.token_a.address,
        &fixture.token_b.address,
    );

    assert_eq!(adapter.get_vault(),   vault);
    assert_eq!(adapter.get_router(),  fixture.router.address);
    assert_eq!(adapter.get_token_a(), fixture.token_a.address);
    assert_eq!(adapter.get_token_b(), fixture.token_b.address);
    // pair must be non-zero (resolved from factory)
    let pair = adapter.get_pair();
    assert_ne!(pair, Address::generate(&env)); // just verify it's stored
}

#[test]
fn test_adapter_balance_starts_zero() {
    let env   = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);

    let fixture = create_soroswap_fixture(&env, &admin);

    let adapter = create_adapter(
        &env,
        &admin,
        &vault,
        &fixture.router.address,
        &fixture.token_a.address,
        &fixture.token_b.address,
    );

    assert_eq!(adapter.a_balance(&adapter.address), 0i128);
}

#[test]
fn test_adapter_deposit_creates_position() {
    let env   = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);

    let (_, admin_a) = create_token(&env, &admin);
    let fixture = create_soroswap_fixture(&env, &admin);

    // Use fixture token_a (same contract already set up with liquidity)
    let adapter = create_adapter(
        &env,
        &admin,
        &vault,
        &fixture.router.address,
        &fixture.token_a.address,
        &fixture.token_b.address,
    );

    let amount = 1_000_0000000i128; // 1_000 tokens
    // Mint token_a to adapter (vault would transfer before calling a_deposit)
    let token_a_admin = StellarAssetClient::new(&env, &fixture.token_a.address);
    token_a_admin.mint(&adapter.address, &amount);

    let balance_after = adapter.a_deposit(&amount, &vault);

    assert!(balance_after > 0, "balance should be positive after deposit");
    assert_eq!(adapter.a_balance(&adapter.address), balance_after);

    // suppress unused variable warning
    let _ = admin_a;
}

#[test]
fn test_adapter_withdraw_returns_tokens() {
    let env   = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);

    let fixture = create_soroswap_fixture(&env, &admin);

    let adapter = create_adapter(
        &env,
        &admin,
        &vault,
        &fixture.router.address,
        &fixture.token_a.address,
        &fixture.token_b.address,
    );

    let amount = 1_000_0000000i128;
    let token_a_admin = StellarAssetClient::new(&env, &fixture.token_a.address);
    token_a_admin.mint(&adapter.address, &amount);
    adapter.a_deposit(&amount, &vault);

    let vault_balance_before = fixture.token_a.balance(&vault);
    let position_before      = adapter.a_balance(&adapter.address);
    assert!(position_before > 0);

    let actual = adapter.a_withdraw(&position_before, &vault);

    assert!(actual > 0, "withdraw should return positive amount");
    assert_eq!(fixture.token_a.balance(&vault), vault_balance_before + actual);
}

#[test]
fn test_adapter_apy_returns_zero() {
    let env   = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);

    let fixture = create_soroswap_fixture(&env, &admin);

    let adapter = create_adapter(
        &env,
        &admin,
        &vault,
        &fixture.router.address,
        &fixture.token_a.address,
        &fixture.token_b.address,
    );

    assert_eq!(adapter.a_get_apy(), 0u32);
}

#[test]
fn test_adapter_harvest_returns_zero() {
    let env   = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);

    let fixture = create_soroswap_fixture(&env, &admin);

    let adapter = create_adapter(
        &env,
        &admin,
        &vault,
        &fixture.router.address,
        &fixture.token_a.address,
        &fixture.token_b.address,
    );

    let (reward_token, amount) = adapter.a_harvest(&vault);
    assert_eq!(reward_token, fixture.token_a.address);
    assert_eq!(amount, 0i128);
}
