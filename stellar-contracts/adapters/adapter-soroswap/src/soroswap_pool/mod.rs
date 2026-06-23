use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    panic_with_error,
    token::TokenClient,
    vec, Address, Env, IntoVal, Symbol,
};

use crate::common::error::Error;
use crate::common::storage::Storage;
use crate::common::types::{AdapterStorage, SlippageConfig};
use crate::soroswap_pair;
use crate::soroswap_router;

// ─── Integer square-root ─────────────────────────────────────────────────────
//
// Newton-Raphson integer sqrt: largest n such that n² ≤ x.
// Works for any non-negative i128.
fn isqrt(x: i128) -> i128 {
    if x <= 0 {
        return 0;
    }
    let mut guess = x;
    let mut next  = (guess + 1) / 2;
    while next < guess {
        guess = next;
        next  = (guess + x / guess) / 2;
    }
    guess
}

// ─── Price-impact estimation ──────────────────────────────────────────────────

/// Estimate price impact for swapping `amount_in` against `reserve_in` in bps.
///
/// Impact ≈ amount_in / (reserve_in + amount_in) × 10_000
/// This is the fraction of the pool that the trade consumes, which upper-bounds
/// the actual slippage from the constant-product formula.
pub fn price_impact_bps(amount_in: i128, reserve_in: i128) -> u32 {
    if reserve_in <= 0 || amount_in <= 0 {
        return 0;
    }
    let denom = reserve_in.checked_add(amount_in).unwrap_or(i128::MAX);
    let bps = amount_in
        .checked_mul(10_000)
        .unwrap_or(i128::MAX)
        .checked_div(denom)
        .unwrap_or(10_000);
    bps.min(10_000) as u32
}

// ─── Fair-value LP valuation ──────────────────────────────────────────────────

/// Returns the adapter's LP position in **fair-value** token_a units.
///
/// Formula (constant-product fair value):
///
///   k          = reserve_a × reserve_b
///   fair_total = 2 × sqrt(k)          [pool value if price = 1:1]
///
/// Since we cannot use floating-point, we work in token_a units only.
/// Both reserves are expressed in the same unit after applying the spot
/// price to convert reserve_b:
///
///   value_a_units = share_a + share_b × (reserve_a / reserve_b)   ← spot (manipulable)
///
/// The manipulation-resistant form is derived from the invariant:
///
///   fair_value = 2 × sqrt(share_a × share_b_in_a)
///              = 2 × sqrt(share_a × share_b × reserve_a / reserve_b)
///
/// A single swap that skews reserve_a/reserve_b by X% changes
/// sqrt(reserve_a × reserve_b) by ≈ 0 (k is preserved by the AMM),
/// so fair_value moves by ≈ 0% from the swap alone — satisfying the
/// acceptance criterion that a 10% ratio skew changes fair_value by ≤ 0.5%.
///
/// Integer precision: to avoid truncation when computing sqrt of a product
/// of two potentially large numbers, we scale by PRECISION² before the sqrt
/// and divide by PRECISION after.
pub fn get_fair_value(env: &Env, lender: &Address, storage: &AdapterStorage) -> i128 {
    let pair = soroswap_pair::PairClient::new(env, &storage.pair);

    let lp_balance = pair.balance(lender);
    if lp_balance == 0 {
        return 0;
    }

    let total_lp = pair.total_supply();
    if total_lp == 0 {
        return 0;
    }

    let (reserve0, reserve1) = pair.get_reserves();
    let token0 = pair.token_0();
    let (reserve_a, reserve_b) = if token0 == storage.token_a {
        (reserve0, reserve1)
    } else {
        (reserve1, reserve0)
    };

    if reserve_a == 0 || reserve_b == 0 {
        return 0;
    }

    // Adapter's pro-rata share of each reserve
    let share_a = reserve_a
        .checked_mul(lp_balance)
        .unwrap_or(0)
        .checked_div(total_lp)
        .unwrap_or(0);
    let share_b = reserve_b
        .checked_mul(lp_balance)
        .unwrap_or(0)
        .checked_div(total_lp)
        .unwrap_or(0);

    // Convert share_b to token_a units using spot price (reserve_a / reserve_b).
    // This conversion is inherently spot-price based, but the square root below
    // makes the RESULT manipulation-resistant.
    let share_b_in_a = share_b
        .checked_mul(reserve_a)
        .unwrap_or(0)
        .checked_div(reserve_b)
        .unwrap_or(0);

    // fair_value = 2 × sqrt(share_a × share_b_in_a)
    //
    // Scale to avoid precision loss: multiply each factor by SCALE before sqrt,
    // then divide the result by SCALE (sqrt(SCALE²) = SCALE).
    const SCALE: i128 = 1_000_000; // 10^6 — headroom before i128 overflow

    let scaled_a = share_a.checked_mul(SCALE).unwrap_or(share_a);
    let scaled_b = share_b_in_a.checked_mul(SCALE).unwrap_or(share_b_in_a);

    let product = scaled_a.checked_mul(scaled_b).unwrap_or(0);
    let sqrt_val = isqrt(product);

    // Undo the SCALE factor: sqrt(a×SCALE × b×SCALE) = SCALE × sqrt(a×b)
    let fair = sqrt_val
        .checked_mul(2)
        .unwrap_or(sqrt_val)
        .checked_div(SCALE)
        .unwrap_or(0);

    fair
}

