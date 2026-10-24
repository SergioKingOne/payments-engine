//! Properties that must hold for any sequence of transactions, checked on random sequences.
//!
//! Client and transaction ids are drawn from small ranges, so duplicates, cross-client references
//! and repeated disputes on the same deposit come up often.
//!
//! The ledger property does not re-implement the engine's rules. It replays only the transactions
//! the engine accepted, using plain bookkeeping, and checks the engine's balances agree.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail by panicking; clippy exempts only #[test] bodies"
)]

use std::collections::{HashMap, HashSet};

use payments_engine::{Account, Amount, ClientId, Engine, Transaction, TxError, TxId, TxKind, run};
use proptest::prelude::*;
use rust_decimal::Decimal;

fn amount() -> impl Strategy<Value = Amount> {
    // 0.0001 to 1000.0000, at the full four decimal places.
    (1_i64..=10_000_000).prop_map(|units| Amount::new(Decimal::new(units, 4)).unwrap())
}

fn transaction() -> impl Strategy<Value = Transaction> {
    let kind = prop_oneof![
        3 => amount().prop_map(TxKind::Deposit),
        2 => amount().prop_map(TxKind::Withdrawal),
        2 => Just(TxKind::Dispute),
        1 => Just(TxKind::Resolve),
        1 => Just(TxKind::Chargeback),
    ];
    (0_u16..4, 0_u32..16, kind).prop_map(|(client, tx, kind)| Transaction {
        client: ClientId(client),
        tx: TxId(tx),
        kind,
    })
}

/// What an outside observer can work out about one client from the accepted transactions.
#[derive(Default)]
struct Ledger {
    total: Decimal,
    held: Decimal,
    locked: bool,
    disputed_ever: bool,
}

fn snapshot(engine: &Engine) -> HashMap<ClientId, Account> {
    engine
        .accounts()
        .map(|(client, account)| (client, account.clone()))
        .collect()
}

proptest! {
    #[test]
    fn balances_follow_the_accepted_transactions(
        transactions in prop::collection::vec(transaction(), 0..64)
    ) {
        let mut engine = Engine::default();
        let mut ledgers: HashMap<ClientId, Ledger> = HashMap::new();
        let mut deposits: HashMap<(ClientId, TxId), Decimal> = HashMap::new();

        for t in transactions {
            // A rejected transaction still creates its client's (empty) account, by design.
            let mut before = snapshot(&engine);
            before.entry(t.client).or_default();
            let result = engine.apply(t);
            let ledger = ledgers.entry(t.client).or_default();

            let Ok(()) = result else {
                prop_assert_eq!(snapshot(&engine), before, "rejected {:?} changed the engine", t);
                if ledger.locked && matches!(t.kind, TxKind::Deposit(_) | TxKind::Withdrawal(_)) {
                    prop_assert_eq!(result, Err(TxError::AccountLocked));
                }
                continue;
            };
            let disputed = deposits.get(&(t.client, t.tx)).copied();
            match t.kind {
                TxKind::Deposit(amount) => {
                    prop_assert!(!ledger.locked, "deposit accepted on a locked account");
                    deposits.insert((t.client, t.tx), amount.value());
                    ledger.total = ledger.total.checked_add(amount.value()).unwrap();
                }
                TxKind::Withdrawal(amount) => {
                    prop_assert!(!ledger.locked, "withdrawal accepted on a locked account");
                    ledger.total = ledger.total.checked_sub(amount.value()).unwrap();
                }
                TxKind::Dispute => {
                    let amount = disputed.unwrap();
                    ledger.held = ledger.held.checked_add(amount).unwrap();
                    ledger.disputed_ever = true;
                }
                TxKind::Resolve => {
                    ledger.held = ledger.held.checked_sub(disputed.unwrap()).unwrap();
                }
                TxKind::Chargeback => {
                    let amount = disputed.unwrap();
                    ledger.held = ledger.held.checked_sub(amount).unwrap();
                    ledger.total = ledger.total.checked_sub(amount).unwrap();
                    ledger.locked = true;
                }
            }
        }

        let clients: HashSet<_> = engine.accounts().map(|(client, _)| client).collect();
        prop_assert_eq!(clients, ledgers.keys().copied().collect::<HashSet<_>>());
        for (client, account) in engine.accounts() {
            let ledger = &ledgers[&client];
            let b = account.balances();
            prop_assert_eq!(b.total(), ledger.total, "total of {}", client);
            prop_assert_eq!(b.held(), ledger.held, "held of {}", client);
            prop_assert_eq!(b.available(), ledger.total.checked_sub(ledger.held).unwrap());
            prop_assert_eq!(b.total(), b.available().checked_add(b.held()).unwrap());
            prop_assert!(b.held() >= Decimal::ZERO, "negative held for {}", client);
            prop_assert_eq!(account.is_locked(), ledger.locked, "lock of {}", client);
            if !ledger.disputed_ever {
                prop_assert!(b.available() >= Decimal::ZERO, "overdrawn {}", client);
            }
        }
    }
}

/// Values a partner might send by mistake, in any column.
const JUNK: &[&str] = &[
    "",
    "-1",
    "x",
    "1e3",
    "1.23456",
    "99999999999",
    " ",
    "0",
    "1.0",
];

/// One CSV field: mostly valid values, sometimes junk, with random padding.
fn field(valid: &'static [&'static str]) -> impl Strategy<Value = String> {
    let pick = prop_oneof![
        4 => prop::sample::select(valid),
        1 => prop::sample::select(JUNK),
    ];
    (pick, "[ ]{0,2}", "[ ]{0,2}").prop_map(|(value, pre, post)| format!("{pre}{value}{post}"))
}

fn csv_row() -> impl Strategy<Value = String> {
    (
        field(&[
            "deposit",
            "withdrawal",
            "dispute",
            "resolve",
            "chargeback",
            "refund",
        ]),
        field(&["1", "2", "3", "65535"]),
        field(&["1", "2", "3", "4", "4294967295"]),
        prop::option::of(field(&["1", "0.5", "2.75", "0.0001", "100"])),
    )
        .prop_map(|(kind, client, tx, amount)| match amount {
            Some(amount) => format!("{kind},{client},{tx},{amount}"),
            None => format!("{kind},{client},{tx}"),
        })
}

proptest! {
    #[test]
    fn any_rows_produce_well_formed_output(rows in prop::collection::vec(csv_row(), 0..32)) {
        let input = format!("type,client,tx,amount\n{}\n", rows.join("\n"));
        let mut output = Vec::new();
        run(input.as_bytes(), &mut output, std::io::sink()).unwrap();

        let output = String::from_utf8(output).unwrap();
        let mut lines = output.lines();
        prop_assert_eq!(lines.next(), Some("client,available,held,total,locked"));
        for line in lines {
            let fields: Vec<_> = line.split(',').collect();
            prop_assert_eq!(fields.len(), 5, "{}", line);
            let money: Vec<Decimal> = fields[1..4]
                .iter()
                .map(|raw| Decimal::from_str_exact(raw).unwrap())
                .collect();
            prop_assert!(money.iter().all(|value| value.scale() == 4), "{}", line);
            prop_assert_eq!(money[2], money[0].checked_add(money[1]).unwrap(), "{}", line);
            prop_assert!(fields[4] == "true" || fields[4] == "false", "{}", line);
        }
    }
}
