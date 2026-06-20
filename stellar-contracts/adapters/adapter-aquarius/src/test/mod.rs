#![cfg(test)]
extern crate std;

use soroban_sdk::{
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    vec, Address, Env,
};

use crate::{AquariusAdapter, AquariusAdapterClient};

// ============================================================================
// Mock Aquarius pool
//
// Implements the same interface as soroban_liquidity_pool_contract but as a
// native Soroban contract so it compiles against our soroban-sdk version.
//
// Uses constant-product (x·y = k) math with 0.3% fee (30 bps).
// Reserves and shares are stored in instance storage.
// ============================================================================

mod mock_pool {
    use soroban_sdk::{
        contract, contractimpl, contracttype,
        token::{StellarAssetClient, TokenClient},
        vec, Address, Env, Vec,
    };

    const FEE_BPS: u128 = 30; // 0.3%
    const BPS: u128 = 10_000;

    #[contracttype]
    pub struct PoolState {
        pub token_a:   Address,
        pub token_b:   Address,
        pub share_tok: Address,
        pub reserve_a: u128,
        pub reserve_b: u128,
        pub shares:    u128,
    }

    #[contracttype]
    enum Key {
        State,
    }

    fn load(env: &Env) -> PoolState {
        env.storage().instance().get(&Key::State).unwrap()
    }

    fn save(env: &Env, s: &PoolState) {
        env.storage().instance().set(&Key::State, s);
    }

    #[contract]
    pub struct MockPool;

    #[contractimpl]
    impl MockPool {
        pub fn initialize(
            env:     Env,
            token_a: Address,
            token_b: Address,
            share_tok: Address,
        ) {
            save(
                &env,
                &PoolState {
                    token_a,
                    token_b,
                    share_tok,
                    reserve_a: 0,
                    reserve_b: 0,
                    shares: 0,
                },
            );
        }

        pub fn get_tokens(env: Env) -> Vec<Address> {
            let s = load(&env);
            vec![&env, s.token_a, s.token_b]
        }

        pub fn share_id(env: Env) -> Address {
            load(&env).share_tok
        }

        pub fn get_total_shares(env: Env) -> u128 {
            load(&env).shares
        }

        pub fn get_reserves(env: Env) -> Vec<u128> {
            let s = load(&env);
            vec![&env, s.reserve_a, s.reserve_b]
        }

        /// Estimate swap output using constant-product with fee.
        pub fn estimate_swap(env: Env, in_idx: u32, _out_idx: u32, in_amount: u128) -> u128 {
            let s = load(&env);
            let (r_in, r_out) = if in_idx == 0 {
                (s.reserve_a, s.reserve_b)
            } else {
                (s.reserve_b, s.reserve_a)
            };
            if r_in == 0 || r_out == 0 {
                return 0;
            }
            let in_after_fee = in_amount * (BPS - FEE_BPS) / BPS;
            r_out * in_after_fee / (r_in + in_after_fee)
        }

        /// Estimate shares for deposit (proportional to reserves).
        pub fn estimate_deposit(env: Env, desired: Vec<u128>) -> u128 {
            let s = load(&env);
            if s.shares == 0 || s.reserve_a == 0 {
                // First deposit: shares = geometric mean
                let a = desired.get(0).unwrap_or(0);
                let b = desired.get(1).unwrap_or(0);
                // approximate sqrt(a*b) as min to keep it simple
                return a.min(b);
            }
            let a = desired.get(0).unwrap_or(0);
            (a * s.shares) / s.reserve_a
        }

        /// Swap token in_idx → out_idx.
        pub fn swap(
            env:      Env,
            user:     Address,
            in_idx:   u32,
            out_idx:  u32,
            in_amount: u128,
            out_min:  u128,
        ) -> u128 {
            let mut s = load(&env);

            let out = Self::estimate_swap(env.clone(), in_idx, out_idx, in_amount);
            assert!(out >= out_min, "slippage exceeded");

            // Pull token_in from user (user pre-authorized via adapter)
            let (tok_in, tok_out) = if in_idx == 0 {
                (s.token_a.clone(), s.token_b.clone())
            } else {
                (s.token_b.clone(), s.token_a.clone())
            };

            // Receive token_in
            let pool = env.current_contract_address();
            TokenClient::new(&env, &tok_in).transfer(&user, &pool, &(in_amount as i128));

            // Send token_out
            TokenClient::new(&env, &tok_out).transfer(&pool, &user, &(out as i128));

            // Update reserves
            if in_idx == 0 {
                s.reserve_a += in_amount;
                s.reserve_b -= out;
            } else {
                s.reserve_b += in_amount;
                s.reserve_a -= out;
            }
            save(&env, &s);
            out
        }

