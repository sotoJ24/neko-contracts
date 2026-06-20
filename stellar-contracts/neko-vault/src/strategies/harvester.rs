use soroban_sdk::{Env, IntoVal};

pub mod soroswap_router {
    soroban_sdk::contractimport!(file = "../wasms/external_wasms/soroswap/router.wasm");
    pub type RouterClient<'a> = Client<'a>;
}

use crate::adapters::AdapterClient;
use crate::common::error::Error;
use crate::common::events::Events;
use crate::common::storage::Storage;
use crate::common::types::{BPS, SCALAR_7, SECONDS_PER_YEAR};
use crate::vault::nav::Nav;
use crate::vault::shares::Shares;

pub struct Harvester;

impl Harvester {
    /// Call a_harvest() on all active protocols and accumulate rewards in liquid_reserve.
    /// Also accrues the management fee.
    pub fn harvest_all(env: &Env) -> Result<i128, Error> {
        let mut storage = Storage::load(env);
        let vault_addr = env.current_contract_address();
        let mut total_harvested = 0i128;

        let harvest_config = Storage::get_harvest_config(env);

        for protocol_id in storage.protocol_ids.iter() {
            if let Some(alloc) = storage.protocol_allocations.get(protocol_id.clone()) {
                if !alloc.is_active {
                    continue;
                }
                let adapter = AdapterClient::new(env, &alloc.adapter);
                let (reward_token, amount) = adapter.a_harvest(&vault_addr);
                if amount <= 0 {
                    continue;
                }

                if reward_token == storage.deposit_token {
                    total_harvested = total_harvested
                        .checked_add(amount)
                        .ok_or(Error::ArithmeticError)?;
                } else {
                    let config = harvest_config.as_ref().ok_or(Error::InvalidConfig)?;
                    if reward_token != config.reward_token {
                        return Err(Error::InvalidConfig);
                    }

                    if amount >= config.min_swap_amount {
                        let router = soroswap_router::RouterClient::new(env, &config.swap_router);
                        let path = soroban_sdk::vec![env, reward_token.clone(), storage.deposit_token.clone()];

                        // 1. Estimate expected output via router_get_amounts_out
                        let estimate_args = soroban_sdk::vec![
                            env,
                            amount.into_val(env),
                            path.clone().into_val(env),
                        ];
                        let estimate_res = env.try_invoke_contract::<soroban_sdk::Vec<i128>, soroban_sdk::Error>(
                            &config.swap_router,
                            &soroban_sdk::Symbol::new(env, "router_get_amounts_out"),
                            estimate_args,
                        );

                        if let Ok(Ok(amounts_out)) = estimate_res {
                            let expected_out = amounts_out.last().unwrap_or(0);
                            let min_amount_out = expected_out
                                .checked_mul(10000 - config.max_slippage_bps as i128)
                                .ok_or(Error::ArithmeticError)?
                                .checked_div(10000)
                                .ok_or(Error::ArithmeticError)?;

                            // Resolve the pair address
                            let pair = router.router_pair_for(&reward_token, &storage.deposit_token);

                            // Pre-authorize the router to transfer reward tokens from the vault to the pair
                            env.authorize_as_current_contract(soroban_sdk::vec![
                                env,
                                soroban_sdk::auth::InvokerContractAuthEntry::Contract(soroban_sdk::auth::SubContractInvocation {
                                    context: soroban_sdk::auth::ContractContext {
                                        contract: reward_token.clone(),
                                        fn_name: soroban_sdk::Symbol::new(env, "transfer"),
                                        args: soroban_sdk::vec![
                                            env,
                                            vault_addr.clone().into_val(env),
                                            pair.clone().into_val(env),
                                            amount.into_val(env),
                                        ],
                                    },
                                    sub_invocations: soroban_sdk::vec![env],
                                }),
                            ]);

                            let deadline = env.ledger().timestamp() + 3600;
                            let swap_args = soroban_sdk::vec![
                                env,
                                amount.into_val(env),
                                min_amount_out.into_val(env),
                                path.into_val(env),
                                vault_addr.clone().into_val(env),
                                deadline.into_val(env),
                            ];

                            let swap_res = env.try_invoke_contract::<soroban_sdk::Vec<i128>, soroban_sdk::Error>(
                                &config.swap_router,
                                &soroban_sdk::Symbol::new(env, "swap_exact_tokens_for_tokens"),
                                swap_args,
                            );

                            if let Ok(Ok(swap_out)) = swap_res {
                                let actual_out = swap_out.last().unwrap_or(0);
                                total_harvested = total_harvested
                                    .checked_add(actual_out)
                                    .ok_or(Error::ArithmeticError)?;
                            }
                        }
                    } else {
                        // Skip swap below min_swap_amount (dust guard)
                        Events::dust_accumulated(env, &alloc.adapter, &reward_token, amount);
                    }
                }
            }
        }

        // Harvested rewards go to liquid_reserve
        storage.liquid_reserve = storage
            .liquid_reserve
            .checked_add(total_harvested)
            .ok_or(Error::ArithmeticError)?;

        // Accrue management fee (mints shares to admin)
        Self::accrue_management_fee(env, &mut storage)?;

        Storage::save(env, &storage);
        Events::harvested(env, total_harvested);

        Ok(total_harvested)
    }

