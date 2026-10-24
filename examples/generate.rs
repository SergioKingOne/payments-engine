//! Writes a large, valid transactions CSV to stdout, for measuring throughput and memory.
//!
//! ```sh
//! cargo run --release --example generate -- 10000000 > transactions.csv
//! ```
//!
//! The output is deterministic: the same row count always gives the same file. Every client id
//! (0 to 65535) appears. Each transaction id is used once and owned by one client, so disputes,
//! resolves and chargebacks can name an earlier transaction with its real owner without the
//! generator remembering anything. Roughly 60% deposits, 30% withdrawals, 6% disputes,
//! 4% resolves and 0.1% chargebacks.

use std::env;
use std::io::{self, BufWriter, Write};

const DEFAULT_ROWS: u32 = 10_000_000;

/// How far back a dispute, resolve or chargeback may reach. A small window makes a resolve or
/// chargeback likely to find a deposit that is actually under dispute.
const WINDOW: u32 = 64;

fn main() -> io::Result<()> {
    let rows = match env::args().nth(1) {
        Some(arg) => arg
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "rows must be a u32"))?,
        None => DEFAULT_ROWS,
    };
    let mut out = BufWriter::new(io::stdout().lock());
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    writeln!(out, "type, client, tx, amount")?;
    for tx in 1..=rows {
        let roll = rng.below(1000);
        // Positive, at most 4 decimal places: 0.0001 to 1000.0000.
        let units = rng.below(10_000_000).wrapping_add(1);
        let (whole, fraction) = (units / 10_000, units % 10_000);
        match roll {
            0..600 => writeln!(
                out,
                "deposit, {}, {tx}, {whole}.{fraction:04}",
                client_of(tx)
            )?,
            600..900 => writeln!(
                out,
                "withdrawal, {}, {tx}, {whole}.{fraction:04}",
                client_of(tx)
            )?,
            _ => {
                let kind = match roll {
                    900..960 => "dispute",
                    960..999 => "resolve",
                    _ => "chargeback",
                };
                // An earlier id, which may be a withdrawal or already settled: that is fine, the
                // engine has to handle those rows too.
                let back = u32::try_from(rng.below(u64::from(WINDOW))).unwrap_or(0);
                let target = tx.saturating_sub(back).max(1);
                writeln!(out, "{kind}, {}, {target},", client_of(target))?;
            }
        }
    }
    out.flush()
}

/// The client that owns transaction `tx`: a fixed scramble of the id onto all 65,536 clients.
fn client_of(tx: u32) -> u16 {
    let mixed = tx.wrapping_mul(0x9E37_79B9) ^ (tx >> 16);
    u16::try_from(mixed >> 16).unwrap_or(0)
}

/// xorshift64: fast, deterministic, and good enough for test data. Not for anything secret.
struct XorShift(u64);

impl XorShift {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0.checked_rem(bound).unwrap_or(0)
    }
}
