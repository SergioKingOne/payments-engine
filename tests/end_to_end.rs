//! Runs the compiled binary the way a user does: one CSV path in, accounts CSV on stdout.
//!
//! Each case in `tests/fixtures/<case>/` holds an `input.csv` and the `expected.csv` it must
//! produce. Output rows are compared after sorting, because row order is not part of the
//! contract.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail by panicking; clippy exempts only #[test] bodies"
)]

use std::path::Path;
use std::process::{Command, Output};

const HEADER: &str = "client,available,held,total,locked";

fn run_binary<I: AsRef<std::ffi::OsStr>>(args: &[I]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_payments-engine"))
        .args(args)
        .output()
        .unwrap()
}

/// The header line, then the remaining rows sorted.
fn header_and_sorted_rows(csv: &str) -> (Option<&str>, Vec<&str>) {
    let mut lines = csv.lines();
    let header = lines.next();
    let mut rows: Vec<_> = lines.collect();
    rows.sort_unstable();
    (header, rows)
}

fn check(case: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(case);
    let output = run_binary(&[dir.join("input.csv")]);
    assert!(output.status.success(), "{case}: {output:?}");

    let actual = String::from_utf8(output.stdout).unwrap();
    let expected = std::fs::read_to_string(dir.join("expected.csv")).unwrap();
    let (header, rows) = header_and_sorted_rows(&actual);
    assert_eq!(header, Some(HEADER), "{case}");
    assert_eq!((header, rows), header_and_sorted_rows(&expected), "{case}");
}

#[test]
fn spec_sample() {
    check("spec_sample");
}

#[test]
fn whitespace_and_precision() {
    check("whitespace_and_precision");
}

#[test]
fn dispute_resolve_redispute() {
    check("dispute_resolve_redispute");
}

#[test]
fn chargeback_locks_account() {
    check("chargeback_locks_account");
}

#[test]
fn dispute_after_withdrawal() {
    check("dispute_after_withdrawal");
}

#[test]
fn rejected_rows() {
    check("rejected_rows");
}

#[test]
fn header_only() {
    check("header_only");
}

#[test]
fn rejected_rows_are_reported_on_stderr_only() {
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rejected_rows/input.csv");
    let output = run_binary(&[input]);
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stdout.lines().all(|line| !line.starts_with("row ")));
    assert!(
        stderr.lines().all(|line| line.starts_with("row ")),
        "{stderr}"
    );
    assert!(
        stderr.contains("row 12: client 1, tx 9: insufficient funds"),
        "{stderr}"
    );
}

/// Asserts the run failed with nothing on stdout and `message` on stderr.
fn assert_fails_with(output: &Output, message: &str) {
    assert!(!output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert!(String::from_utf8_lossy(&output.stderr).contains(message));
}

#[test]
fn missing_argument_is_a_usage_error() {
    assert_fails_with(&run_binary::<&str>(&[]), "usage");
}

#[test]
fn extra_argument_is_a_usage_error() {
    assert_fails_with(&run_binary(&["a.csv", "b.csv"]), "usage");
}

#[test]
fn input_without_header_is_an_error() {
    let dir = std::env::temp_dir().join(format!("payments-engine-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("no_header.csv");
    std::fs::write(&input, "deposit,1,1,1.0\ndeposit,1,2,2.0\n").unwrap();
    let output = run_binary(&[&input]);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_fails_with(&output, "header is missing column(s) type, client, tx");
}

#[test]
fn missing_file_is_an_error() {
    let output = run_binary(&["does/not/exist.csv"]);
    assert_fails_with(&output, "cannot open does/not/exist.csv");
}
