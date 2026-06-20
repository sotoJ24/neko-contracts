use soroban_sdk::{Address, Env, panic_with_error, symbol_short};

use crate::common::error::Error;
use crate::common::types::{
    DataKey, INSTANCE_BUMP, INSTANCE_TTL, STORAGE_KEY, Txn, VaultAllowance, VaultStorage, HarvestConfig,
};

// ============================================================================
// Vault Storage (instance)
// ============================================================================

pub struct Storage;

impl Storage {
    pub fn load(env: &Env) -> VaultStorage {
        env.storage()
            .instance()
            .get(&STORAGE_KEY)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized))
    }

    pub fn save(env: &Env, storage: &VaultStorage) {
        env.storage().instance().set(&STORAGE_KEY, storage);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL, INSTANCE_BUMP);
    }

    pub fn is_initialized(env: &Env) -> bool {
        env.storage().instance().has(&STORAGE_KEY)
    }

    pub fn get_harvest_config(env: &Env) -> Option<HarvestConfig> {
        env.storage()
            .instance()
            .get(&symbol_short!("H_CONF"))
    }

    pub fn set_harvest_config(env: &Env, config: &HarvestConfig) {
        env.storage()
            .instance()
            .set(&symbol_short!("H_CONF"), config);
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL, INSTANCE_BUMP);
    }
}

// ============================================================================
// Share Balances (persistent) — SEP-41 vToken balances
// ============================================================================

pub struct BalanceStorage;

impl BalanceStorage {
    pub fn get(env: &Env, id: &Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::Balance(id.clone()))
            .unwrap_or(0)
    }

    pub fn set(env: &Env, id: &Address, amount: i128) {
        let key = DataKey::Balance(id.clone());
        env.storage().persistent().set(&key, &amount);
        let ttl = env.storage().max_ttl();
        env.storage().persistent().extend_ttl(&key, ttl, ttl);
    }

    pub fn add(env: &Env, id: &Address, amount: i128) {
        let balance = Self::get(env, id);
        let new_balance = balance
            .checked_add(amount)
            .unwrap_or_else(|| panic_with_error!(env, Error::ArithmeticError));
        Self::set(env, id, new_balance);
    }

    pub fn subtract(env: &Env, id: &Address, amount: i128) {
        let balance = Self::get(env, id);
        if balance < amount {
            panic_with_error!(env, Error::InsufficientBalance);
        }
        Self::set(env, id, balance - amount);
    }
}

// ============================================================================
// Allowance Storage (persistent) — SEP-41 vToken allowances
// ============================================================================

pub struct AllowanceStorage;

impl AllowanceStorage {
    pub fn get(env: &Env, from: &Address, spender: &Address) -> VaultAllowance {
        let key = DataKey::Allowance(Txn(from.clone(), spender.clone()));
        env.storage()
            .persistent()
            .get(&key)
            .unwrap_or(VaultAllowance {
                amount: 0,
                live_until_ledger: 0,
            })
    }

    pub fn set(env: &Env, from: &Address, spender: &Address, amount: i128, live_until_ledger: u32) {
        let key = DataKey::Allowance(Txn(from.clone(), spender.clone()));
        let allowance = VaultAllowance {
            amount,
            live_until_ledger,
        };
        env.storage().persistent().set(&key, &allowance);
        let ttl = env.storage().max_ttl();
        env.storage().persistent().extend_ttl(&key, ttl, ttl);
    }

    pub fn subtract(env: &Env, from: &Address, spender: &Address, amount: i128) {
        let mut allowance = Self::get(env, from, spender);
        if allowance.amount < amount {
            panic_with_error!(env, Error::InsufficientAllowance);
        }
        // Check not expired
        let current = env.ledger().sequence();
        if allowance.live_until_ledger != 0 && allowance.live_until_ledger < current {
            panic_with_error!(env, Error::InsufficientAllowance);
        }
        allowance.amount -= amount;
        Self::set(
            env,
            from,
            spender,
            allowance.amount,
            allowance.live_until_ledger,
        );
    }
}
