//! The engine: applies transactions to client accounts and tracks the dispute lifecycle of
//! every deposit.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::fmt;

use rust_decimal::Decimal;

use crate::money::{Amount, Balances};
use crate::transaction::{ClientId, Transaction, TxId, TxKind};

/// All client accounts, keyed by client id. Iterates in client-id order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Engine {
    accounts: BTreeMap<ClientId, Account>,
}

impl Engine {
    /// Applies one transaction. Every transaction creates its client's account if it does not
    /// exist yet, even when the transaction itself is then rejected.
    ///
    /// # Errors
    ///
    /// Returns a [`TxError`] when the transaction is rejected. A rejected transaction leaves the
    /// account unchanged; callers report it and carry on with the next one.
    pub fn apply(&mut self, transaction: Transaction) -> Result<(), TxError> {
        let account = self.accounts.entry(transaction.client).or_default();
        let tx = transaction.tx;
        match transaction.kind {
            TxKind::Deposit(amount) => account.deposit(tx, amount),
            TxKind::Withdrawal(amount) => account.withdraw(amount),
            TxKind::Dispute => account.transition(
                tx,
                DepositState::Settled,
                DepositState::Disputed,
                Balances::hold,
            ),
            TxKind::Resolve => account.transition(
                tx,
                DepositState::Disputed,
                DepositState::Settled,
                Balances::release,
            ),
            TxKind::Chargeback => {
                account.transition(
                    tx,
                    DepositState::Disputed,
                    DepositState::ChargedBack,
                    Balances::charge_back,
                )?;
                account.status = AccountStatus::Locked;
                Ok(())
            }
        }
    }

    /// Every account, in client-id order.
    pub fn accounts(&self) -> impl Iterator<Item = (ClientId, &Account)> {
        self.accounts
            .iter()
            .map(|(&client, account)| (client, account))
    }
}

/// One client's funds, lock status, and the deposits that can still be disputed.
///
/// Deposits are stored per account, so a dispute can only ever reach its own client's deposits:
/// a dispute naming another client's transaction finds nothing and is rejected as unknown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Account {
    balances: Balances,
    status: AccountStatus,
    deposits: HashMap<TxId, Deposit>,
}

impl Account {
    /// Available, held and total funds.
    #[must_use]
    pub fn balances(&self) -> Balances {
        self.balances
    }

    /// Whether a chargeback has frozen this account.
    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.status == AccountStatus::Locked
    }

    fn deposit(&mut self, tx: TxId, amount: Amount) -> Result<(), TxError> {
        self.ensure_active()?;
        let Entry::Vacant(slot) = self.deposits.entry(tx) else {
            return Err(TxError::DuplicateTx);
        };
        self.balances = self.balances.deposit(amount).ok_or(TxError::Overflow)?;
        slot.insert(Deposit {
            amount,
            state: DepositState::Settled,
        });
        Ok(())
    }

    /// Withdrawals are not stored: only deposits can be disputed.
    fn withdraw(&mut self, amount: Amount) -> Result<(), TxError> {
        self.ensure_active()?;
        if self.balances.available() < amount.value() {
            return Err(TxError::InsufficientFunds {
                available: self.balances.available(),
                requested: amount.value(),
            });
        }
        self.balances = self.balances.withdraw(amount).ok_or(TxError::Overflow)?;
        Ok(())
    }

    /// Moves deposit `tx` from state `from` to state `to`, applying `operation` to the balances.
    /// Used by dispute, resolve and chargeback, which differ only in these three arguments. Works
    /// on locked accounts: disputes on earlier deposits keep arriving after a freeze.
    fn transition(
        &mut self,
        tx: TxId,
        from: DepositState,
        to: DepositState,
        operation: fn(Balances, Amount) -> Option<Balances>,
    ) -> Result<(), TxError> {
        let deposit = self.deposits.get_mut(&tx).ok_or(TxError::UnknownTx)?;
        if deposit.state != from {
            return Err(TxError::WrongState {
                expected: from,
                actual: deposit.state,
            });
        }
        self.balances = operation(self.balances, deposit.amount).ok_or(TxError::Overflow)?;
        deposit.state = to;
        Ok(())
    }

    fn ensure_active(&self) -> Result<(), TxError> {
        match self.status {
            AccountStatus::Active => Ok(()),
            AccountStatus::Locked => Err(TxError::AccountLocked),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum AccountStatus {
    #[default]
    Active,
    Locked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Deposit {
    amount: Amount,
    state: DepositState,
}

/// Where a deposit is in the dispute lifecycle:
/// `Settled → Disputed → Settled` (resolved, may be disputed again) or `→ ChargedBack` (final).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepositState {
    Settled,
    Disputed,
    ChargedBack,
}

impl fmt::Display for DepositState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Settled => "settled",
            Self::Disputed => "disputed",
            Self::ChargedBack => "charged back",
        })
    }
}

