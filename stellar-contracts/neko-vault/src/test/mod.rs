extern crate std;

use soroban_sdk::{
    Address, Env, String, contract, contractimpl, symbol_short,
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
};

use crate::common::types::{RiskTier, VaultConfig, VaultStatus, HarvestConfig};
use crate::{VaultContract, VaultContractClient};

// ============================================================================
// Mock Adapter Contract (in-memory, no real lending)
// ============================================================================

#[contract]
struct MockAdapter;

#[contractimpl]
impl MockAdapter {
    pub fn initialize(env: Env, deposit_token: Address, _vault: Address) {
        env.storage()
            .instance()
            .set(&symbol_short!("TOKEN"), &deposit_token);
        env.storage().instance().set(&symbol_short!("BAL"), &0i128);
    }

    pub fn a_deposit(env: Env, amount: i128, _from: Address) -> i128 {
        // In tests, vault transfers tokens to adapter before calling this.
        // We just track the virtual balance.
        let bal: i128 = env
            .storage()
            .instance()
            .get(&symbol_short!("BAL"))
            .unwrap_or(0);
        let new_bal = bal + amount;
        env.storage()
            .instance()
            .set(&symbol_short!("BAL"), &new_bal);
        new_bal
    }

    pub fn a_withdraw(env: Env, amount: i128, to: Address) -> i128 {
        let bal: i128 = env
            .storage()
            .instance()
            .get(&symbol_short!("BAL"))
            .unwrap_or(0);
        let actual = amount.min(bal);
        env.storage()
            .instance()
            .set(&symbol_short!("BAL"), &(bal - actual));

        // Transfer tokens from adapter to vault (to)
        let token_addr: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("TOKEN"))
            .unwrap();
        let token = TokenClient::new(&env, &token_addr);
        token.transfer(&env.current_contract_address(), &to, &actual);

        actual
    }

    pub fn a_balance(env: Env, _from: Address) -> i128 {
        env.storage()
            .instance()
            .get(&symbol_short!("BAL"))
            .unwrap_or(0)
    }

    pub fn a_get_apy(_env: Env) -> u32 {
        500 // 5% in BPS
    }

    pub fn a_harvest(env: Env, to: Address) -> (Address, i128) {
        let reward_token: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("H_TOK"))
            .unwrap_or_else(|| {
                env.storage()
                    .instance()
                    .get(&symbol_short!("TOKEN"))
                    .unwrap()
            });
        let reward_amount: i128 = env
            .storage()
            .instance()
            .get(&symbol_short!("H_AMT"))
            .unwrap_or(0);

        if reward_amount > 0 {
            let token = TokenClient::new(&env, &reward_token);
            token.transfer(&env.current_contract_address(), &to, &reward_amount);
        }

        (reward_token, reward_amount)
    }

    pub fn set_mock_harvest(env: Env, token: Address, amount: i128) {
        env.storage().instance().set(&symbol_short!("H_TOK"), &token);
        env.storage().instance().set(&symbol_short!("H_AMT"), &amount);
    }
}

// ============================================================================
// Test helpers
// ============================================================================

fn default_config() -> VaultConfig {
    VaultConfig {
        management_fee_bps: 50,       // 0.5%
        performance_fee_bps: 1000,    // 10%
        min_liquidity_bps: 500,       // 5%
        max_protocol_bps: 9000,       // 90%
        rebalance_threshold_bps: 200, // 2%
    }
}

fn create_vault<'a>(env: &'a Env, admin: &Address, token: &Address) -> VaultContractClient<'a> {
    let contract_id = env.register(VaultContract, ());
    let client = VaultContractClient::new(env, &contract_id);
    client.initialize(
        admin,
        admin, // manager = admin for tests
        token,
        &String::from_str(env, "Neko CETES Vault"),
        &String::from_str(env, "vCETES"),
        &7u32,
        &default_config(),
    );
    client
}

