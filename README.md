# payments-engine

A toy payments engine. It reads a CSV of deposits, withdrawals, disputes, resolves and
chargebacks, applies them in order to client accounts, and writes each account's final balances
as CSV to stdout.

```sh
cargo run -- tests/fixtures/spec_sample/input.csv
```

```text
client,available,held,total,locked
1,1.5000,0.0000,1.5000,false
2,2.0000,0.0000,2.0000,false
```

The sample's last withdrawal exceeds client 2's funds, so it is rejected and reported on stderr.

## Usage

```sh
cargo run -- transactions.csv > accounts.csv
```

Input columns are `type, client, tx, amount`. Whitespace around fields is ignored, columns may come
in any order, and dispute, resolve and chargeback rows may omit the amount. Output always has four
decimal places, sorted by client.

A row the engine cannot use is skipped. It is reported on stderr as one line, and processing
continues:

```text
row 5: client 2, tx 5: insufficient funds: 2 available, 3 requested
row 9: invalid amount: amount has 5 decimal places; at most 4 are allowed
```

`row N` counts data rows from 1, so the header and blank lines are not counted. Stdout only ever
carries the accounts CSV.

The exit status is 0 when the file was processed, even if some rows were rejected. It is 1 for a
missing or extra argument, an unreadable file, a header without the `type`, `client` and `tx`
columns, or a failed write. Nothing is written to stdout in those cases.

## Design

```
main.rs      reads the one argument, opens the file, maps errors to exit status 1
lib.rs       run(): reader → engine → writer, reports rejected rows
csv_io.rs    CSV ⇄ domain types; no business rules
engine.rs    Engine::apply(Transaction) -> Result<(), TxError>; all the rules
transaction.rs, money.rs   domain types
```

The engine has no I/O. `Engine::apply` takes one parsed `Transaction` and either applies all of it
or returns a `TxError` and changes nothing. `csv_io` streams the file one record at a time into a
single reused buffer, so input size does not affect memory.

State is a `BTreeMap<ClientId, Account>`. Each `Account` holds its balances, its lock status, and a
`HashMap<TxId, Deposit>` of its own deposits, the only transactions a dispute can name.
Withdrawals are applied and forgotten. Keeping deposits inside their client's account means a
dispute that names another client's transaction finds nothing. There is no cross-client check to
forget.

The types carry most of the rules:

- An `Amount` is positive, has at most 4 decimal places, and is at most 10^14. The only ways to make one validate all three.
- `Balances` stores `total`, and its only constructor computes it, so `total == available + held` for every value. Its operations are pure and return `None` on overflow, and the engine commits the result only when the whole transaction succeeds.
- `TxKind::Deposit(Amount)` and `TxKind::Withdrawal(Amount)` carry an amount. `Dispute`, `Resolve` and `Chargeback` do not, so a dispute row's amount column cannot leak into the arithmetic.
- A deposit's dispute lifecycle is an enum, `Settled → Disputed → Settled | ChargedBack`, not a pair of booleans that could both be true.

## Assumptions

The spec leaves these open. Each choice follows what a bank or card processor does.

| # | Rule | Why |
|---|---|---|
| 1 | Only deposits can be disputed. A dispute naming a withdrawal is rejected. | A chargeback claws back money that came *in*. Under this spec's model, a disputed withdrawal would hold funds the client no longer has. |
| 2 | A dispute applies even when it drives `available` negative. | This is the fraud case: deposit, withdraw, then reverse the deposit. Refusing the dispute would let the money leave. Processors debit into a negative balance and restrict the account. |
| 3 | A dispute, resolve or chargeback must name the same client's deposit. Otherwise it is rejected as unknown. | A partner cannot reverse one client's payment through another client's account. |
| 4 | A locked account rejects deposits and withdrawals. Disputes, resolves and chargebacks on its earlier deposits still apply. | A freeze stops client-initiated activity. Disputes come from the card network and keep arriving regardless. |
| 5 | A resolved deposit can be disputed again. A charged-back deposit is final. | A payment can receive more than one dispute; a lost dispute is closed. |
| 6 | A repeated deposit id for the same client is rejected. | Ids are unique by spec, so a repeat is a replay. Accepting it would credit the client twice. |
| 7 | Amounts that are zero, negative, missing, above 10^14, or have more than 4 decimal places are rejected, never rounded. So are `1e3` and `1_000`. | Silently rounding money hides a partner bug. |
| 8 | A withdrawal larger than `available` fails whole and changes nothing. | No partial withdrawals, as with a bounced debit. |
| 9 | A bad row is skipped and reported. Only I/O errors and an unusable header stop the run. | One bad row must not cost every other client their balances. |
| 10 | Any row that parses creates its client's account, even if the transaction is then rejected. A row that fails to parse creates nothing. | The spec says to create the client when a transaction references it. |
| 11 | An amount on a dispute, resolve or chargeback row is ignored. | Those rows identify the deposit by id. Rejecting them over an unused field would drop a real dispute. |

## Correctness

84 tests, all run in CI on stable and on the minimum supported Rust version, 1.85:

- **70 unit tests** sit next to the code they test. Each is named for the rule it checks, for example `dispute_may_drive_available_negative` or `charged_back_deposit_is_final`. Every rejection test also asserts the engine is unchanged.
- **12 end-to-end tests** (`tests/end_to_end.rs`) run the compiled binary:
  - 7 fixtures in `tests/fixtures/<case>/{input,expected}.csv`, one per group of rules above, compared as sorted rows;
  - checks that usage errors, a missing file and a missing header exit 1 with empty stdout.
