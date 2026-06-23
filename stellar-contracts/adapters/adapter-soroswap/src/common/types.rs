use soroban_sdk::{contracttype, Address};

#[contracttype]
#[derive(Clone, Debug)]
pub struct AdapterStorage {
    pub vault:   Address,  // Authorized neko-vault address
    pub router:  Address,  // Soroswap router contract
    pub pair:    Address,  // Soroswap pair (token_a / token_b)
    pub token_a: Address,  // deposit_token (single-asset entry point, e.g. USDC)
    pub token_b: Address,  // pair token (e.g. XLM)
    pub admin:   Address,
}

/// Slippage and price-impact limits for all swap/liquidity operations.
///
/// Stored separately from `AdapterStorage` so it can be updated by admin
/// without re-initialising the adapter.
///
/// Units are basis points (bps): 1 bps = 0.01%.
/// Examples:
///   max_slippage_bps   = 50  →  0.5% maximum slippage per swap / liquidity op
///   max_price_impact_bps = 100 → 1.0% maximum price impact per operation
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct SlippageConfig {
    /// Maximum acceptable slippage on swap and liquidity calls.
    /// Used to compute `min_amount_out = expected * (10_000 - max_slippage_bps) / 10_000`.
    pub max_slippage_bps: u32,
    /// Maximum acceptable price impact before the operation is rejected.
    /// Estimated as `amount_in / (reserve_in + amount_in)` in bps.
    pub max_price_impact_bps: u32,
}

impl SlippageConfig {
    /// Conservative defaults: 0.5% slippage, 1% price impact.
    pub fn default() -> Self {
        Self {
            max_slippage_bps: 50,
            max_price_impact_bps: 100,
        }
    }

    /// Apply slippage to an expected output amount.
    /// Returns `expected * (10_000 - max_slippage_bps) / 10_000`.
    pub fn min_out(&self, expected: i128) -> i128 {
        expected
            .checked_mul((10_000 - self.max_slippage_bps as i128))
            .unwrap_or(0)
            .checked_div(10_000)
            .unwrap_or(0)
    }
}