        /// Deposit tokens and mint LP shares.
        pub fn deposit(
            env:      Env,
            user:     Address,
            desired:  Vec<u128>,
            min_shares: u128,
        ) -> (Vec<u128>, u128) {
            let mut s = load(&env);
            let pool = env.current_contract_address();

            let a = desired.get(0).unwrap_or(0);
            let b = desired.get(1).unwrap_or(0);

            // Pull tokens from user
            if a > 0 {
                TokenClient::new(&env, &s.token_a).transfer(&user, &pool, &(a as i128));
            }
            if b > 0 {
                TokenClient::new(&env, &s.token_b).transfer(&user, &pool, &(b as i128));
            }

            // Compute shares to mint
            let new_shares = if s.shares == 0 {
                a.min(b)
            } else {
                (a * s.shares) / s.reserve_a
            };
            assert!(new_shares >= min_shares, "insufficient shares");

            // Mint LP tokens
            StellarAssetClient::new(&env, &s.share_tok).mint(&user, &(new_shares as i128));

            s.reserve_a += a;
            s.reserve_b += b;
            s.shares += new_shares;
            save(&env, &s);

            (vec![&env, a, b], new_shares)
        }

        /// Burn LP shares and return tokens proportionally.
        pub fn withdraw(
            env:        Env,
            user:       Address,
            share_amount: u128,
            min_amounts: Vec<u128>,
        ) -> Vec<u128> {
            let mut s = load(&env);

            if s.shares == 0 {
                return vec![&env, 0u128, 0u128];
            }

            let out_a = s.reserve_a * share_amount / s.shares;
            let out_b = s.reserve_b * share_amount / s.shares;

            assert!(out_a >= min_amounts.get(0).unwrap_or(0), "slippage A");
            assert!(out_b >= min_amounts.get(1).unwrap_or(0), "slippage B");

            let pool = env.current_contract_address();

            // Burn shares from user
            TokenClient::new(&env, &s.share_tok).burn(&user, &(share_amount as i128));

            // Transfer tokens to user
            if out_a > 0 {
                TokenClient::new(&env, &s.token_a).transfer(&pool, &user, &(out_a as i128));
            }
            if out_b > 0 {
                TokenClient::new(&env, &s.token_b).transfer(&pool, &user, &(out_b as i128));
            }

            s.reserve_a -= out_a;
            s.reserve_b -= out_b;
            s.shares -= share_amount;
            save(&env, &s);

            vec![&env, out_a, out_b]
        }

        /// Claim AQUA rewards (returns 0 in mock — no rewards configured).
        pub fn claim(_env: Env, _user: Address) -> u128 {
            0
        }
    }
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
// Test fixture
// ============================================================================

struct Fixture<'a> {
    pub pool:          mock_pool::MockPoolClient<'a>,
    pub deposit_token: TokenClient<'a>,
    pub pair_token:    TokenClient<'a>,
    pub aqua_token:    TokenClient<'a>,
}

fn create_fixture<'a>(env: &'a Env, admin: &Address) -> Fixture<'a> {
    env.mock_all_auths_allowing_non_root_auth();

    let (deposit_token, deposit_admin) = create_token(env, admin); // CETES
    let (pair_token,    pair_admin)    = create_token(env, admin); // USDC
    let (aqua_token,    _)             = create_token(env, admin); // AQUA

    // LP share token — the pool mints shares directly so we give mint authority to pool
    let share_sac  = env.register_stellar_asset_contract_v2(admin.clone());
    let share_addr = share_sac.address();

    // Deploy and initialize the mock pool
    let pool_id = env.register(mock_pool::MockPool, ());
    let pool    = mock_pool::MockPoolClient::new(env, &pool_id);
    pool.initialize(&deposit_token.address, &pair_token.address, &share_addr);

    // Transfer share token admin to pool so it can mint/burn
    let share_admin = StellarAssetClient::new(env, &share_addr);
    share_admin.set_admin(&pool_id);

    // Seed initial liquidity so the pool has reserves (1:1 ratio, 100k each)
    let liquidity = 100_000_0000000i128;
    deposit_admin.mint(admin, &liquidity);
    pair_admin.mint(admin, &liquidity);
    pool.deposit(
        admin,
        &vec![env, liquidity as u128, liquidity as u128],
        &0u128,
    );

    Fixture { pool, deposit_token, pair_token, aqua_token }
}

