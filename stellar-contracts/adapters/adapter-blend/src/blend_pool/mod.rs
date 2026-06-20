use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    vec, Address, Env, IntoVal,
};

use crate::blend;
use crate::common::types::{AdapterStorage, REQUEST_SUPPLY, REQUEST_WITHDRAW, SCALAR_12};

/// Supply `amount` of deposit_token to the Blend pool.
///
/// Pre-condition: `amount` tokens are already held by the adapter.
/// Pre-authorizes the token.transfer(adapter → pool, amount) that pool.submit() will execute.
/// Returns the adapter's updated position value in deposit_token units.
pub fn supply(env: &Env, amount: i128, storage: &AdapterStorage) -> i128 {
    let adapter_addr = env.current_contract_address();

    // Pre-authorize the token.transfer(adapter, pool, amount) that Blend will execute
    env.authorize_as_current_contract(vec![
        env,
        InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: storage.deposit_token.clone(),
                fn_name: soroban_sdk::symbol_short!("transfer"),
                args: vec![
                    env,
                    adapter_addr.clone().into_val(env),
                    storage.blend_pool.clone().into_val(env),
                    amount.into_val(env),
                ],
            },
            sub_invocations: vec![env],
        }),
    ]);

    let pool = blend::PoolClient::new(env, &storage.blend_pool);
    let requests = vec![
        env,
        blend::Request {
            request_type: REQUEST_SUPPLY,
            address: storage.deposit_token.clone(),
            amount,
        },
    ];

    // submit(from=adapter, spender=adapter, to=adapter, requests)
    // Blend checks lender.require_auth() — adapter is the invoker → PASS
    pool.submit(&adapter_addr, &adapter_addr, &adapter_addr, &requests);

    position_value(env, &adapter_addr, storage)
}

/// Withdraw `amount` of deposit_token from the Blend pool and transfer to `to` (the vault).
///
/// Blend sends the withdrawn tokens directly to `to` via the `to` parameter in submit().
/// Returns the actual amount received (may differ slightly from requested due to b_rate rounding).
pub fn withdraw(env: &Env, amount: i128, to: &Address, storage: &AdapterStorage) -> i128 {
    let adapter_addr = env.current_contract_address();
    let pool = blend::PoolClient::new(env, &storage.blend_pool);

    // Convert requested underlying amount to b_tokens (ceiling division to avoid dust)
    let b_rate = pool.get_reserve(&storage.deposit_token).data.b_rate;
    let b_tokens_to_burn = amount
        .checked_mul(SCALAR_12)
        .unwrap_or(i128::MAX)
        .checked_add(b_rate - 1)
        .unwrap_or(i128::MAX)
        .checked_div(b_rate)
        .unwrap_or(0);

    if b_tokens_to_burn == 0 {
        return 0;
    }

    // Cap at adapter's actual b_token balance
    let adapter_b_tokens = pool
        .get_positions(&adapter_addr)
        .supply
        .try_get(storage.reserve_id)
        .unwrap_or(Some(0))
        .unwrap_or(0);

    let b_tokens_actual = b_tokens_to_burn.min(adapter_b_tokens);
    if b_tokens_actual == 0 {
        return 0;
    }

    // Actual underlying = b_tokens_actual * b_rate / SCALAR_12 (floor)
    let actual_amount = b_tokens_actual
        .checked_mul(b_rate)
        .unwrap_or(0)
        .checked_div(SCALAR_12)
        .unwrap_or(0);

    let requests = vec![
        env,
        blend::Request {
            request_type: REQUEST_WITHDRAW,
            address: storage.deposit_token.clone(),
            amount: actual_amount,
        },
    ];

    // submit(from=adapter, spender=adapter, to=vault)
    // Blend transfers tokens directly to `to` — no pre-auth needed from adapter side
    pool.submit(&adapter_addr, &adapter_addr, to, &requests);

    actual_amount
}

/// Claim BLND emissions from the pool directly to `to` (the vault), and return the token address and amount claimed.
pub fn claim(env: &Env, to: &Address, storage: &AdapterStorage) -> (Address, i128) {
    let adapter_addr = env.current_contract_address();
    let pool = blend::PoolClient::new(env, &storage.blend_pool);

    // Claim BLND emissions directly to the vault (to)
    let blnd_claimed = pool.claim(&adapter_addr, &storage.claim_ids, to);

    (storage.blend_token.clone(), blnd_claimed)
}

/// Returns the adapter's current position value in deposit_token units.
/// value = b_tokens * b_rate / SCALAR_12
pub fn position_value(env: &Env, lender: &Address, storage: &AdapterStorage) -> i128 {
    let pool = blend::PoolClient::new(env, &storage.blend_pool);

    let b_tokens = pool
        .get_positions(lender)
        .supply
        .try_get(storage.reserve_id)
        .unwrap_or(Some(0))
        .unwrap_or(0);

    if b_tokens == 0 {
        return 0;
    }

    let b_rate = pool.get_reserve(&storage.deposit_token).data.b_rate;

    b_tokens
        .checked_mul(b_rate)
        .unwrap_or(0)
        .checked_div(SCALAR_12)
        .unwrap_or(0)
}
