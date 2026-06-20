#![cfg(test)]
extern crate std;

use soroban_sdk::{
    testutils::{Address as _, BytesN as _},
    token::{StellarAssetClient, TokenClient},
    vec, Address, BytesN, Env, String,
};

use crate::{BlendAdapter, BlendAdapterClient};

// ============================================================================
// Blend WASM imports
//
// Build order before running tests:
//   Copy Blend WASMs into stellar-contracts/external_wasms/blend/:
//     pool.wasm, backstop.wasm, pool_factory.wasm, emitter.wasm, comet.wasm
//   Source: https://github.com/blend-capital/blend-contracts/releases
// ============================================================================

mod blend_pool {
    soroban_sdk::contractimport!(file = "../../wasms/external_wasms/blend/pool.wasm");
}
mod blend_backstop {
    soroban_sdk::contractimport!(file = "../../wasms/external_wasms/blend/backstop.wasm");
}
mod blend_pool_factory {
    soroban_sdk::contractimport!(file = "../../wasms/external_wasms/blend/pool_factory.wasm");
}
mod blend_emitter {
    soroban_sdk::contractimport!(file = "../../wasms/external_wasms/blend/emitter.wasm");
}
mod blend_comet {
    soroban_sdk::contractimport!(file = "../../wasms/external_wasms/blend/comet.wasm");
}

// ============================================================================
// Token helper
// ============================================================================

fn create_token<'a>(env: &'a Env, admin: &Address) -> (TokenClient<'a>, StellarAssetClient<'a>) {
    let sac = env.register_stellar_asset_contract_v2(admin.clone());
    let token = TokenClient::new(env, &sac.address());
    let token_admin = StellarAssetClient::new(env, &sac.address());
    (token, token_admin)
}

// ============================================================================
// Blend fixture
//
// Matches the initialization sequence used in DeFindex's blend_setup.rs:
//   comet (LP) → emitter → backstop → pool_factory → pool
// ============================================================================

struct BlendFixture<'a> {
    pub pool_addr: Address,
    pub blnd_token: TokenClient<'a>,
}

fn create_blend_fixture<'a>(
    env: &'a Env,
    admin: &Address,
    deposit_token: &Address,
) -> BlendFixture<'a> {
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();

    // ── BLND token ────────────────────────────────────────────────────────
    let (blnd_token, blnd_admin) = create_token(env, admin);
    let blnd_addr = blnd_token.address.clone();

    // ── USDC token (used as backstop LP pair) ─────────────────────────────
    let (usdc_token, usdc_admin) = create_token(env, admin);
    let usdc_addr = usdc_token.address.clone();

    // Mint BLND + USDC to admin for Comet initialization and join_pool
    blnd_admin.mint(admin, &(1_000_0000000i128 * 2001));
    usdc_admin.mint(admin, &(25_0000000i128 * 2001));

    // ── Comet LP token (80/20 BLND-USDC pool) ─────────────────────────────
    let comet_addr = env.register(blend_comet::WASM, ());
    let comet = blend_comet::Client::new(env, &comet_addr);

    // init(factory, tokens, weights, balances, swap_fee)
    comet.init(
        admin,
        &vec![env, blnd_addr.clone(), usdc_addr.clone()],
        &vec![env, 0_8000000i128, 0_2000000i128],
        &vec![env, 1_000_0000000i128, 25_0000000i128],
        &0_0030000i128,
    );
    // Mint comet LP tokens to admin (2000x the initial ratio)
    comet.join_pool(
        &199_900_0000000i128,
        &vec![env, 1_000_0000000i128 * 2000, 25_0000000i128 * 2000],
        admin,
    );

    // ── Pre-compute addresses for chicken-and-egg dependency ──────────────
    let backstop_id = Address::generate(env);
    let pool_factory_id = Address::generate(env);

    // ── Emitter ───────────────────────────────────────────────────────────
    let emitter_addr = env.register(blend_emitter::WASM, ());
    let emitter = blend_emitter::Client::new(env, &emitter_addr);

    // Transfer BLND minting authority to emitter
    blnd_admin.set_admin(&emitter_addr);

    // initialize(blnd, backstop, backstop_token/comet)
    emitter.initialize(&blnd_addr, &backstop_id, &comet_addr);

    // ── Backstop ──────────────────────────────────────────────────────────
    let empty_drop_list: soroban_sdk::Vec<(Address, i128)> = vec![env];
    env.register_at(
        &backstop_id,
        blend_backstop::WASM,
        (
            &comet_addr,        // backstop_token (comet LP)
            &emitter_addr,      // emitter
            &blnd_addr,         // blnd_token
            &usdc_addr,         // usdc_token
            &pool_factory_id,   // pool_factory (pre-computed)
            &empty_drop_list,   // drop_list
        ),
    );
    let backstop = blend_backstop::Client::new(env, &backstop_id);

    // ── Pool Factory ──────────────────────────────────────────────────────
    let pool_hash = env.deployer().upload_contract_wasm(blend_pool::WASM);
    env.register_at(
        &pool_factory_id,
        blend_pool_factory::WASM,
        (blend_pool_factory::PoolInitMeta {
            backstop: backstop_id.clone(),
            pool_hash: pool_hash.clone(),
            blnd_id: blnd_addr.clone(),
        },),
    );
    let pool_factory = blend_pool_factory::Client::new(env, &pool_factory_id);

    // Start backstop distribution period
    backstop.distribute();

    // ── Deploy Pool ───────────────────────────────────────────────────────
    // oracle is not queried for supply/withdraw, so any address works for tests
    let oracle_addr = Address::generate(env);

    let pool_addr = pool_factory.deploy(
        admin,
        &String::from_str(env, "TestPool"),
        &BytesN::random(env),
        &oracle_addr,
        &0u32,          // backstop_take_rate
        &4u32,          // max_positions
        &1_0000000i128, // min_backstop (1 token)
    );
    let pool = blend_pool::Client::new(env, &pool_addr);

    // Deposit comet LP tokens to backstop — matching DeFindex: 20_000 LP tokens
    // (each LP ≈ 0.25 USDC; reward zone threshold requires a large enough share)
    backstop.deposit(admin, &pool_addr, &20_0000_0000000i128);

    // ── Configure reserve for deposit_token ───────────────────────────────
    pool.queue_set_reserve(
        deposit_token,
        &blend_pool::ReserveConfig {
            index: 0,
            decimals: 7,
            c_factor: 7_500_000,
            l_factor: 7_500_000,
            util: 7_500_000,
            max_util: 9_500_000,
            r_base: 100_000,
            r_one: 500_000,
            r_two: 5_000_000,
            r_three: 15_000_000,
            reactivity: 200,
            supply_cap: i128::MAX,
            enabled: true,
        },
    );
    pool.set_reserve(deposit_token);

    // Configure emissions for reserve 0 (supply + borrow), then enter reward zone, then activate
    pool.set_emissions_config(&vec![
        env,
        blend_pool::ReserveEmissionMetadata {
            res_index: 0,
            res_type: 0,
            share: 500_0000,
        },
        blend_pool::ReserveEmissionMetadata {
            res_index: 0,
            res_type: 1,
            share: 500_0000,
        },
    ]);
    backstop.add_reward(&pool_addr, &None);
    pool.set_status(&0u32);

    let _ = pool; // only needed during setup
    BlendFixture {
        pool_addr,
        blnd_token,
    }
}

