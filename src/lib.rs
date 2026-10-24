//! A toy payments engine: applies deposits, withdrawals, disputes, resolves and chargebacks to
//! client accounts.

mod engine;
mod money;
mod transaction;

pub use engine::{Account, DepositState, Engine, TxError};
pub use money::{Amount, AmountError, Balances, PRECISION};
pub use transaction::{ClientId, Transaction, TxId, TxKind};
