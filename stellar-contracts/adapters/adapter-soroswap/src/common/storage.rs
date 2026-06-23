use soroban_sdk::{contracttype, Env};

use crate::common::types::{AdapterStorage, SlippageConfig};

#[contracttype]
enum DataKey {
    Storage,
    SlippageCfg,
}

pub struct Storage;

impl Storage {
    pub fn is_initialized(env: &Env) -> bool {
        env.storage().instance().has(&DataKey::Storage)
    }

    pub fn save(env: &Env, data: &AdapterStorage) {
        env.storage().instance().set(&DataKey::Storage, data);
        env.storage().instance().extend_ttl(
            518_400, // 30 days
            518_400,
        );
    }

    pub fn load(env: &Env) -> AdapterStorage {
        env.storage()
            .instance()
            .get(&DataKey::Storage)
            .unwrap_or_else(|| panic!())
    }

    /// Persist slippage config. Falls back to `SlippageConfig::default()` when
    /// never set, so the adapter is always safe even before an admin call.
    pub fn save_slippage(env: &Env, config: &SlippageConfig) {
        env.storage().instance().set(&DataKey::SlippageCfg, config);
        env.storage().instance().extend_ttl(518_400, 518_400);
    }

    pub fn load_slippage(env: &Env) -> SlippageConfig {
        env.storage()
            .instance()
            .get(&DataKey::SlippageCfg)
            .unwrap_or_else(|| SlippageConfig::default())
    }
}