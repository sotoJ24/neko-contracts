use soroban_sdk::{Address, BytesN, Env, String, Symbol, Vec, contract, contractimpl};

use crate::admin::Admin;
use crate::common::error::Error;
use crate::common::events::Events;
use crate::common::storage::{AllowanceStorage, BalanceStorage, Storage};
use crate::common::types::{ProtocolAllocation, RiskTier, VaultConfig, VaultStatus, HarvestConfig};
use crate::strategies::harvester::Harvester;
use crate::strategies::optimizer::Optimizer;
use crate::strategies::rebalancer::Rebalancer;
use crate::vault::deposit::Deposit;
use crate::vault::shares::Shares;
use crate::vault::withdraw::Withdraw;

#[contract]
pub struct VaultContract;

#[contractimpl]
impl VaultContract {
    // ========== Initialization ==========

    pub fn initialize(
        env: Env,
        admin: Address,
        manager: Address,
        deposit_token: Address,
        token_name: String,
        token_symbol: String,
        token_decimals: u32,
        config: VaultConfig,
    ) {
        Admin::initialize(
            &env,
            &admin,
            &manager,
            &deposit_token,
            token_name,
            token_symbol,
            token_decimals,
            config,
        );
    }

    // ========== Admin ==========

    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) {
        Admin::upgrade(&env, new_wasm_hash);
    }

    pub fn pause(env: Env) {
        Admin::pause(&env);
    }

    pub fn unpause(env: Env) {
        Admin::unpause(&env);
    }

    pub fn emergency_exit(env: Env) {
        Admin::emergency_exit(&env);
    }

    pub fn set_config(env: Env, config: VaultConfig) {
        Admin::set_config(&env, config);
    }

    pub fn set_manager(env: Env, manager: Address) {
        Admin::set_manager(&env, &manager);
    }

    pub fn set_harvest_config(env: Env, config: HarvestConfig) {
        Admin::require_admin(&env);
        Storage::set_harvest_config(&env, &config);
    }

    pub fn get_harvest_config(env: Env) -> Option<HarvestConfig> {
        Admin::require_admin(&env);
        Storage::get_harvest_config(&env)
    }

    // ========== Protocol management ==========

    pub fn add_protocol(
        env: Env,
        id: Symbol,
        adapter: Address,
        target_bps: u32,
        risk_tier: RiskTier,
    ) {
        Admin::add_protocol(&env, &id, &adapter, target_bps, risk_tier);
    }

    pub fn remove_protocol(env: Env, id: Symbol) {
        Admin::remove_protocol(&env, &id);
    }

    pub fn set_protocol_active(env: Env, id: Symbol, active: bool) {
        Admin::set_protocol_active(&env, &id, active);
    }

    // ========== User operations ==========

    /// Deposit `amount` of deposit_token, receive vTokens proportional to current NAV.
    pub fn deposit(env: Env, from: Address, amount: i128) -> Result<i128, Error> {
        Deposit::execute(&env, &from, amount)
    }

    /// Burn `shares` of vTokens and receive deposit_token in return.
    pub fn withdraw(env: Env, from: Address, shares: i128) -> Result<i128, Error> {
        Withdraw::execute(&env, &from, shares)
    }

    // ========== Manager operations ==========

    /// Rebalance allocations between protocols. Manager-only.
    pub fn rebalance(env: Env) -> Result<(), Error> {
        Admin::require_manager(&env);
        Rebalancer::execute(&env)
    }

    /// Harvest rewards from all protocols. Manager-only.
    pub fn harvest_all(env: Env) -> Result<i128, Error> {
        Admin::require_manager(&env);
        Harvester::harvest_all(&env)
    }

    /// Apply performance fee if share price exceeds the high water mark. Manager-only.
    pub fn apply_performance_fee(env: Env) -> Result<(), Error> {
        Admin::require_manager(&env);
        Harvester::apply_performance_fee(&env)
    }

    // ========== View functions ==========

    pub fn get_nav(env: Env) -> i128 {
        Admin::get_nav(&env)
    }

    pub fn get_share_price(env: Env) -> i128 {
        Admin::get_share_price(&env)
    }

    pub fn get_total_shares(env: Env) -> i128 {
        let storage = Storage::load(&env);
        storage.total_shares
    }

    pub fn get_user_shares(env: Env, user: Address) -> i128 {
        BalanceStorage::get(&env, &user)
    }

    pub fn get_liquid_reserve(env: Env) -> i128 {
        let storage = Storage::load(&env);
        storage.liquid_reserve
    }

    pub fn get_status(env: Env) -> VaultStatus {
        let storage = Storage::load(&env);
        storage.status
    }

    pub fn get_config(env: Env) -> VaultConfig {
        let storage = Storage::load(&env);
        storage.config
    }

    pub fn get_protocols(env: Env) -> Vec<(Symbol, ProtocolAllocation)> {
        let storage = Storage::load(&env);
        let mut result: Vec<(Symbol, ProtocolAllocation)> = Vec::new(&env);
        for id in storage.protocol_ids.iter() {
            if let Some(alloc) = storage.protocol_allocations.get(id.clone()) {
                result.push_back((id, alloc));
            }
        }
        result
    }

    pub fn get_weighted_apy(env: Env) -> u32 {
        let storage = Storage::load(&env);
        Optimizer::weighted_apy(&env, &storage)
    }

    pub fn get_ranked_protocols(env: Env) -> Vec<(Symbol, u32)> {
        let storage = Storage::load(&env);
        Optimizer::rank_by_apy(&env, &storage)
    }

    pub fn get_admin(env: Env) -> Address {
        let storage = Storage::load(&env);
        storage.admin
    }

    pub fn get_manager(env: Env) -> Address {
        let storage = Storage::load(&env);
        storage.manager
    }

    // ========== SEP-41 Token Interface ==========

    pub fn allowance(env: Env, from: Address, spender: Address) -> i128 {
        AllowanceStorage::get(&env, &from, &spender).amount
    }

    pub fn approve(
        env: Env,
        from: Address,
        spender: Address,
        amount: i128,
        live_until_ledger: u32,
    ) {
        from.require_auth();
        let current = env.ledger().sequence();
        if live_until_ledger != 0 && live_until_ledger < current {
            panic!();
        }
        AllowanceStorage::set(&env, &from, &spender, amount, live_until_ledger);
        Events::approve(&env, &from, &spender, amount, live_until_ledger);
    }

    pub fn balance(env: Env, id: Address) -> i128 {
        BalanceStorage::get(&env, &id)
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();
        Shares::transfer(&env, &from, &to, amount);
        Events::transfer(&env, &from, &to, amount);
    }

    pub fn transfer_from(env: Env, spender: Address, from: Address, to: Address, amount: i128) {
        spender.require_auth();
        Shares::transfer_from(&env, &spender, &from, &to, amount);
        Events::transfer(&env, &from, &to, amount);
    }

    pub fn burn(env: Env, from: Address, amount: i128) {
        from.require_auth();
        Shares::burn(&env, &from, amount);
        let mut storage = Storage::load(&env);
        storage.total_shares -= amount;
        Storage::save(&env, &storage);
        Events::burn(&env, &from, amount);
    }

    pub fn burn_from(env: Env, spender: Address, from: Address, amount: i128) {
        spender.require_auth();
        AllowanceStorage::subtract(&env, &from, &spender, amount);
        Shares::burn(&env, &from, amount);
        let mut storage = Storage::load(&env);
        storage.total_shares -= amount;
        Storage::save(&env, &storage);
        Events::burn(&env, &from, amount);
    }

    pub fn decimals(env: Env) -> u32 {
        let storage = Storage::load(&env);
        storage.token_decimals
    }

    pub fn name(env: Env) -> String {
        let storage = Storage::load(&env);
        storage.token_name
    }

    pub fn symbol(env: Env) -> String {
        let storage = Storage::load(&env);
        storage.token_symbol
    }
}
