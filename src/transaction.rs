//! A transaction as the engine sees it: parsed, validated, and shaped so that only deposits and
//! withdrawals carry an amount.

use std::fmt;

use crate::money::Amount;

/// A client identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientId(pub u16);

/// A globally unique transaction identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TxId(pub u32);

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for TxId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// One instruction against a client's account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transaction {
    pub client: ClientId,
    /// For deposits and withdrawals, this transaction's own id. For disputes, resolves and
    /// chargebacks, the id of the deposit they refer to.
    pub tx: TxId,
    pub kind: TxKind,
}

/// What a transaction does. Only deposits and withdrawals carry an amount; the others refer to
/// an earlier deposit by id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxKind {
    Deposit(Amount),
    Withdrawal(Amount),
    Dispute,
    Resolve,
    Chargeback,
}
