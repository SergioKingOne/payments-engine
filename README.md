# payments-engine

A toy payments engine. It streams a CSV of transactions, applies deposits, withdrawals,
disputes, resolutions and chargebacks to client accounts, and writes the final account
balances as CSV to stdout.

## Usage

```sh
cargo run -- transactions.csv > accounts.csv
```

## Development

```sh
cargo fmt --all --check
cargo clippy --all-targets
cargo test
```

<!-- Sections to fill in as the implementation lands: design, assumptions, correctness, safety, efficiency. -->