fn create_token<'a>(env: &'a Env, admin: &Address) -> (TokenClient<'a>, StellarAssetClient<'a>) {
    let sac = env.register_stellar_asset_contract_v2(admin.clone());
    let token = TokenClient::new(env, &sac.address());
    let token_admin = StellarAssetClient::new(env, &sac.address());
    (token, token_admin)
}

// ============================================================================
// Tests
// ============================================================================

#[test]
fn test_initialize() {
    let env = Env::default();
    let admin = Address::generate(&env);
    let (token, _) = create_token(&env, &admin);

    let vault = create_vault(&env, &admin, &token.address);

    assert_eq!(vault.get_status(), VaultStatus::Active);
    assert_eq!(vault.get_total_shares(), 0);
    assert_eq!(vault.get_liquid_reserve(), 0);
    assert_eq!(vault.get_nav(), 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_double_initialize() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let (token, _) = create_token(&env, &admin);

    let vault = create_vault(&env, &admin, &token.address);

    // Try to initialize again — should panic with AlreadyInitialized (#1)
    vault.initialize(
        &admin,
        &admin,
        &token.address,
        &String::from_str(&env, "Neko CETES Vault"),
        &String::from_str(&env, "vCETES"),
        &7u32,
        &default_config(),
    );
}

#[test]
fn test_deposit_first_is_one_to_one() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let user = Address::generate(&env);
    let (token, token_admin) = create_token(&env, &admin);

    // Mint 1000 tokens to user
    token_admin.mint(&user, &10_000_000_000_i128); // 1000 tokens (7 decimals)

    let vault = create_vault(&env, &admin, &token.address);

    // First deposit: 100 tokens → should receive 100 shares (1:1)
    let amount = 100_0000000i128;
    let shares = vault.deposit(&user, &amount);

    assert_eq!(shares, amount); // 1:1 for first deposit
    assert_eq!(vault.get_total_shares(), amount);
    assert_eq!(vault.get_liquid_reserve(), amount);
    assert_eq!(vault.balance(&user), amount);
}

#[test]
fn test_deposit_share_price_proportional() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let user1 = Address::generate(&env);
    let user2 = Address::generate(&env);
    let (token, token_admin) = create_token(&env, &admin);

    token_admin.mint(&user1, &10_000_000_000_i128);
    token_admin.mint(&user2, &10_000_000_000_i128);

    let vault = create_vault(&env, &admin, &token.address);

    // First deposit: 100 tokens → 100 shares
    let amount1 = 100_0000000i128;
    vault.deposit(&user1, &amount1);

    // Second deposit: same amount → same shares (NAV unchanged)
    let amount2 = 100_0000000i128;
    let shares2 = vault.deposit(&user2, &amount2);

    assert_eq!(shares2, amount2); // Same amount since NAV = 100 and total_shares = 100

    assert_eq!(vault.get_total_shares(), amount1 + amount2);
    assert_eq!(vault.get_liquid_reserve(), amount1 + amount2);
}

#[test]
fn test_withdraw_from_liquid_reserve() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let user = Address::generate(&env);
    let (token, token_admin) = create_token(&env, &admin);

    token_admin.mint(&user, &10_000_000_000_i128);

    let vault = create_vault(&env, &admin, &token.address);
    let amount = 100_0000000i128;
    let shares = vault.deposit(&user, &amount);

    // Withdraw half
    let half_shares = shares / 2;
    let received = vault.withdraw(&user, &half_shares);

    assert_eq!(received, amount / 2);
    assert_eq!(vault.balance(&user), shares - half_shares);
    assert_eq!(vault.get_liquid_reserve(), amount - received);
}

#[test]
fn test_withdraw_full() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let user = Address::generate(&env);
    let (token, token_admin) = create_token(&env, &admin);

    token_admin.mint(&user, &10_000_000_000_i128);

    let vault = create_vault(&env, &admin, &token.address);
    let amount = 100_0000000i128;
    let shares = vault.deposit(&user, &amount);

    let received = vault.withdraw(&user, &shares);
    assert_eq!(received, amount);
    assert_eq!(vault.balance(&user), 0);
    assert_eq!(vault.get_total_shares(), 0);
}

