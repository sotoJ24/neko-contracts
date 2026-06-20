use soroban_sdk::{Address, Env, Symbol, contractevent};

// ============================================================================
// Vault events
// ============================================================================

#[contractevent]
pub struct DepositEvent {
    pub from: Address,
    pub amount: i128,
    pub shares: i128,
}

#[contractevent]
pub struct WithdrawEvent {
    pub to: Address,
    pub amount: i128,
    pub shares: i128,
}

#[contractevent]
pub struct RebalancedEvent {
    pub nav: i128,
}

#[contractevent]
pub struct HarvestedEvent {
    pub total: i128,
}

#[contractevent]
pub struct ProtocolAddedEvent {
    pub id: Symbol,
    pub adapter: Address,
}

#[contractevent]
pub struct ProtocolRemovedEvent {
    pub id: Symbol,
}

#[contractevent]
pub struct DustAccumulatedEvent {
    pub adapter: Address,
    pub token: Address,
    pub amount: i128,
}

// ============================================================================
// SEP-41 token events (matching neko-token pattern)
// ============================================================================

#[contractevent]
pub struct TransferEvent {
    #[topic]
    pub from: Address,
    #[topic]
    pub to: Address,
    pub amount: i128,
}

#[contractevent]
pub struct ApproveEvent {
    #[topic]
    pub from: Address,
    #[topic]
    pub spender: Address,
    pub amount: i128,
    pub live_until_ledger: u32,
}

#[contractevent]
pub struct BurnEvent {
    #[topic]
    pub from: Address,
    pub amount: i128,
}

// ============================================================================
// Emission helpers
// ============================================================================

pub struct Events;

impl Events {
    pub fn deposit(env: &Env, from: &Address, amount: i128, shares: i128) {
        DepositEvent {
            from: from.clone(),
            amount,
            shares,
        }
        .publish(env);
    }

    pub fn withdraw(env: &Env, to: &Address, amount: i128, shares: i128) {
        WithdrawEvent {
            to: to.clone(),
            amount,
            shares,
        }
        .publish(env);
    }

    pub fn rebalanced(env: &Env, nav: i128) {
        RebalancedEvent { nav }.publish(env);
    }

    pub fn harvested(env: &Env, total: i128) {
        HarvestedEvent { total }.publish(env);
    }

    pub fn protocol_added(env: &Env, id: &Symbol, adapter: &Address) {
        ProtocolAddedEvent {
            id: id.clone(),
            adapter: adapter.clone(),
        }
        .publish(env);
    }

    pub fn protocol_removed(env: &Env, id: &Symbol) {
        ProtocolRemovedEvent { id: id.clone() }.publish(env);
    }

    pub fn dust_accumulated(env: &Env, adapter: &Address, token: &Address, amount: i128) {
        DustAccumulatedEvent {
            adapter: adapter.clone(),
            token: token.clone(),
            amount,
        }
        .publish(env);
    }

    pub fn transfer(env: &Env, from: &Address, to: &Address, amount: i128) {
        TransferEvent {
            from: from.clone(),
            to: to.clone(),
            amount,
        }
        .publish(env);
    }

    pub fn approve(
        env: &Env,
        from: &Address,
        spender: &Address,
        amount: i128,
        live_until_ledger: u32,
    ) {
        ApproveEvent {
            from: from.clone(),
            spender: spender.clone(),
            amount,
            live_until_ledger,
        }
        .publish(env);
    }

    pub fn burn(env: &Env, from: &Address, amount: i128) {
        BurnEvent {
            from: from.clone(),
            amount,
        }
        .publish(env);
    }
}
