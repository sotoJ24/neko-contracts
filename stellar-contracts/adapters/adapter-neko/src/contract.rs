use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    contract, contractimpl,
    token::TokenClient,
    Address, Env, IntoVal, Symbol,
};

use crate::admin::Admin;
use crate::common::events::Events;
use crate::common::storage::Storage;
use crate::common::types::SCALAR_12;
use crate::neko_pool;

/// Adapter connecting neko-vault to a single neko-pool pool.
///
/// Cross-contract auth flow for a_deposit:
///   vault → token.transfer(vault, adapter, amount)   [vault self-auth]
///   vault → adapter.a_deposit(amount, vault)
///     adapter → authorize_as_current_contract([token.transfer(adapter, lending, amount)])
///     adapter → lending.deposit(adapter, asset, amount)
///       lending: lender.require_auth()        [adapter is invoker → PASS]
///       lending: token.transfer(adapter, lending, amount) [pre-authorized → PASS]
#[contract]
pub struct NekoAdapter;

#[contractimpl]
impl NekoAdapter {
    // ========== Initialization ==========

    pub fn initialize(
        env: Env,
        admin: Address,
        vault: Address,
        lending_pool: Address,
        deposit_token: Address,
        rwa_asset: Symbol,
    ) {
        Admin::initialize(&env, &admin, &vault, &lending_pool, &deposit_token, rwa_asset);
    }

    pub fn get_vault(env: Env) -> Address {
        Storage::load(&env).vault
    }

    pub fn get_lending_pool(env: Env) -> Address {
        Storage::load(&env).lending_pool
    }

    // ========== IAdapter interface ==========

    /// Deposit tokens into neko-pool.
    /// Pre-condition: vault has already transferred `amount` tokens to this adapter.
    /// Returns the adapter's current balance in the lending pool in deposit_token units.
    pub fn a_deposit(env: Env, amount: i128, _from: Address) -> i128 {
        let storage = Storage::load(&env);
        let adapter_addr = env.current_contract_address();

        // Pre-authorize the token.transfer(adapter, lending, amount)
        // that lending.deposit() will execute internally.
        env.authorize_as_current_contract(soroban_sdk::vec![
            &env,
            InvokerContractAuthEntry::Contract(SubContractInvocation {
                context: ContractContext {
                    contract: storage.deposit_token.clone(),
                    fn_name: soroban_sdk::symbol_short!("transfer"),
                    args: soroban_sdk::vec![
                        &env,
                        adapter_addr.clone().into_val(&env),
                        storage.lending_pool.clone().into_val(&env),
                        amount.into_val(&env),
                    ],
                },
                sub_invocations: soroban_sdk::vec![&env],
            }),
        ]);

        // Call lending.deposit — lending checks lender.require_auth() (adapter is invoker)
        // and calls token.transfer(adapter, lending, amount) (pre-authorized above)
        let lending = neko_pool::Client::new(&env, &storage.lending_pool);
        let b_tokens = lending.deposit(&adapter_addr, &storage.rwa_asset, &amount);

        Events::deposited(&env, &adapter_addr, &storage.rwa_asset, amount, b_tokens);

        // Return current balance in underlying token units
        Self::balance_in_underlying(&env, &adapter_addr, &storage)
    }

    /// Withdraw tokens from neko-pool and transfer them to `to` (the vault).
    /// Returns the actual amount withdrawn in deposit_token units.
    pub fn a_withdraw(env: Env, amount: i128, to: Address) -> i128 {
        let storage = Storage::load(&env);
        let adapter_addr = env.current_contract_address();

        let lending = neko_pool::Client::new(&env, &storage.lending_pool);

        // Convert underlying amount to b_tokens (round up to ensure we withdraw enough)
        let b_rate = lending.get_b_token_rate(&storage.rwa_asset);
        let b_tokens_to_burn = amount
            .checked_mul(SCALAR_12)
            .unwrap_or(i128::MAX)
            .checked_add(b_rate - 1)
            .unwrap_or(i128::MAX)
            .checked_div(b_rate)
            .unwrap_or(0);

        if b_tokens_to_burn == 0 {
            return 0;
        }

        // Cap at adapter's b_token balance
        let adapter_b_tokens = lending.get_b_token_balance(&adapter_addr, &storage.rwa_asset);
        let b_tokens_actual = b_tokens_to_burn.min(adapter_b_tokens);

        if b_tokens_actual == 0 {
            return 0;
        }

        // Pre-authorize the token.transfer(lending, adapter, underlying_amount)
        // that lending.withdraw() will execute internally.
        let underlying_out = b_tokens_actual
            .checked_mul(b_rate)
            .unwrap_or(0)
            .checked_div(SCALAR_12)
            .unwrap_or(0);

        env.authorize_as_current_contract(soroban_sdk::vec![
            &env,
            InvokerContractAuthEntry::Contract(SubContractInvocation {
                context: ContractContext {
                    contract: storage.deposit_token.clone(),
                    fn_name: soroban_sdk::symbol_short!("transfer"),
                    args: soroban_sdk::vec![
                        &env,
                        storage.lending_pool.clone().into_val(&env),
                        adapter_addr.clone().into_val(&env),
                        underlying_out.into_val(&env),
                    ],
                },
                sub_invocations: soroban_sdk::vec![&env],
            }),
        ]);

        // lending.withdraw returns the underlying amount
        let actual_withdrawn = lending.withdraw(&adapter_addr, &storage.rwa_asset, &b_tokens_actual);

        // Forward tokens to the vault (to)
        let token = TokenClient::new(&env, &storage.deposit_token);
        token.transfer(&adapter_addr, &to, &actual_withdrawn);

        Events::withdrawn(&env, &adapter_addr, &storage.rwa_asset, actual_withdrawn);

        actual_withdrawn
    }

    /// Returns the adapter's current value in neko-pool expressed in deposit_token units.
    /// value = b_tokens * b_rate / SCALAR_12
    pub fn a_balance(env: Env, from: Address) -> i128 {
        let storage = Storage::load(&env);
        Self::balance_in_underlying(&env, &from, &storage)
    }

    /// Returns the current supply APY in basis points.
    /// Approximated from b_rate change — for MVP returns 0 (no on-chain rate query in neko-pool).
    pub fn a_get_apy(_env: Env) -> u32 {
        // TODO: integrate with neko-pool interest rate model to compute APY
        // For MVP, returned as 0 (rate is embedded in b_rate change over time)
        0
    }

    /// No explicit harvest — yield is embedded in the b_rate appreciation.
    pub fn a_harvest(env: Env, _to: Address) -> (Address, i128) {
        let storage = Storage::load(&env);
        (storage.deposit_token, 0)
    }

    // ========== Helpers ==========

    fn balance_in_underlying(env: &Env, lender: &Address, storage: &crate::common::types::AdapterStorage) -> i128 {
        let lending = neko_pool::Client::new(env, &storage.lending_pool);
        let b_tokens = lending.get_b_token_balance(lender, &storage.rwa_asset);
        let b_rate = lending.get_b_token_rate(&storage.rwa_asset);

        b_tokens
            .checked_mul(b_rate)
            .unwrap_or(0)
            .checked_div(SCALAR_12)
            .unwrap_or(0)
    }
}