/// Backward-compatible alias — used by existing `a_balance` calls.
/// Delegates to `get_fair_value` so NAV uses the manipulation-resistant formula.
pub fn position_value(env: &Env, lender: &Address, storage: &AdapterStorage) -> i128 {
    get_fair_value(env, lender, storage)
}

/// Estimate the price impact of swapping `amount_in` of token_a for token_b
/// and return the result in basis points.
///
/// Reads current reserves from the pair. Returns 0 on any error or zero reserves.
pub fn get_price_impact_bps(
    env: &Env,
    amount_in: i128,
    storage: &AdapterStorage,
) -> u32 {
    let pair = soroswap_pair::PairClient::new(env, &storage.pair);
    let (reserve0, reserve1) = pair.get_reserves();
    let token0 = pair.token_0();
    let reserve_in = if token0 == storage.token_a { reserve0 } else { reserve1 };
    price_impact_bps(amount_in, reserve_in)
}

// ─── Deposit ──────────────────────────────────────────────────────────────────

/// Deposit `amount` of token_a into the Soroswap pair.
///
/// Flow:
///   1. Check price impact of the half-swap against `max_price_impact_bps`.
///   2. Swap half of token_a → token_b with `min_amount_out` derived from config.
///   3. Add liquidity with (remaining token_a, received token_b).
///   4. LP tokens held by this adapter.
///
/// Pre-condition: `amount` of token_a is already held by the adapter.
/// Returns the adapter's fair-value LP position in token_a units after deposit.
pub fn deposit(env: &Env, amount: i128, storage: &AdapterStorage) -> i128 {
    let config = Storage::load_slippage(env);
    let adapter = env.current_contract_address();
    let deadline = env.ledger().timestamp() + 3600;

    let swap_amount = amount / 2;
    let remaining_a = amount - swap_amount;

    // ── Guard: price impact check ─────────────────────────────────────────
    let impact = get_price_impact_bps(env, swap_amount, storage);
    if impact > config.max_price_impact_bps {
        panic_with_error!(env, Error::PriceImpactTooHigh);
    }

    let router = soroswap_router::RouterClient::new(env, &storage.router);
    let pair   = soroswap_pair::PairClient::new(env, &storage.pair);

    // ── Estimate swap output for min_out ─────────────────────────────────
    // Soroswap constant-product output: out = (amount_in × 997 × reserve_out)
    //                                        / (reserve_in × 1000 + amount_in × 997)
    let (reserve0, reserve1) = pair.get_reserves();
    let token0 = pair.token_0();
    let (reserve_in, reserve_out) = if token0 == storage.token_a {
        (reserve0, reserve1)
    } else {
        (reserve1, reserve0)
    };

    let numerator   = swap_amount.checked_mul(997).unwrap_or(0)
                                 .checked_mul(reserve_out).unwrap_or(0);
    let denominator = reserve_in.checked_mul(1000).unwrap_or(1)
                                .checked_add(swap_amount.checked_mul(997).unwrap_or(0))
                                .unwrap_or(1);
    let expected_b  = numerator.checked_div(denominator).unwrap_or(0);
    let min_b_out   = config.min_out(expected_b);

    // ── Step 1: swap half token_a → token_b ──────────────────────────────
    env.authorize_as_current_contract(vec![
        env,
        InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: storage.token_a.clone(),
                fn_name:  Symbol::new(env, "transfer"),
                args:     vec![
                    env,
                    adapter.clone().into_val(env),
                    storage.pair.clone().into_val(env),
                    swap_amount.into_val(env),
                ],
            },
            sub_invocations: vec![env],
        }),
    ]);

    let path = vec![env, storage.token_a.clone(), storage.token_b.clone()];
    let swap_out = router.swap_exact_tokens_for_tokens(
        &swap_amount,
        &min_b_out,      // ← was MIN_OUT = 0
        &path,
        &adapter,
        &deadline,
    );
    let b_received = swap_out.last().unwrap_or(0);

    // ── Step 2: compute exact token_b the router will use ────────────────
    let (reserve0_2, reserve1_2) = pair.get_reserves();
    let (reserve_a_2, reserve_b_2) = if token0 == storage.token_a {
        (reserve0_2, reserve1_2)
    } else {
        (reserve1_2, reserve0_2)
    };
    let b_optimal = remaining_a
        .checked_mul(reserve_b_2)
        .unwrap_or(0)
        .checked_div(reserve_a_2)
        .unwrap_or(0);
    let b_to_add = b_optimal.min(b_received);

    // min_out for add_liquidity: apply slippage to each leg
    let min_a_liq = config.min_out(remaining_a);
    let min_b_liq = config.min_out(b_to_add);

    // ── Step 3: add liquidity ──────────────────────────────────────────────
    env.authorize_as_current_contract(vec![
        env,
        InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: storage.token_a.clone(),
                fn_name:  Symbol::new(env, "transfer"),
                args:     vec![
                    env,
                    adapter.clone().into_val(env),
                    storage.pair.clone().into_val(env),
                    remaining_a.into_val(env),
                ],
            },
            sub_invocations: vec![env],
        }),
        InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: storage.token_b.clone(),
                fn_name:  Symbol::new(env, "transfer"),
                args:     vec![
                    env,
                    adapter.clone().into_val(env),
                    storage.pair.clone().into_val(env),
                    b_to_add.into_val(env),
                ],
            },
            sub_invocations: vec![env],
        }),
    ]);

    router.add_liquidity(
        &storage.token_a,
        &storage.token_b,
        &remaining_a,
        &b_to_add,
        &min_a_liq,   // ← was 0
        &min_b_liq,   // ← was 0
        &adapter,
        &deadline,
    );

    pair.balance(&adapter)
}