fn create_adapter<'a>(
    env: &'a Env,
    admin: &Address,
    vault: &Address,
    pool_addr: &Address,
    deposit_token: &Address,
    blend_token: &Address,
) -> BlendAdapterClient<'a> {
    let id = env.register(BlendAdapter, ());
    let client = BlendAdapterClient::new(env, &id);
    client.initialize(admin, vault, pool_addr, deposit_token, blend_token);
    client
}

// ============================================================================
// Tests
// ============================================================================

#[test]
fn test_adapter_initialize() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let (deposit_token, _) = create_token(&env, &admin);

    let fixture = create_blend_fixture(&env, &admin, &deposit_token.address);

    let adapter = create_adapter(
        &env,
        &admin,
        &vault,
        &fixture.pool_addr,
        &deposit_token.address,
        &fixture.blnd_token.address,
    );

    assert_eq!(adapter.get_vault(), vault);
    assert_eq!(adapter.get_blend_pool(), fixture.pool_addr);
    assert_eq!(adapter.get_reserve_id(), 0u32);
}

#[test]
fn test_adapter_balance_starts_zero() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let (deposit_token, _) = create_token(&env, &admin);

    let fixture = create_blend_fixture(&env, &admin, &deposit_token.address);

    let adapter = create_adapter(
        &env,
        &admin,
        &vault,
        &fixture.pool_addr,
        &deposit_token.address,
        &fixture.blnd_token.address,
    );

    assert_eq!(adapter.a_balance(&adapter.address), 0i128);
}

#[test]
fn test_adapter_deposit_creates_position() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let (deposit_token, deposit_admin) = create_token(&env, &admin);

    let fixture = create_blend_fixture(&env, &admin, &deposit_token.address);

    let adapter = create_adapter(
        &env,
        &admin,
        &vault,
        &fixture.pool_addr,
        &deposit_token.address,
        &fixture.blnd_token.address,
    );

    let amount = 100_0000000i128;
    // Vault transfers tokens to adapter before calling a_deposit
    deposit_admin.mint(&adapter.address, &amount);

    let balance_after = adapter.a_deposit(&amount, &vault);

    assert!(balance_after > 0);
    assert!(balance_after <= amount);
    assert_eq!(adapter.a_balance(&adapter.address), balance_after);
}

#[test]
fn test_adapter_withdraw_returns_tokens() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let (deposit_token, deposit_admin) = create_token(&env, &admin);

    let fixture = create_blend_fixture(&env, &admin, &deposit_token.address);

    let adapter = create_adapter(
        &env,
        &admin,
        &vault,
        &fixture.pool_addr,
        &deposit_token.address,
        &fixture.blnd_token.address,
    );

    let amount = 100_0000000i128;
    deposit_admin.mint(&adapter.address, &amount);
    adapter.a_deposit(&amount, &vault);

    let vault_balance_before = deposit_token.balance(&vault);

    let actual = adapter.a_withdraw(&50_0000000i128, &vault);

    assert!(actual > 0);
    assert_eq!(deposit_token.balance(&vault), vault_balance_before + actual);
}

#[test]
fn test_adapter_apy_returns_zero() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let (deposit_token, _) = create_token(&env, &admin);

    let fixture = create_blend_fixture(&env, &admin, &deposit_token.address);

    let adapter = create_adapter(
        &env,
        &admin,
        &vault,
        &fixture.pool_addr,
        &deposit_token.address,
        &fixture.blnd_token.address,
    );

    assert_eq!(adapter.a_get_apy(), 0u32);
}

#[test]
fn test_adapter_harvest() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let (deposit_token, _) = create_token(&env, &admin);

    let fixture = create_blend_fixture(&env, &admin, &deposit_token.address);

    let adapter = create_adapter(
        &env,
        &admin,
        &vault,
        &fixture.pool_addr,
        &deposit_token.address,
        &fixture.blnd_token.address,
    );

    let (reward_token, amount) = adapter.a_harvest(&vault);
    assert_eq!(reward_token, fixture.blnd_token.address);
    assert_eq!(amount, 0i128);
}
