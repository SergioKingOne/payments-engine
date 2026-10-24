//! A toy payments engine: applies deposits, withdrawals, disputes, resolves and chargebacks to
//! client accounts.

mod money;
mod transaction;

pub use money::{Amount, AmountError, Balances, PRECISION};
pub use transaction::{ClientId, Transaction, TxId, TxKind};
