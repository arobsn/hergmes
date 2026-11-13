# AGENTS.md

hergmes is a realtime, mempool-aware relay for the Ergo blockchain. It sits in front of an indexed
Ergo node and streams box, balance and mempool changes for tracked addresses and small-supply
tokens. See `README.md` for the overview.

## Documents

- `DESIGN.md` is the spec: protocol, engine semantics, edge cases and the testing layers.
- `PLAN.md` is the build order: numbered steps, each with a **Done when** and notes on what was
  actually done.
- Both are gitignored local files. Read the sections that cover the code you are touching before
  changing it.
- When a decision changes or a finding contradicts the docs, update `DESIGN.md`, and record what
  was done and why in that step's notes in `PLAN.md`. Write in the same style: short declarative
  sentences that give the reason.

## Commands

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- --deny warnings
cargo test                    # long property runs: cargo test --release
RUST_LOG=hergmes=debug cargo run
node scripts/record-fixtures.ts --node http://127.0.0.1:9053   # re-record node fixtures
```

CI runs the first three. All of them must pass before a change is done.

## Workflow

- Never commit, and never offer to. Leave changes in the working tree; the user commits.
- Every plan step leaves `main` runnable.
- A step is done when its **Done when** holds, not when the code compiles.

## Node behaviour

- The node's source code is the source of truth, not the OpenAPI spec, SDK conventions or memory.
  Check behaviour in `ergoplatform/ergo` at the commit of the node the fixtures were recorded
  from (`appVersion` in `tests/fixtures/info.json`; 6.0.8 is `c1a69f65`). Clone it into a
  scratch directory, since there is no local checkout.
- Cite the file and function you relied on in `DESIGN.md` and in code comments, so the check can
  be repeated after a node upgrade.
- Build serde types from the recorded fixtures, not from the spec.

## Architecture

```
NodeSource (trait)  ──►  Engine (pure state machine)  ──►  Transport (REST + WS)
 real: NodeClient         Engine::apply(Event) -> Result<Vec<Output>, EngineError>
 test: SimChain           Engine::query(Query) -> Reply
```

- `src/engine/` has no I/O and no clock: no async, no time, no randomness. Anything time-like is
  counted in blocks (e.g. the untracking grace period), so the engine is deterministic and
  replayable in the simulator.
- Adapters (`src/adapters/`) do all I/O. They stay thin: fetch, stamp with the observed tip, and
  send events to the engine.
- The engine refuses events that are not about its tip (`EngineError`) and changes nothing; the
  caller resynchronizes.
- Within one event, outputs go `block` / `rollback` / `resync_required`, then `mempool`, then
  `balance`, each in key order. Keep that order.

## Be frugal with resources

Memory, CPU and node calls are budgets, not afterthoughts.

- Wire types deserialize only the fields the relay reads. A field comes back only when a concrete
  use needs it.
- Per-key state is ids only. Full boxes live in bounded structures (ring buffer, LRU cache).
- Prefer fixed-size byte arrays to hex strings, integers to strings, and shared data stored once.
- Batch node calls where the node allows it, cache what never changes, and reuse data already
  fetched.

## Logging

- Use `tracing`. The default filter is `info`; `RUST_LOG` overrides it.
- The engine logs at `debug`, and only the decisions that leave no trace in what it returns (a
  mempool snapshot held, ignored or pruned, an idle key untracked, a duplicate load).
- Whatever the engine returns (`EngineError`, `resync_required`, `LoadRejected`, block deltas) is
  logged by the engine task that receives it, never inside the engine, so nothing is logged
  twice.
- Adapters log their own I/O decisions: reorg depth, back-off, retries, dropped txs.

## Testing

- Unit tests live in a sibling `<module>_tests.rs`, included from the module with
  `#[cfg(test)] #[path = "<module>_tests.rs"] mod tests;`.
- The engine is tested against `SimChain` and the oracle in `src/sim/` (compiled for tests, or
  with the `sim` feature for integration tests). The oracle recomputes state from scratch and is
  meant to be slow and obviously correct.
- Write the properties first, then the code.
- Check that a property can fail: plant bugs one at a time and confirm a test catches each, then
  record the list in the step's notes in `PLAN.md`. Cases the simulator can't reach get a
  hand-driven test.
- Node client tests run against `tests/fixtures/` served with `wiremock`. A new endpoint needs a
  recorded fixture.
- Keep the `proptest-regressions/` files: they are tracked, and replay past failures.

## Style

- `rustfmt.toml` sets the format (edition 2024, width 100). Run `cargo fmt`.
- Doc comments say what a thing is and why it is that way, not how the code does it.