#[test]
fn test_add_remove_protocol() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let adapter = Address::generate(&env);
    let (token, _) = create_token(&env, &admin);

    let vault = create_vault(&env, &admin, &token.address);

    let id = symbol_short!("POOL1");
    vault.add_protocol(&id, &adapter, &5000u32, &RiskTier::Low);

    let protocols = vault.get_protocols();
    assert_eq!(protocols.len(), 1);
    assert_eq!(protocols.get(0).unwrap().0, id);

    vault.remove_protocol(&id);
    assert_eq!(vault.get_protocols().len(), 0);
}

#[test]
fn test_pause_blocks_deposit_not_withdraw() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let user = Address::generate(&env);
    let (token, token_admin) = create_token(&env, &admin);

    token_admin.mint(&user, &10_000_000_000_i128);

    let vault = create_vault(&env, &admin, &token.address);
    let amount = 100_0000000i128;
    let shares = vault.deposit(&user, &amount);

    // Pause the vault
    vault.pause();
    assert_eq!(vault.get_status(), VaultStatus::Paused);

    // Withdraw should still work
    let received = vault.withdraw(&user, &shares);
    assert_eq!(received, amount);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_paused_deposit_panics() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let user = Address::generate(&env);
    let (token, token_admin) = create_token(&env, &admin);

    token_admin.mint(&user, &10_000_000_000_i128);

    let vault = create_vault(&env, &admin, &token.address);
    vault.pause();

    // Should panic with VaultNotActive (#6)
    vault.deposit(&user, &100_0000000i128);
}

#[test]
fn test_sep41_transfer() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let user1 = Address::generate(&env);
    let user2 = Address::generate(&env);
    let (token, token_admin) = create_token(&env, &admin);

    token_admin.mint(&user1, &10_000_000_000_i128);

    let vault = create_vault(&env, &admin, &token.address);
    let amount = 100_0000000i128;
    vault.deposit(&user1, &amount);

    // Transfer 30 shares from user1 to user2
    vault.transfer(&user1, &user2, &30_0000000i128);

    assert_eq!(vault.balance(&user1), 70_0000000i128);
    assert_eq!(vault.balance(&user2), 30_0000000i128);
}

#[test]
fn test_sep41_approve_transfer_from() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let user1 = Address::generate(&env);
    let user2 = Address::generate(&env);
    let spender = Address::generate(&env);
    let (token, token_admin) = create_token(&env, &admin);

    token_admin.mint(&user1, &10_000_000_000_i128);

    let vault = create_vault(&env, &admin, &token.address);
    vault.deposit(&user1, &100_0000000i128);

    // Approve spender to transfer 50 shares
    vault.approve(&user1, &spender, &50_0000000i128, &1000u32);
    assert_eq!(vault.allowance(&user1, &spender), 50_0000000i128);

    // Spender transfers 30 from user1 to user2
    vault.transfer_from(&spender, &user1, &user2, &30_0000000i128);

    assert_eq!(vault.balance(&user1), 70_0000000i128);
    assert_eq!(vault.balance(&user2), 30_0000000i128);
    assert_eq!(vault.allowance(&user1, &spender), 20_0000000i128);
}

#[test]
fn test_decimals_name_symbol() {
    let env = Env::default();
    let admin = Address::generate(&env);
    let (token, _) = create_token(&env, &admin);

    let vault = create_vault(&env, &admin, &token.address);

    assert_eq!(vault.decimals(), 7u32);
    assert_eq!(vault.name(), String::from_str(&env, "Neko CETES Vault"));
    assert_eq!(vault.symbol(), String::from_str(&env, "vCETES"));
}

// ============================================================================
// Mock Router Contract
// ============================================================================

#[contract]
struct MockRouter;

#[contractimpl]
impl MockRouter {
    pub fn router_pair_for(env: Env, _token_a: Address, _token_b: Address) -> Address {
        let key = symbol_short!("PAIR");
        env.storage().instance().get(&key).unwrap_or_else(|| {
            Address::generate(&env)
        })
    }