fn create_adapter<'a>(
    env: &'a Env,
    admin: &Address,
    vault: &Address,
    fixture: &Fixture,
    max_slippage_bps: u32,
) -> AquariusAdapterClient<'a> {
    let id = env.register(AquariusAdapter, ());
    let client = AquariusAdapterClient::new(env, &id);
    client.initialize(
        admin,
        vault,
        &fixture.pool.address,
        &fixture.deposit_token.address,
        &fixture.pair_token.address,
        &fixture.aqua_token.address,
        &max_slippage_bps,
    );
    client
}

// ============================================================================
// Tests
// ============================================================================

#[test]
fn test_initialize_getters() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let fixture = create_fixture(&env, &admin);

    let adapter = create_adapter(&env, &admin, &vault, &fixture, 50);

    assert_eq!(adapter.get_vault(),            vault);
    assert_eq!(adapter.get_pool(),             fixture.pool.address);
    assert_eq!(adapter.get_deposit_token(),    fixture.deposit_token.address);
    assert_eq!(adapter.get_pair_token(),       fixture.pair_token.address);
    assert_eq!(adapter.get_aqua_token(),       fixture.aqua_token.address);
    assert_eq!(adapter.get_max_slippage_bps(), 50u32);

    // share_token and indices are auto-resolved from pool
    let dep_idx  = adapter.get_deposit_token_idx();
    let pair_idx = adapter.get_pair_token_idx();
    assert!(dep_idx == 0 || dep_idx == 1);
    assert!(pair_idx == 0 || pair_idx == 1);
    assert_ne!(dep_idx, pair_idx);
}

#[test]
fn test_balance_starts_zero() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let fixture = create_fixture(&env, &admin);

    let adapter = create_adapter(&env, &admin, &vault, &fixture, 50);

    assert_eq!(adapter.a_balance(&adapter.address), 0i128);
}

#[test]
fn test_deposit_creates_position() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let fixture = create_fixture(&env, &admin);

    let adapter = create_adapter(&env, &admin, &vault, &fixture, 50);

    let amount = 1_000_0000000i128;
    StellarAssetClient::new(&env, &fixture.deposit_token.address)
        .mint(&adapter.address, &amount);

    let lp_value = adapter.a_deposit(&amount, &vault);

    assert!(lp_value > 0, "LP value should be positive after deposit");
    assert_eq!(adapter.a_balance(&adapter.address), lp_value);
}

#[test]
fn test_deposit_position_value_near_input() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let fixture = create_fixture(&env, &admin);

    // 1% max slippage
    let adapter = create_adapter(&env, &admin, &vault, &fixture, 100);

    let amount = 1_000_0000000i128;
    StellarAssetClient::new(&env, &fixture.deposit_token.address)
        .mint(&adapter.address, &amount);

    let lp_value = adapter.a_deposit(&amount, &vault);

    // LP value should be within 3% of input (swap fee + slippage)
    let tolerance = amount * 3 / 100;
    assert!(
        lp_value >= amount - tolerance,
        "LP value {} should be within 3% of input {}",
        lp_value,
        amount,
    );
}

#[test]
fn test_withdraw_returns_deposit_token_to_vault() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let fixture = create_fixture(&env, &admin);

    let adapter = create_adapter(&env, &admin, &vault, &fixture, 50);

    let amount = 1_000_0000000i128;
    StellarAssetClient::new(&env, &fixture.deposit_token.address)
        .mint(&adapter.address, &amount);
    let lp_value = adapter.a_deposit(&amount, &vault);
    assert!(lp_value > 0);

    let vault_before = fixture.deposit_token.balance(&vault);
    let actual = adapter.a_withdraw(&lp_value, &vault);

    assert!(actual > 0, "withdraw should return positive amount");
    assert_eq!(fixture.deposit_token.balance(&vault), vault_before + actual);
}

#[test]
fn test_partial_withdraw() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let fixture = create_fixture(&env, &admin);

    let adapter = create_adapter(&env, &admin, &vault, &fixture, 50);

    let amount = 10_000_0000000i128;
    StellarAssetClient::new(&env, &fixture.deposit_token.address)
        .mint(&adapter.address, &amount);
    let lp_value = adapter.a_deposit(&amount, &vault);
    assert!(lp_value > 0);

    let half_value   = lp_value / 2;
    let vault_before = fixture.deposit_token.balance(&vault);
    let actual       = adapter.a_withdraw(&half_value, &vault);

    assert!(actual > 0);
    assert_eq!(fixture.deposit_token.balance(&vault), vault_before + actual);

    // Remaining position
    let remaining = adapter.a_balance(&adapter.address);
    assert!(remaining > 0, "position should remain after partial withdraw");
}

