#![cfg(test)]
extern crate std;

use soroban_sdk::{
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    Address, Env,
};

use crate::{SoroswapAdapter, SoroswapAdapterClient};
use crate::common::error::Error;
use crate::common::types::SlippageConfig;

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
    let sac         = env.register_stellar_asset_contract_v2(admin.clone());
    let token       = TokenClient::new(env, &sac.address());
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

fn create_soroswap_fixture<'a>(env: &'a Env, admin: &Address) -> SoroswapFixture<'a> {
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();

    let (token_a, admin_a) = create_token(env, admin);
    let (token_b, admin_b) = create_token(env, admin);

    admin_a.mint(admin, &1_000_000_0000000i128);
    admin_b.mint(admin, &1_000_000_0000000i128);

    let pair_hash = env.deployer().upload_contract_wasm(soroswap_pair::WASM);

    let factory_addr = env.register(soroswap_factory::WASM, ());
    let factory      = soroswap_factory::Client::new(env, &factory_addr);
    factory.initialize(admin, &pair_hash);

    let router_addr = env.register(soroswap_router::WASM, ());
    let router      = soroswap_router::Client::new(env, &router_addr);
    router.initialize(&factory_addr);

    // 1:1 initial price, 100_000 of each token
    router.add_liquidity(
        &token_a.address,
        &token_b.address,
        &100_000_0000000i128,
        &100_000_0000000i128,
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
// Existing tests (unchanged behaviour)
// ============================================================================

#[test]
fn test_adapter_initialize() {
    let env   = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);

    let fixture = create_soroswap_fixture(&env, &admin);

    let adapter = create_adapter(
        &env, &admin, &vault,
        &fixture.router.address,
        &fixture.token_a.address,
        &fixture.token_b.address,
    );

    assert_eq!(adapter.get_vault(),   vault);
    assert_eq!(adapter.get_router(),  fixture.router.address);
    assert_eq!(adapter.get_token_a(), fixture.token_a.address);
    assert_eq!(adapter.get_token_b(), fixture.token_b.address);
    let pair = adapter.get_pair();
    assert_ne!(pair, Address::generate(&env));
}

#[test]
fn test_adapter_balance_starts_zero() {
    let env   = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);

    let fixture = create_soroswap_fixture(&env, &admin);

    let adapter = create_adapter(
        &env, &admin, &vault,
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

    let fixture = create_soroswap_fixture(&env, &admin);

    let adapter = create_adapter(
        &env, &admin, &vault,
        &fixture.router.address,
        &fixture.token_a.address,
        &fixture.token_b.address,
    );

    let amount = 1_000_0000000i128;
    let token_a_admin = StellarAssetClient::new(&env, &fixture.token_a.address);
    token_a_admin.mint(&adapter.address, &amount);

    let balance_after = adapter.a_deposit(&amount, &vault);

    assert!(balance_after > 0, "balance should be positive after deposit");
    assert_eq!(adapter.a_balance(&adapter.address), balance_after);
}

#[test]
fn test_adapter_withdraw_returns_tokens() {
    let env   = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);

    let fixture = create_soroswap_fixture(&env, &admin);

    let adapter = create_adapter(
        &env, &admin, &vault,
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
        &env, &admin, &vault,
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
        &env, &admin, &vault,
        &fixture.router.address,
        &fixture.token_a.address,
        &fixture.token_b.address,
    );

    let (reward_token, amount) = adapter.a_harvest(&vault);
    assert_eq!(reward_token, fixture.token_a.address);
    assert_eq!(amount, 0i128);
}

// ============================================================================
// NEW: SlippageConfig tests
// ============================================================================

#[test]
fn test_default_slippage_config_is_set_on_initialize() {
    let env   = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);

    let fixture = create_soroswap_fixture(&env, &admin);

    let adapter = create_adapter(
        &env, &admin, &vault,
        &fixture.router.address,
        &fixture.token_a.address,
        &fixture.token_b.address,
    );

    let cfg = adapter.get_slippage_config();
    assert_eq!(cfg.max_slippage_bps, 50,  "default slippage should be 50 bps");
    assert_eq!(cfg.max_price_impact_bps, 100, "default price impact should be 100 bps");
}

