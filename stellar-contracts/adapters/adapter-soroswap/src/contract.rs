use soroban_sdk::{contract, contractimpl, Address, Env};

use crate::admin::Admin;
use crate::common::events::Events;
use crate::common::storage::Storage;
use crate::soroswap_pool;

/// Adapter connecting neko-vault to a Soroswap AMM pair.
///
/// Deposit flow (token_a single-sided entry):
///   vault → token_a.transfer(vault, adapter, amount)  [vault self-auth]
///   vault → adapter.a_deposit(amount, vault)
///     adapter → authorize_as_current_contract([token_a.transfer(adapter, pair, swap)])
///     adapter → router.swap_exact_tokens_for_tokens(swap, 0, [A,B], adapter, deadline)
///     adapter → authorize_as_current_contract([token_a.transfer, token_b.transfer])
///     adapter → router.add_liquidity(A, B, remaining, b_received, 0, 0, adapter, deadline)
///     adapter → returns LP balance as position value in token_a units
///
/// Withdraw flow:
///   vault → adapter.a_withdraw(amount, vault)
///     adapter → authorize_as_current_contract([pair.transfer(adapter, pair, lp_to_burn)])
///     adapter → router.remove_liquidity(A, B, lp_to_burn, 0, 0, adapter, deadline)
///     adapter → authorize_as_current_contract([token_b.transfer(adapter, pair, b_out)])
///     adapter → router.swap_exact_tokens_for_tokens(b_out, 0, [B,A], adapter, deadline)
///     adapter → token_a.transfer(adapter, vault, total_a)
#[contract]
pub struct SoroswapAdapter;

#[contractimpl]
impl SoroswapAdapter {
    // ========== Initialization ==========

    /// Initialize the adapter.
    ///
    /// Queries the Soroswap router to resolve the pair address for (token_a, token_b).
    pub fn initialize(
        env: Env,
        admin: Address,
        vault: Address,
        router: Address,
        token_a: Address, // deposit token (single-asset entry, e.g. USDC)
        token_b: Address, // pair token (e.g. XLM)
    ) {
        Admin::initialize(&env, &admin, &vault, &router, &token_a, &token_b);
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

    // ========== IAdapter interface ==========

    /// Deposit token_a into the Soroswap pair.
    ///
    /// Pre-condition: vault has already transferred `amount` token_a to this adapter.
    /// Returns the adapter's LP position value in token_a units after deposit.
    pub fn a_deposit(env: Env, amount: i128, _from: Address) -> i128 {
        let storage      = Storage::load(&env);
        let adapter_addr = env.current_contract_address();

        let lp_balance = soroswap_pool::deposit(&env, amount, &storage);

        Events::deposited(&env, &adapter_addr, &storage.token_a, amount, lp_balance);

        soroswap_pool::position_value(&env, &adapter_addr, &storage)
    }

    /// Withdraw token_a from the Soroswap pair and transfer to `to` (the vault).
    ///
    /// Burns LP tokens proportional to `amount / total_balance`.
    /// Returns the actual token_a amount sent to the vault.
    pub fn a_withdraw(env: Env, amount: i128, to: Address) -> i128 {
        let storage      = Storage::load(&env);
        let adapter_addr = env.current_contract_address();

        let actual = soroswap_pool::withdraw(&env, amount, &to, &storage);

        Events::withdrawn(&env, &adapter_addr, &storage.token_a, actual);

        actual
    }

    /// Returns the adapter's LP position value in token_a units.
    ///
    /// value = share_of_reserve_a + (share_of_reserve_b × spot_price_b_in_a)
    pub fn a_balance(env: Env, from: Address) -> i128 {
        let storage = Storage::load(&env);
        soroswap_pool::position_value(&env, &from, &storage)
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
