//! A toy payments engine: applies deposits, withdrawals, disputes, resolves and chargebacks to
//! client accounts.
//!
//! [`run`] streams transactions from CSV and writes the final account balances as CSV. The
//! [`Engine`] underneath is I/O-free and can be driven directly.

mod csv_io;
mod engine;
mod money;
mod transaction;

use std::io::{self, Read, Write};

pub use engine::{Account, DepositState, Engine, TxError};
pub use money::{Amount, AmountError, Balances, PRECISION};
pub use transaction::{ClientId, Transaction, TxId, TxKind};

use csv_io::{ParsedRow, TransactionReader};

/// Reads transactions as CSV from `input`, applies them in order, and writes the final account
/// balances as CSV to `output`.
///
/// A row that cannot be parsed, or that the engine rejects, is skipped and reported as one line
/// on `diagnostics` (`row N: reason`, counting data rows from 1, so not the header or blank
/// lines). Processing always continues with the next row.
///
/// # Errors
///
/// Returns an error for I/O failures (reading `input`, writing `output` or `diagnostics`), and
/// for a header that lacks the `type`, `client` or `tx` column. Nothing is written to `output`
/// in that case.
pub fn run<R: Read, W: Write, E: Write>(input: R, output: W, mut diagnostics: E) -> io::Result<()> {
    let mut engine = Engine::default();
    for row in TransactionReader::new(input)? {
        let ParsedRow { row, transaction } = row?;
        let rejection = match transaction {
            Ok(transaction) => engine.apply(transaction).err().map(|error| {
                format!(
                    "client {}, tx {}: {error}",
                    transaction.client, transaction.tx
                )
            }),
            Err(error) => Some(error.to_string()),
        };
        if let Some(reason) = rejection {
            match row {
                Some(row) => writeln!(diagnostics, "row {row}: {reason}")?,
                None => writeln!(diagnostics, "{reason}")?,
            }
        }
    }
    csv_io::write_accounts(&engine, output)?;
    diagnostics.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the engine on `input`, returning (stdout, stderr).
    fn run_on(input: &str) -> (String, String) {
        let (mut output, mut diagnostics) = (Vec::new(), Vec::new());
        run(input.as_bytes(), &mut output, &mut diagnostics).unwrap();
        (
            String::from_utf8(output).unwrap(),
            String::from_utf8(diagnostics).unwrap(),
        )
    }

    #[test]
    fn spec_sample_produces_expected_accounts() {
        let input = "type, client, tx, amount\n\
                     deposit, 1, 1, 1.0\n\
                     deposit, 2, 2, 2.0\n\
                     deposit, 1, 3, 2.0\n\
                     withdrawal, 1, 4, 1.5\n\
                     withdrawal, 2, 5, 3.0\n";
        let (output, diagnostics) = run_on(input);
        assert_eq!(
            output,
            "client,available,held,total,locked\n\
             1,1.5000,0.0000,1.5000,false\n\
             2,2.0000,0.0000,2.0000,false\n"
        );
        assert_eq!(
            diagnostics,
            "row 5: client 2, tx 5: insufficient funds: 2 available, 3 requested\n"
        );
    }

    #[test]
    fn rejected_rows_are_reported_and_skipped() {
        let input = "type,client,tx,amount\n\
                     deposit,1,1,10\n\
                     deposit,1,2,abc\n\
                     dispute,1,99,\n\
                     withdrawal,1,3,4\n";
        let (output, diagnostics) = run_on(input);
        assert_eq!(
            output,
            "client,available,held,total,locked\n1,6.0000,0.0000,6.0000,false\n"
        );
        assert_eq!(
            diagnostics,
            "row 2: invalid amount: \"abc\" is not a decimal number\n\
             row 3: client 1, tx 99: no deposit with this id for this client\n"
        );
    }

    #[test]
    fn dispute_lifecycle_end_to_end() {
        let input = "type,client,tx,amount\n\
                     deposit,1,1,10\n\
                     withdrawal,1,2,8\n\
                     dispute,1,1,\n\
                     chargeback,1,1,\n\
                     deposit,1,3,100\n\
                     deposit,2,4,1.2345\n";
        let (output, diagnostics) = run_on(input);
        assert_eq!(
            output,
            "client,available,held,total,locked\n\
             1,-8.0000,0.0000,-8.0000,true\n\
             2,1.2345,0.0000,1.2345,false\n"
        );
        assert_eq!(diagnostics, "row 5: client 1, tx 3: account is locked\n");
    }

    #[test]
    fn empty_input_writes_header_only() {
        assert_eq!(
            run_on(""),
            ("client,available,held,total,locked\n".into(), String::new())
        );
    }

    #[test]
    fn missing_header_writes_nothing_to_output() {
        let (mut output, mut diagnostics) = (Vec::new(), Vec::new());
        let error = run(
            "deposit,1,1,1.0\n".as_bytes(),
            &mut output,
            &mut diagnostics,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!((output, diagnostics), (Vec::new(), Vec::new()));
    }

    #[test]
    fn rows_are_sorted_by_client() {
        let input = "type,client,tx,amount\ndeposit,3,1,1\ndeposit,1,2,1\ndeposit,2,3,1\n";
        let (output, _) = run_on(input);
        let clients: Vec<_> = output
            .lines()
            .skip(1)
            .filter_map(|line| line.split(',').next())
            .collect();
        assert_eq!(clients, ["1", "2", "3"]);
    }
}