#[test]
fn test_admin_can_update_slippage_config() {
    let env   = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);

    let fixture = create_soroswap_fixture(&env, &admin);

    let adapter = create_adapter(
        &env, &admin, &vault,
        &fixture.router.address,
        &fixture.token_a.address,
        &fixture.token_b.address,
    );

    let new_cfg = SlippageConfig {
        max_slippage_bps: 30,
        max_price_impact_bps: 200,
    };
    adapter.set_slippage_config(&admin, &new_cfg);

    let cfg = adapter.get_slippage_config();
    assert_eq!(cfg.max_slippage_bps, 30);
    assert_eq!(cfg.max_price_impact_bps, 200);
}

#[test]
fn test_non_admin_cannot_update_slippage_config() {
    let env   = Env::default();
    env.mock_all_auths();

    let admin     = Address::generate(&env);
    let vault     = Address::generate(&env);
    let non_admin = Address::generate(&env);

    let fixture = create_soroswap_fixture(&env, &admin);

    let adapter = create_adapter(
        &env, &admin, &vault,
        &fixture.router.address,
        &fixture.token_a.address,
        &fixture.token_b.address,
    );

    let result = std::panic::catch_unwind(|| {
        adapter.set_slippage_config(&non_admin, &SlippageConfig {
            max_slippage_bps: 500,
            max_price_impact_bps: 500,
        });
    });

    assert!(result.is_err(), "non-admin should not be able to set slippage config");
}

// ============================================================================
// NEW: Fair-value / NAV manipulation resistance
// ============================================================================

/// Core acceptance criterion from issue #49:
/// A 10% single-swap skew of reserve_a / reserve_b must change
/// `get_fair_value` by ≤ 0.5%.
#[test]
fn test_fair_value_resistant_to_10pct_reserve_skew() {
    let env   = Env::default();
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);

    let fixture = create_soroswap_fixture(&env, &admin);

    // Use very loose slippage so the skew swap itself doesn't revert.
    let adapter = create_adapter(
        &env, &admin, &vault,
        &fixture.router.address,
        &fixture.token_a.address,
        &fixture.token_b.address,
    );
    adapter.set_slippage_config(&admin, &SlippageConfig {
        max_slippage_bps: 9_000,
        max_price_impact_bps: 9_000,
    });

    // Deposit into the adapter
    let deposit_amount = 10_000_0000000i128; // 10_000 tokens
    let token_a_admin = StellarAssetClient::new(&env, &fixture.token_a.address);
    token_a_admin.mint(&adapter.address, &deposit_amount);
    adapter.a_deposit(&deposit_amount, &vault);

    let fair_before = adapter.get_fair_value(&adapter.address);
    assert!(fair_before > 0, "fair value must be positive after deposit");

    // Attacker skews the pool by swapping ~10% of reserve_a (100_000 tokens)
    // into token_b directly via the router (bypassing the adapter).
    let attacker      = Address::generate(&env);
    let skew_amount   = 10_000_0000000i128; // 10% of initial 100_000 reserve
    token_a_admin.mint(&attacker, &skew_amount);

    fixture.router.swap_exact_tokens_for_tokens(
        &skew_amount,
        &0i128,
        &soroban_sdk::vec![&env, fixture.token_a.address.clone(), fixture.token_b.address.clone()],
        &attacker,
        &(env.ledger().timestamp() + 3600),
    );

    let fair_after = adapter.get_fair_value(&adapter.address);

    // Change must be ≤ 0.5% (50 bps)
    let diff = (fair_after - fair_before).abs();
    // diff / fair_before ≤ 0.005  →  diff × 10_000 ≤ fair_before × 50
    let lhs = diff.checked_mul(10_000).unwrap_or(i128::MAX);
    let rhs = fair_before.checked_mul(50).unwrap_or(0);
    assert!(
        lhs <= rhs,
        "fair value changed by more than 0.5% after 10% reserve skew: \
         before={fair_before}, after={fair_after}, diff={diff}"
    );
}

