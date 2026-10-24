use std::env;
use std::fs::File;
use std::io::{self, BufWriter};
use std::path::PathBuf;

use anyhow::{Context, bail};

const USAGE: &str = "usage: payments-engine <transactions.csv>";

/// Reads the transactions file named by the only argument and writes account balances to
/// stdout. Rejected rows are reported on stderr. Exits with status 1 on usage or I/O errors.
fn main() -> anyhow::Result<()> {
    let mut args = env::args_os().skip(1);
    let (Some(path), None) = (args.next(), args.next()) else {
        bail!(USAGE);
    };
    let path = PathBuf::from(path);
    let input = File::open(&path).with_context(|| format!("cannot open {}", path.display()))?;
    payments_engine::run(
        input,
        io::stdout().lock(),
        BufWriter::new(io::stderr().lock()),
    )
    .with_context(|| format!("failed to process {}", path.display()))
}