    pub fn set_pair(env: Env, pair: Address) {
        env.storage().instance().set(&symbol_short!("PAIR"), &pair);
    }

    pub fn router_get_amounts_out(env: Env, amount_in: i128, _path: soroban_sdk::Vec<soroban_sdk::Address>) -> soroban_sdk::Vec<i128> {
        let rate = env.storage().instance().get(&symbol_short!("E_RATE"))
            .unwrap_or_else(|| env.storage().instance().get(&symbol_short!("RATE")).unwrap_or(100i128));
        let amount_out = amount_in * rate / 100;
        soroban_sdk::vec![&env, amount_in, amount_out]
    }

    pub fn set_rate(env: Env, rate: i128) {
        env.storage().instance().set(&symbol_short!("RATE"), &rate);
    }

    pub fn set_rates(env: Env, estimate_rate: i128, swap_rate: i128) {
        env.storage().instance().set(&symbol_short!("E_RATE"), &estimate_rate);
        env.storage().instance().set(&symbol_short!("S_RATE"), &swap_rate);
    }

    pub fn swap_exact_tokens_for_tokens(
        env: Env,
        amount_in: i128,
        amount_out_min: i128,
        path: soroban_sdk::Vec<soroban_sdk::Address>,
        to: Address,
        _deadline: u64,
    ) -> soroban_sdk::Vec<i128> {
        let token_in_addr = path.get(0).unwrap();
        let token_out_addr = path.get(1).unwrap();
        
        let rate = env.storage().instance().get(&symbol_short!("S_RATE"))
            .unwrap_or_else(|| env.storage().instance().get(&symbol_short!("RATE")).unwrap_or(100i128));
        let amount_out = amount_in * rate / 100;
        
        if amount_out < amount_out_min {
            soroban_sdk::panic_with_error!(&env, crate::common::error::Error::ArithmeticError);
        }

        let pair = Self::router_pair_for(env.clone(), token_in_addr.clone(), token_out_addr.clone());
        let token_in = TokenClient::new(&env, &token_in_addr);
        token_in.transfer(&to, &pair, &amount_in);

        let token_out = TokenClient::new(&env, &token_out_addr);
        token_out.transfer(&env.current_contract_address(), &to, &amount_out);

        soroban_sdk::vec![&env, amount_in, amount_out]
    }
}

// ============================================================================
// Harvest Swap Path Tests
// ============================================================================

#[test]
fn test_harvest_zero() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token, _) = create_token(&env, &admin);
    let vault = create_vault(&env, &admin, &token.address);

    let adapter_id = env.register(MockAdapter, ());
    MockAdapterClient::new(&env, &adapter_id).initialize(&token.address, &vault.address);
    vault.add_protocol(&symbol_short!("MOCK"), &adapter_id, &10000u32, &RiskTier::Low);

    let harvested = vault.harvest_all();
    assert_eq!(harvested, 0);
    assert_eq!(vault.get_liquid_reserve(), 0);
}

#[test]
fn test_harvest_same_token() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (token, token_admin) = create_token(&env, &admin);
    let vault = create_vault(&env, &admin, &token.address);

    let adapter_id = env.register(MockAdapter, ());
    let adapter = MockAdapterClient::new(&env, &adapter_id);
    adapter.initialize(&token.address, &vault.address);
    vault.add_protocol(&symbol_short!("MOCK"), &adapter_id, &10000u32, &RiskTier::Low);

    let reward_amount = 50_0000000i128;
    token_admin.mint(&adapter_id, &reward_amount);
    adapter.set_mock_harvest(&token.address, &reward_amount);

    let harvested = vault.harvest_all();
    assert_eq!(harvested, reward_amount);
    assert_eq!(vault.get_liquid_reserve(), reward_amount);
}