/// The old spot-price formula (share_a + share_b × reserve_a/reserve_b) is
/// manipulable; the new fair-value formula should be substantially more stable.
/// This test documents the improvement by comparing both.
#[test]
fn test_fair_value_more_stable_than_spot_price_after_skew() {
    let env   = Env::default();
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);

    let fixture = create_soroswap_fixture(&env, &admin);

    let adapter = create_adapter(
        &env, &admin, &vault,
        &fixture.router.address,
        &fixture.token_a.address,
        &fixture.token_b.address,
    );
    adapter.set_slippage_config(&admin, &SlippageConfig {
        max_slippage_bps: 9_000,
        max_price_impact_bps: 9_000,
    });

    let deposit_amount = 10_000_0000000i128;
    let token_a_admin = StellarAssetClient::new(&env, &fixture.token_a.address);
    token_a_admin.mint(&adapter.address, &deposit_amount);
    adapter.a_deposit(&deposit_amount, &vault);

    let fair_before = adapter.get_fair_value(&adapter.address);
    let bal_before  = adapter.a_balance(&adapter.address);

    // Skew 20% of reserve
    let attacker    = Address::generate(&env);
    let skew_amount = 20_000_0000000i128;
    token_a_admin.mint(&attacker, &skew_amount);
    fixture.router.swap_exact_tokens_for_tokens(
        &skew_amount,
        &0i128,
        &soroban_sdk::vec![&env, fixture.token_a.address.clone(), fixture.token_b.address.clone()],
        &attacker,
        &(env.ledger().timestamp() + 3600),
    );

    let fair_after = adapter.get_fair_value(&adapter.address);
    let bal_after  = adapter.a_balance(&adapter.address);

    let fair_change = (fair_after - fair_before).abs();
    let bal_change  = (bal_after - bal_before).abs();

    // fair_value change should be strictly less than a_balance (spot) change
    assert!(
        fair_change <= bal_change,
        "fair_value should be at least as stable as spot-price balance: \
         fair_change={fair_change}, bal_change={bal_change}"
    );
}

// ============================================================================
// NEW: Price-impact estimation
// ============================================================================

#[test]
fn test_price_impact_bps_small_trade() {
    let env   = Env::default();
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);

    let fixture = create_soroswap_fixture(&env, &admin);

    let adapter = create_adapter(
        &env, &admin, &vault,
        &fixture.router.address,
        &fixture.token_a.address,
        &fixture.token_b.address,
    );

    // 100 tokens against 100_000 reserve → impact ≈ 100/100_100 ≈ 10 bps
    let impact = adapter.get_price_impact_bps(&100_0000000i128);
    assert!(impact < 200, "small trade should have low price impact: {impact} bps");
    assert!(impact > 0,   "impact should be non-zero for non-trivial trade");
}

#[test]
fn test_price_impact_bps_large_trade() {
    let env   = Env::default();
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);

    let fixture = create_soroswap_fixture(&env, &admin);

    let adapter = create_adapter(
        &env, &admin, &vault,
        &fixture.router.address,
        &fixture.token_a.address,
        &fixture.token_b.address,
    );

    // 50_000 tokens against 100_000 reserve → impact ≈ 33%
    let impact = adapter.get_price_impact_bps(&50_000_0000000i128);
    assert!(impact > 2_000, "large trade should have significant price impact: {impact} bps");
}

// ============================================================================
// NEW: Slippage enforcement — deposit rejected when price impact too high
// ============================================================================