- **2 property tests** (`tests/properties.rs`, proptest):
  - The first applies random transaction sequences over a few clients and ids, so collisions and repeated disputes are common. It then checks the engine's balances and lock flags against plain bookkeeping of the transactions the engine accepted, and checks that every rejection changes nothing. The bookkeeping does not re-implement the rules, so it cannot repeat their mistakes.
  - The second feeds junk-laden CSV through `run` and checks that the output is always well-formed.

Two tools measured the tests themselves (run once by hand, not part of CI):

| Tool | Result | Command |
|---|---|---|
| cargo-mutants 27.1 | 51 of 51 viable mutants caught, 0 missed | `cargo mutants` |
| cargo-llvm-cov 0.6 | 99.45% of lines, 98.1% of regions | `cargo llvm-cov --all-targets` |

The coverage figures include the inline test modules. The uncovered lines are defensive error arms.

## Safety and robustness

- `unsafe` is forbidden crate-wide (`unsafe_code = "forbid"` in `Cargo.toml`).
- Clippy rejects `unwrap`, `expect`, `panic!`, slice indexing and unchecked arithmetic outside tests (warnings fail CI). These are the usual ways input makes Rust code panic. The property tests feed junk input to back this up; they do not prove it.
- Money is `rust_decimal::Decimal`, never a float.
- Amounts are parsed from the raw text with `Decimal::from_str_exact`, behind a syntax check. serde's decimal path rounds after 28 digits and accepts exponents, so it is not used.
- All arithmetic is checked. Inputs are capped at 10^14, so balances stay below about 4.3 × 10^23 even after 2^32 transactions. That is far inside the range where `Decimal` is exact at 4 decimal places; near the top of its range it rounds instead of overflowing. An overflow still rejects the transaction rather than wrapping.
- Output goes through `normalize` then `rescale(4)`. `Decimal` subtraction can produce a negative zero, and this prints it as `0.0000`, not `-0.0000`.
- Output is written through a locked stdout and flushed explicitly. A closed pipe (`| head`) ends the run with an error, not a panic.
- A rejected value is quoted in diagnostics up to 32 characters, so a huge junk field cannot flood stderr.

## Efficiency

The input is streamed. Memory is used only for accounts, which are bounded at 65,536, and for deposits, which a later dispute may name.

Measured on an Apple M4 Max with the release build. `examples/generate.rs` makes a deterministic file: 60% deposits, 30% withdrawals, the rest disputes, resolves and chargebacks.

```sh
cargo run --release --example generate -- 10000000 > big.csv
/usr/bin/time -l target/release/payments-engine big.csv > /dev/null
```

| Rows | Stored deposits | Time | Peak memory footprint |
|---|---|---|---|
| 1M | 0.6M | 0.44 s | 56 MB |
| 10M | 6.0M | 4.5 s | 390 MB |
| 20M | 12.0M | 9.3 s | 734 MB |

Time is linear, at about 2.2 million rows per second. Each stored deposit costs about 60 bytes: a
24-byte map entry, plus hash-table slack and resize overhead. The debug build that `cargo run`
uses is about 11× slower.

A full u32 id space of deposits would need roughly 260 GB, which does not fit in memory. A
production system would move old deposits to a disk-backed store keyed by tx id, or evict them
once the dispute window has passed. Neither is built here.

## Scaling to many concurrent streams

This section describes a design; none of it is built. The engine is synchronous and I/O-free,
so a server would wrap it rather than change it:

1. One async task per TCP connection parses rows with the same `csv_io` code.
2. Each row goes to a shard chosen by `client_id % N`, over a bounded channel. Backpressure then reaches slow producers instead of growing memory.
3. Each shard owns a plain `Engine` for its clients, so no locks are needed. Rule 3 means a row never touches another client's data, so no row ever needs two shards.

Order is preserved within each client, which is the only order the rules depend on. Order between
streams is whatever the network delivers. The real system would also need a durable log in front
of the shards, to be replayed after a crash.

## Use of AI

I built this with an AI coding assistant, Claude Code (Claude Opus 5.5), working in this
repository. I set the goals and constraints, had the work broken into tickets, and approved each plan
before it was built. The architecture and design choices were discussed with the assistant and
agreed between us, and each one was recorded with its alternatives and sources. The assistant did
the research, drafted the design, the code, the tests and this README, and ran the tools.

Its output was not trusted on its own word:

- Every design decision was recorded with the alternatives and sources behind it, and checked
  against primary sources: the Rust and Cargo documentation, the crates' own source and issue
  trackers, and how card networks and processors handle disputes.
- Two independent review passes ran in fresh sessions with no access to the author's reasoning,
  one on the design and one on the finished code. Every finding had to be reproduced before it
  was acted on.
- Claims were backed by evidence rather than by the assistant's say-so: runs of the real binary,
  bugs planted to show a test catches them, mutation testing, and measurements.

The full session transcript (prompts, responses, commands run and their output) is provided with
the submission.

## Development

```sh
cargo fmt --all --check
cargo clippy --all-targets      # pedantic plus selected restriction lints, warnings denied in CI
cargo test
cargo +1.85 test                # minimum supported Rust version
```

CI runs all of these on every push, with GitHub Actions pinned to commit SHAs and a read-only
token.
