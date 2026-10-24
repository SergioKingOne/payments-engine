//! CSV input and output. This module only translates between CSV and domain types; it applies
//! no business rules.

use std::io::{self, Read, Write};

use csv::{ReaderBuilder, StringRecord, Trim, WriterBuilder};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::engine::Engine;
use crate::money::{Amount, AmountError, PRECISION};
use crate::transaction::{ClientId, Transaction, TxId, TxKind};

/// One input row as written. `amount` stays raw text so it is parsed exactly by [`Amount`], and
/// so junk in the amount column of a dispute, resolve or chargeback cannot reject the row.
#[derive(Deserialize)]
struct Row<'a> {
    #[serde(rename = "type")]
    kind: RowKind,
    client: u16,
    tx: u32,
    #[serde(borrow)]
    amount: Option<&'a str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum RowKind {
    Deposit,
    Withdrawal,
    Dispute,
    Resolve,
    Chargeback,
}

impl TryFrom<Row<'_>> for Transaction {
    type Error = RowError;

    fn try_from(row: Row<'_>) -> Result<Self, RowError> {
        let amount = || -> Result<Amount, RowError> {
            Ok(row.amount.ok_or(RowError::MissingAmount)?.parse()?)
        };
        let kind = match row.kind {
            RowKind::Deposit => TxKind::Deposit(amount()?),
            RowKind::Withdrawal => TxKind::Withdrawal(amount()?),
            RowKind::Dispute => TxKind::Dispute,
            RowKind::Resolve => TxKind::Resolve,
            RowKind::Chargeback => TxKind::Chargeback,
        };
        Ok(Self {
            client: ClientId(row.client),
            tx: TxId(row.tx),
            kind,
        })
    }
}

/// Why an input row could not be turned into a [`Transaction`].
#[derive(Debug, thiserror::Error)]
pub(crate) enum RowError {
    #[error("malformed row: {0}")]
    Malformed(String),
    #[error("missing amount")]
    MissingAmount,
    #[error("invalid amount: {0}")]
    InvalidAmount(#[from] AmountError),
}

/// One input row: its position, and the transaction it holds or why it holds none.
pub(crate) struct ParsedRow {
    /// 1-based count of data rows: the header and blank lines are not counted. The csv crate's
    /// line numbers skip blank lines too, so they cannot be trusted to point at the right line.
    pub(crate) row: Option<u64>,
    pub(crate) transaction: Result<Transaction, RowError>,
}

/// Streams transactions from CSV, one row at a time, reusing a single record buffer.
///
/// Leading and trailing whitespace is trimmed from every field and header, and rows may omit
/// the trailing amount column. A bad row is yielded as a [`RowError`] and reading continues;
/// only I/O errors end the stream. A header without the required columns is rejected up front,
/// since every row after it would fail.
pub(crate) struct TransactionReader<R> {
    reader: csv::Reader<R>,
    headers: StringRecord,
    record: StringRecord,
}

impl<R: Read> TransactionReader<R> {
    pub(crate) fn new(input: R) -> io::Result<Self> {
        let mut reader = ReaderBuilder::new()
            .trim(Trim::All)
            .flexible(true)
            .from_reader(input);
        let headers = reader.headers()?.clone();
        // An empty input has no header and no rows; that is a valid, empty batch.
        if !headers.is_empty() {
            let missing: Vec<_> = REQUIRED_COLUMNS
                .into_iter()
                .filter(|column| !headers.iter().any(|header| header == *column))
                .collect();
            if !missing.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "header is missing column(s) {}; expected `type, client, tx, amount`",
                        missing.join(", ")
                    ),
                ));
            }
        }
        Ok(Self {
            reader,
            headers,
            record: StringRecord::new(),
        })
    }
}

/// `amount` is optional: dispute, resolve and chargeback rows may omit the column entirely.
const REQUIRED_COLUMNS: [&str; 3] = ["type", "client", "tx"];