#[test]
fn test_deposit_rejected_when_price_impact_exceeds_limit() {
    let env   = Env::default();
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);

    let fixture = create_soroswap_fixture(&env, &admin);

    let adapter = create_adapter(
        &env, &admin, &vault,
        &fixture.router.address,
        &fixture.token_a.address,
        &fixture.token_b.address,
    );

    // Tighten price impact limit to 5 bps — any meaningful trade will exceed this
    adapter.set_slippage_config(&admin, &SlippageConfig {
        max_slippage_bps: 50,
        max_price_impact_bps: 5, // extremely tight
    });

    // A 1_000 token deposit will try to swap 500 tokens → ~50 bps impact on 100k pool
    let amount = 1_000_0000000i128;
    let token_a_admin = StellarAssetClient::new(&env, &fixture.token_a.address);
    token_a_admin.mint(&adapter.address, &amount);

    let result = std::panic::catch_unwind(|| {
        adapter.a_deposit(&amount, &vault);
    });

    assert!(result.is_err(), "deposit should be rejected when price impact exceeds limit");
}

// ============================================================================
// NEW: Zero-liquidity edge case
// ============================================================================

#[test]
fn test_fair_value_zero_when_no_position() {
    let env   = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);

    let fixture = create_soroswap_fixture(&env, &admin);

    let adapter = create_adapter(
        &env, &admin, &vault,
        &fixture.router.address,
        &fixture.token_a.address,
        &fixture.token_b.address,
    );

    assert_eq!(adapter.get_fair_value(&adapter.address), 0i128);
    assert_eq!(adapter.a_balance(&adapter.address),      0i128);
}

#[test]
fn test_withdraw_zero_when_no_position() {
    let env   = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);

    let fixture = create_soroswap_fixture(&env, &admin);

    let adapter = create_adapter(
        &env, &admin, &vault,
        &fixture.router.address,
        &fixture.token_a.address,
        &fixture.token_b.address,
    );

    // Withdraw on empty position must return 0 without panicking
    let result = adapter.a_withdraw(&1_000_0000000i128, &vault);
    assert_eq!(result, 0i128);
}

// ============================================================================
// NEW: Happy path deposit/withdraw with slippage within bounds
// ============================================================================

#[test]
fn test_deposit_and_withdraw_within_slippage_bounds() {
    let env   = Env::default();
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);

    let fixture = create_soroswap_fixture(&env, &admin);

    // Reasonable config: 1% slippage, 2% price impact
    let adapter = create_adapter(
        &env, &admin, &vault,
        &fixture.router.address,
        &fixture.token_a.address,
        &fixture.token_b.address,
    );
    adapter.set_slippage_config(&admin, &SlippageConfig {
        max_slippage_bps: 100,
        max_price_impact_bps: 200,
    });

    let amount = 1_000_0000000i128;
    let token_a_admin = StellarAssetClient::new(&env, &fixture.token_a.address);
    token_a_admin.mint(&adapter.address, &amount);

    // Deposit should succeed
    let pos = adapter.a_deposit(&amount, &vault);
    assert!(pos > 0, "position should be positive");

    // Fair value should be close to deposited amount (within 2% AMM fees + slippage)
    let fair = adapter.get_fair_value(&adapter.address);
    assert!(fair > 0);
    let deviation = (fair - amount).abs();
    // Allow 3% deviation for AMM fees and rounding
    assert!(
        deviation * 100 <= amount * 3,
        "fair value should be within 3% of deposit: amount={amount}, fair={fair}"
    );

    // Withdraw should succeed and return meaningful amount
    let vault_before = fixture.token_a.balance(&vault);
    let actual       = adapter.a_withdraw(&pos, &vault);
    assert!(actual > 0, "withdraw should return positive amount");
    assert_eq!(fixture.token_a.balance(&vault), vault_before + actual);

    // After full withdraw, position should be (near) zero
    let pos_after = adapter.a_balance(&adapter.address);
    // Allow small dust from integer rounding
    assert!(pos_after < 1_000, "position should be near zero after full withdraw: {pos_after}");
}