/// Why a transaction was rejected. Every variant is an expected outcome of partner data, not a
/// bug in the engine.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TxError {
    #[error("transaction id already used by an earlier deposit")]
    DuplicateTx,
    #[error("account is locked")]
    AccountLocked,
    #[error("insufficient funds: {available} available, {requested} requested")]
    InsufficientFunds {
        available: Decimal,
        requested: Decimal,
    },
    #[error("no deposit with this id for this client")]
    UnknownTx,
    #[error("deposit is {actual}, must be {expected}")]
    WrongState {
        expected: DepositState,
        actual: DepositState,
    },
    #[error("arithmetic overflow")]
    Overflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx(client: u16, id: u32, kind: TxKind) -> Transaction {
        Transaction {
            client: ClientId(client),
            tx: TxId(id),
            kind,
        }
    }

    fn deposit(client: u16, id: u32, amount: &str) -> Transaction {
        tx(client, id, TxKind::Deposit(amount.parse().unwrap()))
    }

    fn withdrawal(client: u16, id: u32, amount: &str) -> Transaction {
        tx(client, id, TxKind::Withdrawal(amount.parse().unwrap()))
    }

    fn dispute(client: u16, id: u32) -> Transaction {
        tx(client, id, TxKind::Dispute)
    }

    fn resolve(client: u16, id: u32) -> Transaction {
        tx(client, id, TxKind::Resolve)
    }

    fn chargeback(client: u16, id: u32) -> Transaction {
        tx(client, id, TxKind::Chargeback)
    }

    /// An engine after applying `transactions`, each of which must succeed.
    fn engine(transactions: &[Transaction]) -> Engine {
        let mut engine = Engine::default();
        for &t in transactions {
            engine.apply(t).unwrap();
        }
        engine
    }

    fn account(engine: &Engine, client: u16) -> &Account {
        engine.accounts.get(&ClientId(client)).unwrap()
    }

    /// Asserts an account's (available, held, total, locked).
    fn assert_account(engine: &Engine, client: u16, expected: (&str, &str, &str, bool)) {
        let account = account(engine, client);
        let b = account.balances();
        let dec = |raw: &str| Decimal::from_str_exact(raw).unwrap();
        assert_eq!(
            (b.available(), b.held(), b.total(), account.is_locked()),
            (
                dec(expected.0),
                dec(expected.1),
                dec(expected.2),
                expected.3
            )
        );
    }

    /// Asserts `transaction` is rejected with `error` and changes nothing.
    fn assert_rejected(engine: &mut Engine, transaction: Transaction, error: TxError) {
        let before = engine.clone();
        assert_eq!(engine.apply(transaction), Err(error));
        assert_eq!(*engine, before);
    }

    #[test]
    fn deposit_credits_available_and_total() {
        let e = engine(&[deposit(1, 1, "1.5"), deposit(1, 2, "2.25")]);
        assert_account(&e, 1, ("3.75", "0", "3.75", false));
    }

    #[test]
    fn withdrawal_debits_available_and_total() {
        let e = engine(&[deposit(1, 1, "3"), withdrawal(1, 2, "1.5")]);
        assert_account(&e, 1, ("1.5", "0", "1.5", false));
    }

    #[test]
    fn withdrawal_of_exact_balance_succeeds() {
        let e = engine(&[deposit(1, 1, "2"), withdrawal(1, 2, "2")]);
        assert_account(&e, 1, ("0", "0", "0", false));
    }

    #[test]
    fn withdrawal_exceeding_available_is_rejected() {
        let mut e = engine(&[deposit(1, 1, "1")]);
        assert_rejected(
            &mut e,
            withdrawal(1, 2, "1.0001"),
            TxError::InsufficientFunds {
                available: Decimal::ONE,
                requested: "1.0001".parse().unwrap(),
            },
        );
    }

    #[test]
    fn withdrawal_counts_only_available_funds() {
        let mut e = engine(&[deposit(1, 1, "2"), deposit(1, 2, "3"), dispute(1, 2)]);
        assert!(matches!(
            e.apply(withdrawal(1, 3, "2.5")),
            Err(TxError::InsufficientFunds { .. })
        ));
    }

    #[test]
    fn accounts_are_independent() {
        let e = engine(&[
            deposit(1, 1, "1"),
            deposit(2, 2, "2"),
            withdrawal(2, 3, "0.5"),
        ]);
        assert_account(&e, 1, ("1", "0", "1", false));
        assert_account(&e, 2, ("1.5", "0", "1.5", false));
    }

    #[test]
    fn duplicate_deposit_id_is_rejected() {
        let mut e = engine(&[deposit(1, 1, "1")]);
        assert_rejected(&mut e, deposit(1, 1, "5"), TxError::DuplicateTx);
    }

    #[test]
    fn rejected_deposit_is_not_recorded() {
        let mut e = engine(&[deposit(1, 1, "1"), dispute(1, 1), chargeback(1, 1)]);
        assert_rejected(&mut e, deposit(1, 2, "5"), TxError::AccountLocked);
        assert_rejected(&mut e, dispute(1, 2), TxError::UnknownTx);
    }

    #[test]
    fn dispute_moves_funds_to_held() {
        let e = engine(&[deposit(1, 1, "3"), deposit(1, 2, "1"), dispute(1, 1)]);
        assert_account(&e, 1, ("1", "3", "4", false));
    }

    #[test]
    fn dispute_may_drive_available_negative() {
        // Deposit, withdraw it, then dispute the deposit: the fraud case. The hold still applies.
        let e = engine(&[deposit(1, 1, "10"), withdrawal(1, 2, "8"), dispute(1, 1)]);
        assert_account(&e, 1, ("-8", "10", "2", false));
    }

    #[test]
    fn dispute_of_unknown_tx_is_rejected() {
        let mut e = engine(&[deposit(1, 1, "1")]);
        assert_rejected(&mut e, dispute(1, 99), TxError::UnknownTx);
    }

    #[test]
    fn dispute_of_other_clients_tx_is_rejected() {
        let mut e = engine(&[deposit(1, 1, "1"), deposit(2, 2, "1")]);
        assert_rejected(&mut e, dispute(2, 1), TxError::UnknownTx);
    }

    #[test]
    fn dispute_of_withdrawal_is_rejected() {
        let mut e = engine(&[deposit(1, 1, "3"), withdrawal(1, 2, "1")]);
        assert_rejected(&mut e, dispute(1, 2), TxError::UnknownTx);
    }

    #[test]
    fn disputed_deposit_cannot_be_disputed_twice() {
        let mut e = engine(&[deposit(1, 1, "1"), dispute(1, 1)]);
        assert_rejected(
            &mut e,
            dispute(1, 1),
            TxError::WrongState {
                expected: DepositState::Settled,
                actual: DepositState::Disputed,
            },
        );
    }

    #[test]
    fn resolve_returns_held_funds() {
        let e = engine(&[deposit(1, 1, "3"), dispute(1, 1), resolve(1, 1)]);
        assert_account(&e, 1, ("3", "0", "3", false));
    }

    #[test]
    fn resolve_without_dispute_is_rejected() {
        let mut e = engine(&[deposit(1, 1, "1")]);
        assert_rejected(
            &mut e,
            resolve(1, 1),
            TxError::WrongState {
                expected: DepositState::Disputed,
                actual: DepositState::Settled,
            },
        );
    }

    #[test]
    fn resolve_of_unknown_tx_is_rejected() {
        let mut e = engine(&[deposit(1, 1, "1")]);
        assert_rejected(&mut e, resolve(1, 2), TxError::UnknownTx);
    }

    #[test]
    fn resolved_deposit_can_be_disputed_again() {
        let e = engine(&[
            deposit(1, 1, "2"),
            dispute(1, 1),
            resolve(1, 1),
            dispute(1, 1),
        ]);
        assert_account(&e, 1, ("0", "2", "2", false));
    }

    #[test]
    fn chargeback_removes_held_funds_and_locks_account() {
        let e = engine(&[
            deposit(1, 1, "3"),
            deposit(1, 2, "1"),
            dispute(1, 1),
            chargeback(1, 1),
        ]);
        assert_account(&e, 1, ("1", "0", "1", true));
    }

    #[test]
    fn chargeback_after_withdrawal_leaves_negative_total() {
        let e = engine(&[
            deposit(1, 1, "10"),
            withdrawal(1, 2, "8"),
            dispute(1, 1),
            chargeback(1, 1),
        ]);
        assert_account(&e, 1, ("-8", "0", "-8", true));
    }

    #[test]
    fn chargeback_without_dispute_is_rejected() {
        let mut e = engine(&[deposit(1, 1, "1")]);
        assert_rejected(
            &mut e,
            chargeback(1, 1),
            TxError::WrongState {
                expected: DepositState::Disputed,
                actual: DepositState::Settled,
            },
        );
    }

    #[test]
    fn charged_back_deposit_is_final() {
        let mut e = engine(&[deposit(1, 1, "1"), dispute(1, 1), chargeback(1, 1)]);
        for t in [dispute(1, 1), resolve(1, 1), chargeback(1, 1)] {
            assert!(matches!(
                e.apply(t),
                Err(TxError::WrongState {
                    actual: DepositState::ChargedBack,
                    ..
                })
            ));
        }
        assert_account(&e, 1, ("0", "0", "0", true));
    }

    #[test]
    fn wrong_state_message_names_both_states() {
        let mut e = engine(&[deposit(1, 1, "1"), dispute(1, 1), chargeback(1, 1)]);
        let messages: Vec<_> = [dispute(1, 1), resolve(1, 1)]
            .into_iter()
            .map(|t| e.apply(t).unwrap_err().to_string())
            .collect();
        assert_eq!(
            messages,
            [
                "deposit is charged back, must be settled",
                "deposit is charged back, must be disputed"
            ]
        );
    }

    #[test]
    fn locked_account_rejects_deposits_and_withdrawals() {
        let mut e = engine(&[
            deposit(1, 1, "5"),
            deposit(1, 2, "1"),
            dispute(1, 2),
            chargeback(1, 2),
        ]);
        assert_rejected(&mut e, deposit(1, 3, "1"), TxError::AccountLocked);
        assert_rejected(&mut e, withdrawal(1, 4, "1"), TxError::AccountLocked);
    }

    #[test]
    fn locked_account_accepts_dispute_resolve_chargeback() {
        let mut e = engine(&[
            deposit(1, 1, "5"),
            deposit(1, 2, "1"),
            deposit(1, 3, "2"),
            dispute(1, 3),
            chargeback(1, 3),
        ]);
        for t in [
            dispute(1, 1),
            resolve(1, 1),
            dispute(1, 2),
            chargeback(1, 2),
        ] {
            e.apply(t).unwrap();
        }
        assert_account(&e, 1, ("5", "0", "5", true));
    }

    #[test]
    fn any_parsed_transaction_creates_the_account() {
        let mut e = Engine::default();
        assert!(e.apply(withdrawal(7, 1, "1")).is_err());
        assert!(e.apply(dispute(8, 1)).is_err());
        let clients: Vec<_> = e.accounts().map(|(client, _)| client).collect();
        assert_eq!(clients, [ClientId(7), ClientId(8)]);
        assert_account(&e, 7, ("0", "0", "0", false));
    }

    #[test]
    fn accounts_iterate_in_client_order() {
        let e = engine(&[deposit(3, 1, "1"), deposit(1, 2, "1"), deposit(2, 3, "1")]);
        let clients: Vec<_> = e.accounts().map(|(client, _)| client.0).collect();
        assert_eq!(clients, [1, 2, 3]);
    }
}
