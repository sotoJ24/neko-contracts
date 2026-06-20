use soroban_sdk::{contract, contractimpl, Address, Env};

use crate::admin::Admin;
use crate::blend_pool;
use crate::common::events::Events;
use crate::common::storage::Storage;

/// Adapter connecting neko-vault to a single Blend lending pool.
///
/// Cross-contract auth flow for a_deposit:
///   vault → token.transfer(vault, adapter, amount)     [vault self-auth]
///   vault → adapter.a_deposit(amount, vault)
///     adapter → authorize_as_current_contract([token.transfer(adapter, pool, amount)])
///     adapter → pool.submit(adapter, adapter, adapter, [Supply(token, amount)])
///       blend: lender.require_auth()                   [adapter is invoker → PASS]
///       blend: token.transfer(adapter, pool, amount)   [pre-authorized → PASS]
///       blend: mints b_tokens to adapter's position
///
/// Cross-contract auth flow for a_withdraw:
///   vault → adapter.a_withdraw(amount, vault_addr)
///     adapter → pool.submit(adapter, adapter, vault, [Withdraw(token, amount)])
///       blend: withdraws tokens and sends directly to vault
#[contract]
pub struct BlendAdapter;

#[contractimpl]
impl BlendAdapter {
    // ========== Initialization ==========

    /// Initialize the adapter.
    ///
    /// Queries the Blend pool to resolve the reserve_id and claim_ids for deposit_token
    /// automatically — no manual ID configuration needed.
    pub fn initialize(
        env: Env,
        admin: Address,
        vault: Address,
        blend_pool: Address,
        deposit_token: Address,
        blend_token: Address,
    ) {
        Admin::initialize(&env, &admin, &vault, &blend_pool, &deposit_token, &blend_token);
    }

    pub fn get_vault(env: Env) -> Address {
        Storage::load(&env).vault
    }

    pub fn get_blend_pool(env: Env) -> Address {
        Storage::load(&env).blend_pool
    }

    pub fn get_reserve_id(env: Env) -> u32 {
        Storage::load(&env).reserve_id
    }

    // ========== IAdapter interface ==========

    /// Deposit tokens into the Blend pool.
    ///
    /// Pre-condition: vault has already transferred `amount` tokens to this adapter.
    /// Returns the adapter's current position value in deposit_token units after deposit.
    pub fn a_deposit(env: Env, amount: i128, _from: Address) -> i128 {
        let storage = Storage::load(&env);
        let adapter_addr = env.current_contract_address();

        let balance_after = blend_pool::supply(&env, amount, &storage);

        Events::deposited(&env, &adapter_addr, &storage.deposit_token, amount);

        balance_after
    }

    /// Withdraw tokens from the Blend pool and transfer to `to` (the vault).
    ///
    /// Returns the actual amount withdrawn in deposit_token units.
    pub fn a_withdraw(env: Env, amount: i128, to: Address) -> i128 {
        let storage = Storage::load(&env);
        let adapter_addr = env.current_contract_address();

        let actual = blend_pool::withdraw(&env, amount, &to, &storage);

        Events::withdrawn(&env, &adapter_addr, &storage.deposit_token, actual);

        actual
    }

    /// Returns the adapter's current position value in deposit_token units.
    /// value = b_tokens * b_rate / SCALAR_12
    pub fn a_balance(env: Env, from: Address) -> i128 {
        let storage = Storage::load(&env);
        blend_pool::position_value(&env, &from, &storage)
    }

    /// Returns the current supply APY in basis points.
    ///
    /// Blend's yield is embedded in b_rate appreciation — no explicit per-second APY
    /// is exposed on-chain. Returns 0 for MVP; integrate with Blend's interest rate
    /// model in a future iteration.
    pub fn a_get_apy(_env: Env) -> u32 {
        0
    }

    /// Claim BLND emissions from the pool directly to `to` (the vault).
    ///
    /// Blend emits BLND tokens as liquidity mining rewards. This function:
    ///   1. Claims BLND directly to the vault via pool.claim()
    ///   2. Returns the reward token address and amount harvested
    ///
    /// The vault's neko-vault#harvest_all() accumulates this or swaps it.
    pub fn a_harvest(env: Env, to: Address) -> (Address, i128) {
        let storage = Storage::load(&env);
        let adapter_addr = env.current_contract_address();

        let (reward_token, blnd_harvested) = blend_pool::claim(&env, &to, &storage);

        if blnd_harvested > 0 {
            Events::harvested(&env, &adapter_addr, &storage.blend_token, blnd_harvested);
        }

        (reward_token, blnd_harvested)
    }
}