impl<R: Read> Iterator for TransactionReader<R> {
    type Item = io::Result<ParsedRow>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.reader.read_record(&mut self.record) {
            Ok(false) => None,
            Ok(true) => {
                let transaction = self
                    .record
                    .deserialize::<Row<'_>>(Some(&self.headers))
                    .map_err(|error| RowError::Malformed(describe(&error, &self.headers)))
                    .and_then(Transaction::try_from);
                Some(Ok(ParsedRow {
                    row: self.record.position().map(csv::Position::record),
                    transaction,
                }))
            }
            Err(error) if error.is_io_error() => Some(Err(error.into())),
            // The bad record has been consumed, so the next call moves on to the next row.
            Err(error) => Some(Ok(ParsedRow {
                row: error.position().map(csv::Position::record),
                transaction: Err(RowError::Malformed(describe(&error, &self.headers))),
            })),
        }
    }
}

/// What is wrong with a row, naming the column, without csv's own position (the caller already
/// reports the row).
fn describe(error: &csv::Error, headers: &StringRecord) -> String {
    let column = |index: Option<usize>| index.and_then(|index| headers.get(index));
    match error.kind() {
        csv::ErrorKind::Deserialize { err, .. } => {
            let index = err.field().and_then(|index| usize::try_from(index).ok());
            match column(index) {
                Some(name) => format!("{name}: {}", err.kind()),
                None => err.kind().to_string(),
            }
        }
        csv::ErrorKind::Utf8 { err, .. } => match column(Some(err.field())) {
            Some(name) => format!("{name}: invalid UTF-8"),
            None => "invalid UTF-8".into(),
        },
        _ => error.to_string(),
    }
}

#[derive(Serialize)]
struct OutputRow {
    client: u16,
    available: Decimal,
    held: Decimal,
    total: Decimal,
    locked: bool,
}

/// Writes every account as `client,available,held,total,locked`, in client order, with four
/// decimal places. The header is written even when there are no accounts.
pub(crate) fn write_accounts<W: Write>(engine: &Engine, output: W) -> io::Result<()> {
    let mut writer = WriterBuilder::new().has_headers(false).from_writer(output);
    writer.write_record(["client", "available", "held", "total", "locked"])?;
    for (client, account) in engine.accounts() {
        let balances = account.balances();
        writer.serialize(OutputRow {
            client: client.0,
            available: for_output(balances.available()),
            held: for_output(balances.held()),
            total: for_output(balances.total()),
            locked: account.is_locked(),
        })?;
    }
    // `Drop` would flush too, but would swallow the error.
    writer.flush()
}