#[test]
fn test_full_withdraw_clears_position() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let fixture = create_fixture(&env, &admin);

    let adapter = create_adapter(&env, &admin, &vault, &fixture, 50);

    let amount = 1_000_0000000i128;
    StellarAssetClient::new(&env, &fixture.deposit_token.address)
        .mint(&adapter.address, &amount);
    adapter.a_deposit(&amount, &vault);

    let pos = adapter.a_balance(&adapter.address);
    adapter.a_withdraw(&pos, &vault);

    assert_eq!(adapter.a_balance(&adapter.address), 0i128);
}

#[test]
fn test_apy_returns_zero() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let fixture = create_fixture(&env, &admin);

    let adapter = create_adapter(&env, &admin, &vault, &fixture, 50);

    assert_eq!(adapter.a_get_apy(), 0u32);
}

#[test]
fn test_harvest_returns_zero_when_no_rewards() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let fixture = create_fixture(&env, &admin);

    let adapter = create_adapter(&env, &admin, &vault, &fixture, 50);

    // Deposit first to have an active position
    let amount = 1_000_0000000i128;
    StellarAssetClient::new(&env, &fixture.deposit_token.address)
        .mint(&adapter.address, &amount);
    adapter.a_deposit(&amount, &vault);

    // Mock pool always returns 0 rewards — a_harvest should return (aqua_token, 0)
    let (reward_token, harvested) = adapter.a_harvest(&vault);
    assert_eq!(reward_token, fixture.aqua_token.address);
    assert_eq!(harvested, 0i128);
}

#[test]
fn test_update_slippage() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let fixture = create_fixture(&env, &admin);

    let adapter = create_adapter(&env, &admin, &vault, &fixture, 50);
    assert_eq!(adapter.get_max_slippage_bps(), 50u32);

    adapter.update_slippage(&admin, &200u32);
    assert_eq!(adapter.get_max_slippage_bps(), 200u32);
}

#[test]
#[should_panic]
fn test_update_slippage_above_max_panics() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let fixture = create_fixture(&env, &admin);

    let adapter = create_adapter(&env, &admin, &vault, &fixture, 50);

    // 1001 bps > MAX_SLIPPAGE_BPS (1000) → should panic
    adapter.update_slippage(&admin, &1001u32);
}

#[test]
#[should_panic]
fn test_initialize_slippage_above_max_panics() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let fixture = create_fixture(&env, &admin);

    // Slippage 1001 bps > 1000 max → should panic
    create_adapter(&env, &admin, &vault, &fixture, 1001);
}

#[test]
#[should_panic]
fn test_double_initialize_panics() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let fixture = create_fixture(&env, &admin);

    let adapter = create_adapter(&env, &admin, &vault, &fixture, 50);

    // Second initialize must panic with AlreadyInitialized
    adapter.initialize(
        &admin,
        &vault,
        &fixture.pool.address,
        &fixture.deposit_token.address,
        &fixture.pair_token.address,
        &fixture.aqua_token.address,
        &50u32,
    );
}

#[test]
fn test_sweep_recovers_stuck_tokens() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let receiver = Address::generate(&env);
    let fixture = create_fixture(&env, &admin);

    let adapter = create_adapter(&env, &admin, &vault, &fixture, 50);

    // Simulate tokens stuck in the adapter (excess pair_token from ratio drift)
    let stuck = 100_0000000i128;
    StellarAssetClient::new(&env, &fixture.pair_token.address)
        .mint(&adapter.address, &stuck);

    let before = fixture.pair_token.balance(&receiver);
    adapter.sweep(&admin, &fixture.pair_token.address, &receiver, &stuck);

    assert_eq!(fixture.pair_token.balance(&receiver), before + stuck);
    assert_eq!(fixture.pair_token.balance(&adapter.address), 0i128);
}

#[test]
fn test_multiple_deposits_accumulate() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let fixture = create_fixture(&env, &admin);

    let adapter = create_adapter(&env, &admin, &vault, &fixture, 50);

    let deposit_admin = StellarAssetClient::new(&env, &fixture.deposit_token.address);
    let amount = 1_000_0000000i128;

    // First deposit
    deposit_admin.mint(&adapter.address, &amount);
    let pos1 = adapter.a_deposit(&amount, &vault);
    assert!(pos1 > 0);

    // Second deposit → position should grow
    deposit_admin.mint(&adapter.address, &amount);
    let pos2 = adapter.a_deposit(&amount, &vault);

    assert!(pos2 > pos1, "position should grow after second deposit");
}

#[test]
fn test_withdraw_zero_lp_returns_zero() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let vault = Address::generate(&env);
    let fixture = create_fixture(&env, &admin);

    let adapter = create_adapter(&env, &admin, &vault, &fixture, 50);

    // No deposit → withdraw should return 0 without panicking
    let actual = adapter.a_withdraw(&1_000_0000000i128, &vault);
    assert_eq!(actual, 0i128);
}