#[test]
fn test_harvest_swap_success() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (deposit_token, deposit_admin) = create_token(&env, &admin);
    let (reward_token, reward_admin) = create_token(&env, &admin);
    let vault = create_vault(&env, &admin, &deposit_token.address);

    let adapter_id = env.register(MockAdapter, ());
    let adapter = MockAdapterClient::new(&env, &adapter_id);
    adapter.initialize(&deposit_token.address, &vault.address);
    vault.add_protocol(&symbol_short!("MOCK"), &adapter_id, &10000u32, &RiskTier::Low);

    let router_id = env.register(MockRouter, ());
    let router = MockRouterClient::new(&env, &router_id);
    let pair = Address::generate(&env);
    router.set_pair(&pair);
    router.set_rate(&90);

    deposit_admin.mint(&router_id, &1000_0000000i128);

    let config = HarvestConfig {
        reward_token: reward_token.address.clone(),
        swap_router: router_id.clone(),
        min_swap_amount: 10_0000000i128,
        max_slippage_bps: 1000,
    };
    vault.set_harvest_config(&config);

    let reward_amount = 100_0000000i128;
    reward_admin.mint(&adapter_id, &reward_amount);
    adapter.set_mock_harvest(&reward_token.address, &reward_amount);

    let harvested = vault.harvest_all();
    assert_eq!(harvested, 90_0000000i128);
    assert_eq!(vault.get_liquid_reserve(), 90_0000000i128);
}

#[test]
fn test_harvest_swap_dust_skipped() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (deposit_token, _) = create_token(&env, &admin);
    let (reward_token, reward_admin) = create_token(&env, &admin);
    let vault = create_vault(&env, &admin, &deposit_token.address);

    let adapter_id = env.register(MockAdapter, ());
    let adapter = MockAdapterClient::new(&env, &adapter_id);
    adapter.initialize(&deposit_token.address, &vault.address);
    vault.add_protocol(&symbol_short!("MOCK"), &adapter_id, &10000u32, &RiskTier::Low);

    let router_id = env.register(MockRouter, ());
    let router = MockRouterClient::new(&env, &router_id);
    let pair = Address::generate(&env);
    router.set_pair(&pair);

    let config = HarvestConfig {
        reward_token: reward_token.address.clone(),
        swap_router: router_id.clone(),
        min_swap_amount: 10_0000000i128,
        max_slippage_bps: 1000,
    };
    vault.set_harvest_config(&config);

    let reward_amount = 5_0000000i128;
    reward_admin.mint(&adapter_id, &reward_amount);
    adapter.set_mock_harvest(&reward_token.address, &reward_amount);

    let harvested = vault.harvest_all();
    assert_eq!(harvested, 0);
    assert_eq!(vault.get_liquid_reserve(), 0);

    assert_eq!(reward_token.balance(&vault.address), reward_amount);
}

#[test]
fn test_harvest_swap_slippage_rejection() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let (deposit_token, deposit_admin) = create_token(&env, &admin);
    let (reward_token, reward_admin) = create_token(&env, &admin);
    let vault = create_vault(&env, &admin, &deposit_token.address);

    let adapter_id = env.register(MockAdapter, ());
    let adapter = MockAdapterClient::new(&env, &adapter_id);
    adapter.initialize(&deposit_token.address, &vault.address);
    vault.add_protocol(&symbol_short!("MOCK"), &adapter_id, &10000u32, &RiskTier::Low);

    let router_id = env.register(MockRouter, ());
    let router = MockRouterClient::new(&env, &router_id);
    let pair = Address::generate(&env);
    router.set_pair(&pair);
    router.set_rates(&100, &80);

    deposit_admin.mint(&router_id, &1000_0000000i128);

    let config = HarvestConfig {
        reward_token: reward_token.address.clone(),
        swap_router: router_id.clone(),
        min_swap_amount: 10_0000000i128,
        max_slippage_bps: 500,
    };
    vault.set_harvest_config(&config);

    let reward_amount = 100_0000000i128;
    reward_admin.mint(&adapter_id, &reward_amount);
    adapter.set_mock_harvest(&reward_token.address, &reward_amount);

    let harvested = vault.harvest_all();
    assert_eq!(harvested, 0);
    assert_eq!(vault.get_liquid_reserve(), 0);
}
