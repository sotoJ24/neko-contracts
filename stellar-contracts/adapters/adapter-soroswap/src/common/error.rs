use soroban_sdk::contracterror;

#[contracterror]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    AlreadyInitialized   = 1,
    NotInitialized       = 2,
    NotVault             = 3,
    NotAdmin             = 4,
    ZeroAmount           = 5,
    ArithmeticError      = 6,
    InsufficientBalance  = 7,
    PairNotFound         = 8,
    /// Operation would exceed `max_price_impact_bps`.
    PriceImpactTooHigh   = 9,
    /// Swap or liquidity output fell below computed `min_amount_out`.
    SlippageTooHigh      = 10,
}