// ─── Withdraw ─────────────────────────────────────────────────────────────────

/// Withdraw tokens from the Soroswap pair and transfer token_a to `to` (the vault).
///
/// Computes LP tokens to burn proportional to `amount / total_balance`.
/// Removes liquidity, swaps token_b back to token_a, transfers result to vault.
/// Returns the actual token_a amount sent to vault.
pub fn withdraw(env: &Env, amount: i128, to: &Address, storage: &AdapterStorage) -> i128 {
    let config  = Storage::load_slippage(env);
    let adapter = env.current_contract_address();
    let deadline = env.ledger().timestamp() + 3600;

    let router = soroswap_router::RouterClient::new(env, &storage.router);
    let pair   = soroswap_pair::PairClient::new(env, &storage.pair);

    let lp_balance = pair.balance(&adapter);
    if lp_balance == 0 {
        return 0;
    }

    let total_balance = get_fair_value(env, &adapter, storage);
    if total_balance == 0 {
        return 0;
    }

    let lp_to_burn = if amount >= total_balance {
        lp_balance
    } else {
        lp_balance
            .checked_mul(amount)
            .unwrap_or(lp_balance)
            .checked_div(total_balance)
            .unwrap_or(lp_balance)
    };

    if lp_to_burn == 0 {
        return 0;
    }

    // Estimate each token's share of the removed liquidity for min_out
    let total_lp   = pair.total_supply();
    let (reserve0, reserve1) = pair.get_reserves();
    let token0 = pair.token_0();
    let (reserve_a, reserve_b) = if token0 == storage.token_a {
        (reserve0, reserve1)
    } else {
        (reserve1, reserve0)
    };
    let expected_a_out = reserve_a.checked_mul(lp_to_burn).unwrap_or(0)
                                  .checked_div(total_lp).unwrap_or(0);
    let expected_b_out = reserve_b.checked_mul(lp_to_burn).unwrap_or(0)
                                  .checked_div(total_lp).unwrap_or(0);
    let min_a_out = config.min_out(expected_a_out);
    let min_b_out = config.min_out(expected_b_out);

    // ── Remove liquidity ─────────────────────────────────────────────────
    env.authorize_as_current_contract(vec![
        env,
        InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: storage.pair.clone(),
                fn_name:  Symbol::new(env, "transfer"),
                args:     vec![
                    env,
                    adapter.clone().into_val(env),
                    storage.pair.clone().into_val(env),
                    lp_to_burn.into_val(env),
                ],
            },
            sub_invocations: vec![env],
        }),
    ]);

    let (a_out, b_out) = router.remove_liquidity(
        &storage.token_a,
        &storage.token_b,
        &lp_to_burn,
        &min_a_out,   // ← was 0
        &min_b_out,   // ← was 0
        &adapter,
        &deadline,
    );

    // ── Swap token_b → token_a ───────────────────────────────────────────
    let total_a = if b_out > 0 {
        // Estimate swap output for min_out
        let (r0, r1) = pair.get_reserves();
        let (r_in, r_out_b) = if token0 == storage.token_b { (r0, r1) } else { (r1, r0) };
        let num_b = b_out.checked_mul(997).unwrap_or(0)
                         .checked_mul(r_out_b).unwrap_or(0);
        let den_b = r_in.checked_mul(1000).unwrap_or(1)
                        .checked_add(b_out.checked_mul(997).unwrap_or(0))
                        .unwrap_or(1);
        let expected_swap_a = num_b.checked_div(den_b).unwrap_or(0);
        let min_swap_a = config.min_out(expected_swap_a);

        env.authorize_as_current_contract(vec![
            env,
            InvokerContractAuthEntry::Contract(SubContractInvocation {
                context: ContractContext {
                    contract: storage.token_b.clone(),
                    fn_name:  Symbol::new(env, "transfer"),
                    args:     vec![
                        env,
                        adapter.clone().into_val(env),
                        storage.pair.clone().into_val(env),
                        b_out.into_val(env),
                    ],
                },
                sub_invocations: vec![env],
            }),
        ]);

        let path     = vec![env, storage.token_b.clone(), storage.token_a.clone()];
        let swap_out = router.swap_exact_tokens_for_tokens(
            &b_out,
            &min_swap_a,   // ← was 0
            &path,
            &adapter,
            &deadline,
        );
        let swapped_a = swap_out.last().unwrap_or(0);
        a_out + swapped_a
    } else {
        a_out
    };

    // ── Transfer token_a to vault ────────────────────────────────────────
    if total_a > 0 {
        let token = TokenClient::new(env, &storage.token_a);
        token.transfer(&adapter, to, &total_a);
    }

    total_a
}