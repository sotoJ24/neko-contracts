use soroban_sdk::{Address, Env, contractclient};

/// Polymorphic adapter interface.
/// Any contract at `adapter_address` that exposes these functions
/// can be registered as a protocol in the vault.
#[allow(dead_code)]
#[contractclient(name = "AdapterClient")]
pub trait IAdapter {
    /// Deposit `amount` tokens into the protocol.
    /// The vault has already transferred tokens to the adapter before this call.
    /// Returns the current balance of `from` (vault) in this protocol
    /// expressed in deposit_token units.
    fn a_deposit(env: Env, amount: i128, from: Address) -> i128;

    /// Withdraw `amount` tokens from the protocol and send them to `to` (vault).
    /// Returns the actual amount withdrawn (may be less if insufficient).
    fn a_withdraw(env: Env, amount: i128, to: Address) -> i128;

    /// Returns the current value of `from` (vault) in this protocol
    /// expressed in deposit_token units (underlying, not position tokens).
    fn a_balance(env: Env, from: Address) -> i128;

    /// Returns the current APY in basis points (10_000 = 100%).
    fn a_get_apy(env: Env) -> u32;

    /// Harvest accumulated rewards and send them to `to` (vault).
    /// Returns the reward token address and the amount harvested.
    fn a_harvest(env: Env, to: Address) -> (Address, i128);
}