/// Fixes the output scale at [`PRECISION`] places. `normalize` first turns a negative zero,
/// which `Decimal` arithmetic can produce, into a plain zero so it never prints as `-0.0000`.
fn for_output(value: Decimal) -> Decimal {
    let mut value = value.normalize();
    value.rescale(PRECISION);
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> Vec<(Option<u64>, Result<Transaction, String>)> {
        TransactionReader::new(input.as_bytes())
            .unwrap()
            .map(|row| {
                let row = row.unwrap();
                (row.row, row.transaction.map_err(|e| e.to_string()))
            })
            .collect()
    }

    fn header_error(input: &str) -> String {
        match TransactionReader::new(input.as_bytes()) {
            Ok(_) => panic!("header accepted: {input:?}"),
            Err(error) => error.to_string(),
        }
    }

    fn transactions(input: &str) -> Vec<Transaction> {
        parse(input)
            .into_iter()
            .map(|(_, transaction)| transaction.unwrap())
            .collect()
    }

    fn tx(client: u16, id: u32, kind: TxKind) -> Transaction {
        Transaction {
            client: ClientId(client),
            tx: TxId(id),
            kind,
        }
    }

    fn amount(raw: &str) -> Amount {
        raw.parse().unwrap()
    }

    fn output(engine: &Engine) -> String {
        let mut out = Vec::new();
        write_accounts(engine, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn parses_every_transaction_type() {
        let input = "type,client,tx,amount\n\
                     deposit,1,1,1.5\n\
                     withdrawal,1,2,0.5\n\
                     dispute,1,1,\n\
                     resolve,1,1,\n\
                     chargeback,1,1,\n";
        assert_eq!(
            transactions(input),
            [
                tx(1, 1, TxKind::Deposit(amount("1.5"))),
                tx(1, 2, TxKind::Withdrawal(amount("0.5"))),
                tx(1, 1, TxKind::Dispute),
                tx(1, 1, TxKind::Resolve),
                tx(1, 1, TxKind::Chargeback),
            ]
        );
    }

    #[test]
    fn trims_whitespace_in_fields_and_header() {
        let input = "type, client, tx, amount\n  deposit ,  2 , 7 ,  3.0001 \n";
        assert_eq!(
            transactions(input),
            [tx(2, 7, TxKind::Deposit(amount("3.0001")))]
        );
    }

    #[test]
    fn accepts_dispute_without_trailing_comma() {
        let input = "type,client,tx,amount\ndispute,1,1\nresolve, 1, 1\n";
        assert_eq!(
            transactions(input),
            [tx(1, 1, TxKind::Dispute), tx(1, 1, TxKind::Resolve)]
        );
    }

    #[test]
    fn ignores_amount_on_dispute_rows() {
        let input = "type,client,tx,amount\ndispute,1,1,5.0\nchargeback,1,1,junk\n";
        assert_eq!(
            transactions(input),
            [tx(1, 1, TxKind::Dispute), tx(1, 1, TxKind::Chargeback)]
        );
    }

    #[test]
    fn rejects_deposit_without_amount() {
        let rows = parse("type,client,tx,amount\ndeposit,1,1,\nwithdrawal,1,2\n");
        assert_eq!(
            rows,
            [
                (Some(1), Err("missing amount".into())),
                (Some(2), Err("missing amount".into())),
            ]
        );
    }

    #[test]
    fn rejects_invalid_amount() {
        let rows = parse("type,client,tx,amount\ndeposit,1,1,1.23456\n");
        assert_eq!(
            rows,
            [(
                Some(1),
                Err("invalid amount: amount has 5 decimal places; at most 4 are allowed".into())
            )]
        );
    }

    #[test]
    fn rejects_unknown_type_and_bad_ids_but_keeps_reading() {
        let input = "type,client,tx,amount\n\
                     refund,1,1,1.0\n\
                     deposit,70000,2,1.0\n\
                     deposit,1,x,1.0\n\
                     deposit,1,4,1.0\n";
        let rows: Vec<_> = parse(input)
            .into_iter()
            .map(|(row, result)| (row, result.err()))
            .collect();
        assert_eq!(
            rows,
            [
                (Some(1), Some("malformed row: unknown variant `refund`, expected one of `deposit`, `withdrawal`, `dispute`, `resolve`, `chargeback`".into())),
                (Some(2), Some("malformed row: client: number too large to fit in target type".into())),
                (Some(3), Some("malformed row: tx: invalid digit found in string".into())),
                (Some(4), None),
            ]
        );
    }

    #[test]
    fn rows_are_numbered_past_blank_lines() {
        let input = "type,client,tx,amount\n\n\ndeposit,1,1,1\n\r\ndeposit,1,2,abc\n";
        let rows: Vec<_> = parse(input).into_iter().map(|(row, _)| row).collect();
        assert_eq!(rows, [Some(1), Some(2)]);
    }

    #[test]
    fn header_without_required_columns_is_rejected() {
        assert_eq!(
            header_error("deposit,1,1,1.0\ndeposit,1,2,2.0\n"),
            "header is missing column(s) type, client, tx; expected `type, client, tx, amount`"
        );
        assert_eq!(
            header_error("kind,client,tx,amount\n"),
            "header is missing column(s) type; expected `type, client, tx, amount`"
        );
    }

    #[test]
    fn header_columns_may_be_reordered_or_extra() {
        let input = "tx,note,client,type,amount\n1,hi,2,deposit,1.5\n7,,2,dispute,\n";
        assert_eq!(
            transactions(input),
            [
                tx(2, 1, TxKind::Deposit(amount("1.5"))),
                tx(2, 7, TxKind::Dispute)
            ]
        );
        // `amount` itself is optional when no row needs it.
        assert_eq!(
            transactions("type,client,tx\ndispute,1,1\n"),
            [tx(1, 1, TxKind::Dispute)]
        );
    }

    /// A reader that yields `data`, then fails.
    struct FailsAfter<'a>(&'a [u8]);

    impl Read for FailsAfter<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.0.is_empty() {
                return Err(io::Error::other("disk on fire"));
            }
            self.0.read(buf)
        }
    }

    #[test]
    fn io_error_mid_stream_ends_the_stream() {
        let input = FailsAfter(b"type,client,tx,amount\ndeposit,1,1,1.0\n");
        let mut reader = TransactionReader::new(input).unwrap();
        assert!(reader.next().unwrap().unwrap().transaction.is_ok());
        let error = reader.next().unwrap().err().unwrap();
        assert_eq!(error.to_string(), "disk on fire");
    }

    #[test]
    fn invalid_utf8_row_is_skipped() {
        let mut input = b"type,client,tx,amount\ndeposit,1,1,\xff\n".to_vec();
        input.extend_from_slice(b"deposit,1,2,1.0\n");
        let rows: Vec<_> = TransactionReader::new(input.as_slice())
            .unwrap()
            .map(|row| row.unwrap().transaction.map_err(|e| e.to_string()).err())
            .collect();
        assert_eq!(
            rows,
            [Some("malformed row: amount: invalid UTF-8".into()), None]
        );
    }

    #[test]
    fn utf8_bom_is_ignored() {
        let input = "\u{feff}type,client,tx,amount\ndeposit,1,1,1.0\n";
        assert_eq!(
            transactions(input),
            [tx(1, 1, TxKind::Deposit(amount("1")))]
        );
    }

    #[test]
    fn crlf_is_accepted() {
        let input = "type,client,tx,amount\r\ndeposit,1,1,1.0\r\ndispute,1,1,\r\n";
        assert_eq!(
            transactions(input),
            [
                tx(1, 1, TxKind::Deposit(amount("1"))),
                tx(1, 1, TxKind::Dispute)
            ]
        );
    }

    #[test]
    fn empty_input_yields_no_rows() {
        assert_eq!(parse(""), []);
        assert_eq!(parse("type,client,tx,amount\n"), []);
    }

    #[test]
    fn writes_header_even_without_accounts() {
        assert_eq!(
            output(&Engine::default()),
            "client,available,held,total,locked\n"
        );
    }

    #[test]
    fn balances_are_written_with_four_decimal_places() {
        let mut engine = Engine::default();
        for t in transactions("type,client,tx,amount\ndeposit,1,1,1.5\ndeposit,1,2,2\n") {
            engine.apply(t).unwrap();
        }
        assert_eq!(
            output(&engine),
            "client,available,held,total,locked\n1,3.5000,0.0000,3.5000,false\n"
        );
    }

    #[test]
    fn negative_zero_is_written_as_zero() {
        // Subtracting to zero from a negative balance yields a negative zero in `Decimal`.
        let input = "type,client,tx,amount\n\
                     deposit,1,1,5\n\
                     withdrawal,1,2,5\n\
                     dispute,1,1,\n\
                     resolve,1,1,\n";
        let mut engine = Engine::default();
        for t in transactions(input) {
            engine.apply(t).unwrap();
        }
        assert_eq!(
            output(&engine),
            "client,available,held,total,locked\n1,0.0000,0.0000,0.0000,false\n"
        );
    }
}