    /// Accrue management fee as newly minted shares to the admin.
    ///
    /// fee_shares = NAV * mgmt_fee_bps * elapsed_seconds / BPS / SECONDS_PER_YEAR
    fn accrue_management_fee(
        env: &Env,
        storage: &mut crate::common::types::VaultStorage,
    ) -> Result<(), Error> {
        let now = env.ledger().timestamp();
        let elapsed = now.saturating_sub(storage.last_fee_accrual);

        if elapsed == 0 || storage.config.management_fee_bps == 0 {
            return Ok(());
        }

        let nav = Nav::calculate(env, storage)?;
        if nav == 0 {
            storage.last_fee_accrual = now;
            return Ok(());
        }

        // fee = nav * management_fee_bps * elapsed / BPS / SECONDS_PER_YEAR
        // Using u128 intermediate to avoid overflow
        let fee_amount = (nav as u128)
            .checked_mul(storage.config.management_fee_bps as u128)
            .unwrap_or(0)
            .checked_mul(elapsed as u128)
            .unwrap_or(0)
            / BPS as u128
            / SECONDS_PER_YEAR as u128;

        if fee_amount > 0 && storage.total_shares > 0 {
            // Convert fee_amount to shares using current share_price
            let share_price = Nav::share_price(nav, storage.total_shares)?;
            let fee_shares = (fee_amount as i128)
                .checked_mul(SCALAR_7)
                .ok_or(Error::ArithmeticError)?
                .checked_div(share_price)
                .ok_or(Error::ArithmeticError)?;

            if fee_shares > 0 {
                Shares::mint(env, &storage.admin, fee_shares);
                storage.total_shares = storage
                    .total_shares
                    .checked_add(fee_shares)
                    .ok_or(Error::ArithmeticError)?;
            }
        }

        storage.last_fee_accrual = now;
        Ok(())
    }

    /// Apply performance fee when share_price exceeds the high_water_mark.
    /// Mints new shares to admin for the fee amount.
    pub fn apply_performance_fee(env: &Env) -> Result<(), Error> {
        let mut storage = Storage::load(env);

        if storage.config.performance_fee_bps == 0 || storage.total_shares == 0 {
            return Ok(());
        }

        let nav = Nav::calculate(env, &storage)?;
        let share_price = Nav::share_price(nav, storage.total_shares)?;

        if share_price <= storage.high_water_mark {
            return Ok(());
        }

        // gains = (share_price - HWM) * total_shares / SCALAR_7
        let gains = share_price
            .checked_sub(storage.high_water_mark)
            .ok_or(Error::ArithmeticError)?
            .checked_mul(storage.total_shares)
            .ok_or(Error::ArithmeticError)?
            .checked_div(SCALAR_7)
            .ok_or(Error::ArithmeticError)?;

        // perf_fee = gains * performance_fee_bps / BPS
        let perf_fee = gains
            .checked_mul(storage.config.performance_fee_bps as i128)
            .ok_or(Error::ArithmeticError)?
            .checked_div(BPS)
            .ok_or(Error::ArithmeticError)?;

        if perf_fee > 0 {
            let fee_shares = perf_fee
                .checked_mul(SCALAR_7)
                .ok_or(Error::ArithmeticError)?
                .checked_div(share_price)
                .ok_or(Error::ArithmeticError)?;

            if fee_shares > 0 {
                Shares::mint(env, &storage.admin, fee_shares);
                storage.total_shares = storage
                    .total_shares
                    .checked_add(fee_shares)
                    .ok_or(Error::ArithmeticError)?;
            }
        }

        // Update HWM to current share price (recalculate after fee dilution)
        storage.high_water_mark = share_price;
        Storage::save(env, &storage);

        Ok(())
    }
}
