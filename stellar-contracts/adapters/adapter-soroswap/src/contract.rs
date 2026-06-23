use soroban_sdk::{contract, contractimpl, panic_with_error, Address, Env};

use crate::admin::Admin;
use crate::common::error::Error;
use crate::common::events::Events;
use crate::common::storage::Storage;
use crate::common::types::SlippageConfig;
use crate::soroswap_pool;

/// Adapter connecting neko-vault to a Soroswap AMM pair.
///
/// Deposit flow (token_a single-sided entry):
///   vault → token_a.transfer(vault, adapter, amount)  [vault self-auth]
///   vault → adapter.a_deposit(amount, vault)
///     adapter → price impact check (rejects if > max_price_impact_bps)
///     adapter → authorize_as_current_contract([token_a.transfer(adapter, pair, swap)])
///     adapter → router.swap_exact_tokens_for_tokens(swap, min_b_out, [A,B], adapter, deadline)
///     adapter → authorize_as_current_contract([token_a.transfer, token_b.transfer])
///     adapter → router.add_liquidity(A, B, remaining, b_received, min_a, min_b, adapter, deadline)
///     adapter → returns fair-value LP position in token_a units
///
/// Withdraw flow:
///   vault → adapter.a_withdraw(amount, vault)
///     adapter → authorize_as_current_contract([pair.transfer(adapter, pair, lp_to_burn)])
///     adapter → router.remove_liquidity(A, B, lp_to_burn, min_a, min_b, adapter, deadline)
///     adapter → authorize_as_current_contract([token_b.transfer(adapter, pair, b_out)])
///     adapter → router.swap_exact_tokens_for_tokens(b_out, min_swap_a, [B,A], adapter, deadline)
///     adapter → token_a.transfer(adapter, vault, total_a)
#[contract]
pub struct SoroswapAdapter;

#[contractimpl]
impl SoroswapAdapter {
    // ========== Initialization ==========

    /// Initialize the adapter.
    ///
    /// Queries the Soroswap router to resolve the pair address for (token_a, token_b).
    /// Sets default SlippageConfig (50 bps slippage, 100 bps price impact).
    pub fn initialize(
        env: Env,
        admin: Address,
        vault: Address,
        router: Address,
        token_a: Address,
        token_b: Address,
    ) {
        Admin::initialize(&env, &admin, &vault, &router, &token_a, &token_b);
        // Write conservative defaults so every swap path is safe from day one.
        Storage::save_slippage(&env, &SlippageConfig::default());
    }

    pub fn get_vault(env: Env) -> Address {
        Storage::load(&env).vault
    }

    pub fn get_router(env: Env) -> Address {
        Storage::load(&env).router
    }

    pub fn get_pair(env: Env) -> Address {
        Storage::load(&env).pair
    }

    pub fn get_token_a(env: Env) -> Address {
        Storage::load(&env).token_a
    }

    pub fn get_token_b(env: Env) -> Address {
        Storage::load(&env).token_b
    }

    // ========== Slippage config (admin only) ==========

    /// Update slippage / price-impact limits.
    ///
    /// Only callable by the stored admin address.
    /// `max_slippage_bps` and `max_price_impact_bps` must each be ≤ 10_000.
    pub fn set_slippage_config(env: Env, caller: Address, config: SlippageConfig) {
        let storage = Storage::load(&env);
        if caller != storage.admin {
            panic_with_error!(&env, Error::NotAdmin);
        }
        caller.require_auth();
        if config.max_slippage_bps > 10_000 || config.max_price_impact_bps > 10_000 {
            panic_with_error!(&env, Error::ArithmeticError);
        }
        Storage::save_slippage(&env, &config);
    }

    /// Return the current slippage configuration.
    pub fn get_slippage_config(env: Env) -> SlippageConfig {
        Storage::load_slippage(&env)
    }

    // ========== Price-impact helper ==========

    /// Estimate the price impact in basis points for swapping `amount_in` of
    /// token_a into the pair.
    ///
    /// Rejects the call (panics) if impact > `max_price_impact_bps`.
    pub fn get_price_impact_bps(env: Env, amount_in: i128) -> u32 {
        let storage = Storage::load(&env);
        soroswap_pool::get_price_impact_bps(&env, amount_in, &storage)
    }

    // ========== Fair-value view ==========

    /// Returns the adapter's LP position in fair-value (manipulation-resistant)
    /// token_a units.
    ///
    /// Uses the constant-product fair-value formula:
    ///   fair_value = 2 × sqrt(share_a × share_b_in_a)
    ///
    /// Nav::calculate should call this (via a_balance) instead of any spot-price path.
    pub fn get_fair_value(env: Env, from: Address) -> i128 {
        let storage = Storage::load(&env);
        soroswap_pool::get_fair_value(&env, &from, &storage)
    }

    // ========== IAdapter interface ==========

    /// Deposit token_a into the Soroswap pair.
    ///
    /// Pre-condition: vault has already transferred `amount` token_a to this adapter.
    /// Returns the adapter's fair-value LP position in token_a units after deposit.
    pub fn a_deposit(env: Env, amount: i128, _from: Address) -> i128 {
        let storage      = Storage::load(&env);
        let adapter_addr = env.current_contract_address();

        let lp_balance = soroswap_pool::deposit(&env, amount, &storage);

        Events::deposited(&env, &adapter_addr, &storage.token_a, amount, lp_balance);

        // Return fair-value (manipulation-resistant) position value
        soroswap_pool::get_fair_value(&env, &adapter_addr, &storage)
    }

    /// Withdraw token_a from the Soroswap pair and transfer to `to` (the vault).
    ///
    /// Burns LP tokens proportional to `amount / total_fair_value`.
    /// Returns the actual token_a amount sent to the vault.
    pub fn a_withdraw(env: Env, amount: i128, to: Address) -> i128 {
        let storage      = Storage::load(&env);
        let adapter_addr = env.current_contract_address();

        let actual = soroswap_pool::withdraw(&env, amount, &to, &storage);

        Events::withdrawn(&env, &adapter_addr, &storage.token_a, actual);

        actual
    }

    /// Returns the adapter's fair-value LP position in token_a units.
    ///
    /// Called by Nav::calculate — returns manipulation-resistant value.
    pub fn a_balance(env: Env, from: Address) -> i128 {
        let storage = Storage::load(&env);
        soroswap_pool::get_fair_value(&env, &from, &storage)
    }

    /// Returns 0 — AMM yield (trading fees) is reflected in LP token value growth.
    pub fn a_get_apy(_env: Env) -> u32 {
        0
    }

    /// No explicit harvest — Soroswap fees accrue into the pair reserves and are
    /// realized automatically when liquidity is removed.
    pub fn a_harvest(env: Env, _to: Address) -> (Address, i128) {
        let storage = Storage::load(&env);
        (storage.token_a, 0)
    }
}