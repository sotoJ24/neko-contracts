use soroban_sdk::{contract, contractimpl, Address, Env};

use crate::admin::Admin;
use crate::aquarius_pool;
use crate::common::error::Error;
use crate::common::events::Events;
use crate::common::storage::Storage;

/// Adapter connecting neko-vault to an Aquarius AMM pool.
///
/// Deposit flow (deposit_token single-sided entry):
///   vault → deposit_token.transfer(vault, adapter, amount)  [vault self-auth]
///   vault → adapter.a_deposit(amount, vault)
///     adapter → storage.vault.require_auth()                 [vault-only guard]
///     adapter → estimate_swap → authorize → pool.swap(min_pair_out)
///     adapter → estimate_deposit → authorize → pool.deposit(min_shares)
///
/// Withdraw flow:
///   vault → adapter.a_withdraw(amount, vault)
///     adapter → storage.vault.require_auth()
///     adapter → authorize([share_token.burn]) → pool.withdraw(min_amounts)
///     adapter → estimate_swap → authorize → pool.swap(min_deposit)
///     adapter → deposit_token.transfer(adapter, vault, total)
///
/// Harvest flow:
///   vault → adapter.a_harvest(vault)
///     adapter → storage.vault.require_auth()
///     adapter → pool.claim(adapter)   [permissionless on-chain; guarded here]
///     adapter → aqua_token.transfer(adapter, vault, aqua_amount)
#[contract]
pub struct AquariusAdapter;

#[contractimpl]
impl AquariusAdapter {
    // ========== Initialization ==========

    /// Initialize the adapter.
    ///
    /// Queries the Aquarius pool to resolve deposit_token_idx, pair_token_idx,
    /// and share_token automatically — no manual index configuration needed.
    ///
    /// `max_slippage_bps`: slippage tolerance for all swaps and LP operations
    /// (e.g. 50 = 0.5%). Must be ≤ 1000 (10%).
    pub fn initialize(
        env: Env,
        admin: Address,
        vault: Address,
        pool: Address,
        deposit_token: Address,
        pair_token: Address,
        aqua_token: Address,
        max_slippage_bps: u32,
    ) {
        Admin::initialize(
            &env,
            &admin,
            &vault,
            &pool,
            &deposit_token,
            &pair_token,
            &aqua_token,
            max_slippage_bps,
        );
    }

    // ========== Admin ==========

    /// Update the slippage tolerance. Admin-only.
    pub fn update_slippage(env: Env, admin: Address, new_slippage_bps: u32) {
        admin.require_auth();
        Admin::update_slippage(&env, &admin, new_slippage_bps);
    }

    /// Transfer any tokens stuck in this adapter to `to`.
    ///
    /// Tokens can accumulate when deposit produces excess pair_token (due to reserve
    /// ratio drift between swap and add_liquidity). Admin can recover them at any time.
    pub fn sweep(env: Env, admin: Address, token: Address, to: Address, amount: i128) {
        admin.require_auth();
        let storage = Storage::load(&env);
        if admin != storage.admin {
            soroban_sdk::panic_with_error!(&env, Error::NotAdmin);
        }
        soroban_sdk::token::TokenClient::new(&env, &token).transfer(
            &env.current_contract_address(),
            &to,
            &amount,
        );
    }

    // ========== Getters ==========

    pub fn get_vault(env: Env) -> Address {
        Storage::load(&env).vault
    }

    pub fn get_pool(env: Env) -> Address {
        Storage::load(&env).pool
    }

    pub fn get_deposit_token(env: Env) -> Address {
        Storage::load(&env).deposit_token
    }

    pub fn get_pair_token(env: Env) -> Address {
        Storage::load(&env).pair_token
    }

    pub fn get_share_token(env: Env) -> Address {
        Storage::load(&env).share_token
    }

    pub fn get_aqua_token(env: Env) -> Address {
        Storage::load(&env).aqua_token
    }

    pub fn get_deposit_token_idx(env: Env) -> u32 {
        Storage::load(&env).deposit_token_idx
    }

    pub fn get_pair_token_idx(env: Env) -> u32 {
        Storage::load(&env).pair_token_idx
    }

    pub fn get_max_slippage_bps(env: Env) -> u32 {
        Storage::load(&env).max_slippage_bps
    }

    // ========== IAdapter interface ==========

    /// Deposit deposit_token into the Aquarius pool.
    ///
    /// Pre-condition: vault has already transferred `amount` deposit_token to this adapter.
    /// Returns the adapter's LP position value in deposit_token units after deposit.
    pub fn a_deposit(env: Env, amount: i128, _from: Address) -> i128 {
        let storage = Storage::load(&env);
        // Only the configured vault can trigger deposits.
        storage.vault.require_auth();

        let adapter_addr = env.current_contract_address();
        let lp_value = aquarius_pool::deposit(&env, amount, &storage);

        Events::deposited(&env, &adapter_addr, &storage.deposit_token, amount, lp_value);

        lp_value
    }

    /// Withdraw deposit_token from the Aquarius pool and transfer to `to` (the vault).
    ///
    /// Burns LP tokens proportional to `amount / total_balance`.
    /// Returns the actual deposit_token amount sent to the vault.
    pub fn a_withdraw(env: Env, amount: i128, to: Address) -> i128 {
        let storage = Storage::load(&env);
        // Only the configured vault can trigger withdrawals.
        storage.vault.require_auth();

        let adapter_addr = env.current_contract_address();
        let actual = aquarius_pool::withdraw(&env, amount, &to, &storage);

        Events::withdrawn(&env, &adapter_addr, &storage.deposit_token, actual);

        actual
    }

    /// Returns the adapter's LP position value in deposit_token units.
    ///
    /// value = share_of_reserve_deposit + (share_of_reserve_pair × spot_price)
    pub fn a_balance(env: Env, from: Address) -> i128 {
        let storage = Storage::load(&env);
        aquarius_pool::position_value(&env, &from, &storage)
    }

    /// Returns 0 — AMM trading fees accrue into LP reserve growth, not a separate APY.
    pub fn a_get_apy(_env: Env) -> u32 {
        0
    }

    /// Claim AQUA rewards from the pool and forward to `to` (the vault).
    ///
    /// pool.claim() is permissionless on-chain; guarded here to vault-only to keep
    /// reward accounting consistent with the vault's harvest_all() flow.
    /// Returns the reward token address and amount harvested.
    pub fn a_harvest(env: Env, to: Address) -> (Address, i128) {
        let storage = Storage::load(&env);
        // Only the configured vault can trigger harvests.
        storage.vault.require_auth();

        let adapter_addr = env.current_contract_address();
        let aqua_harvested = aquarius_pool::claim(&env, &to, &storage);

        if aqua_harvested > 0 {
            Events::harvested(&env, &adapter_addr, &storage.aqua_token, aqua_harvested);
        }

        (storage.aqua_token.clone(), aqua_harvested)
    }
}
