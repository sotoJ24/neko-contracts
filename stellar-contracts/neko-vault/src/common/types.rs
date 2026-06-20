use soroban_sdk::{Address, Map, String, Symbol, Vec, contracttype};

// ============================================================================
// SCALAR CONSTANTS
// ============================================================================

pub const SCALAR_7: i128 = 10_000_000;
pub const BPS: i128 = 10_000;
pub const SECONDS_PER_YEAR: u64 = 31_536_000;
pub const MAX_PROTOCOLS: u32 = 10;

// ============================================================================
// TTL CONSTANTS
// ============================================================================

pub const ONE_DAY_LEDGERS: u32 = 17280;
pub const INSTANCE_TTL: u32 = ONE_DAY_LEDGERS * 30;
pub const INSTANCE_BUMP: u32 = ONE_DAY_LEDGERS * 31;

// ============================================================================
// STORAGE KEY
// ============================================================================

pub use soroban_sdk::symbol_short;
pub const STORAGE_KEY: Symbol = symbol_short!("VSTORAGE");

// ============================================================================
// VAULT TYPES
// ============================================================================

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VaultStatus {
    Active,
    Paused,
    EmergencyExit,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RiskTier {
    Low,
    Medium,
    High,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct VaultConfig {
    /// Annual management fee in BPS (e.g. 50 = 0.5%)
    pub management_fee_bps: u32,
    /// Performance fee on gains above HWM in BPS (e.g. 1000 = 10%)
    pub performance_fee_bps: u32,
    /// Minimum liquid reserve in BPS (e.g. 500 = 5%)
    pub min_liquidity_bps: u32,
    /// Maximum allocation per protocol in BPS (e.g. 9000 = 90%)
    pub max_protocol_bps: u32,
    /// Minimum diff to trigger rebalance in BPS (e.g. 200 = 2%)
    pub rebalance_threshold_bps: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HarvestConfig {
    pub reward_token: Address,
    pub swap_router: Address,
    pub min_swap_amount: i128,
    pub max_slippage_bps: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct ProtocolAllocation {
    /// Adapter contract address
    pub adapter: Address,
    /// Target % of NAV in BPS (5000 = 50%)
    pub target_bps: u32,
    /// Whether this protocol is currently active
    pub is_active: bool,
    /// Risk classification
    pub risk_tier: RiskTier,
}

/// Main vault storage (instance storage)
#[contracttype]
#[derive(Clone, Debug)]
pub struct VaultStorage {
    pub status: VaultStatus,
    pub deposit_token: Address,
    pub token_name: String,
    pub token_symbol: String,
    pub token_decimals: u32,
    /// Tokens held in the vault (not deployed to any protocol)
    pub liquid_reserve: i128,
    /// Total vTokens minted
    pub total_shares: i128,
    /// Highest-ever share price (SCALAR_7 = 1.0 initially)
    pub high_water_mark: i128,
    /// Last fee accrual timestamp
    pub last_fee_accrual: u64,
    /// Ordered list of protocol IDs
    pub protocol_ids: Vec<Symbol>,
    /// Per-protocol configuration and allocation
    pub protocol_allocations: Map<Symbol, ProtocolAllocation>,
    pub config: VaultConfig,
    pub admin: Address,
    pub manager: Address,
}

// ============================================================================
// SEP-41 vToken storage types (persistent storage)
// ============================================================================

#[contracttype]
pub enum DataKey {
    Balance(Address),
    Allowance(Txn),
}

#[contracttype]
#[derive(Clone)]
pub struct Txn(pub Address, pub Address);

#[contracttype]
#[derive(Clone)]
pub struct VaultAllowance {
    pub amount: i128,
    pub live_until_ledger: u32,
}
