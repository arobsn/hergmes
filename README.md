# hergmes

A realtime, mempool-aware relay for the [Ergo](https://ergoplatform.org) blockchain.

hergmes sits in front of an indexed Ergo node and lets clients track the boxes of **addresses**
and **small-supply tokens** (NFTs and tokens with an emission of at most 100). A client gets a
key's current boxes and balance, then receives live updates: new boxes, spent boxes, pending
mempool activity and reorgs.

> **Status:** early development. The node client, wire types, address codec, engine types and a
> mempool watcher are in place; the engine, adapters, REST and WebSocket layers are still being
> built.

## How it works

The relay uses a **snapshot + deltas** sync model:

- **REST** serves the confirmed box set of a key, paginated (keyset) and pinned to a block.
- **WebSocket** streams deltas from that block onward: confirmed block changes, rollbacks, and a
  per-key mempool overlay.

Every snapshot and block event carries both `height` and `headerId`, so a client resuming after a
reorg is either rolled back to the fork point or told to resync, never left silently diverged.

## Requirements

- An Ergo node with the **extra indexer** enabled (`extraIndex = true`, for the `/blockchain/*`
  endpoints) and a UTXO state (`stateType = "utxo"`, the default): in digest mode the node
  resolves no mempool inputs and serves no `/utxo/withPool` boxes
- Node.js 23.6+, if you want to re-record test fixtures

## Getting started

```sh
cp .env.example .env
# set ERGO_NODE_URL to your indexed node, e.g. http://127.0.0.1:9053
cargo run
```

| Variable        | Description                            |
| --------------- | -------------------------------------- |
| `ERGO_NODE_URL` | URL of an indexed Ergo node (required) |

Logging uses `tracing`; set `RUST_LOG` (e.g. `RUST_LOG=hergmes=debug`) to adjust verbosity.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- --deny warnings
cargo test
```

- The `sim` feature exposes the `sim` module (a simulated chain and oracle) to integration tests.
- Node client tests run against recorded responses in `tests/fixtures/`, served with `wiremock`.
  To re-record them from a live node:

    ```sh
    node scripts/record-fixtures.ts --node http://127.0.0.1:9053
    ```

    See the header of `scripts/record-fixtures.ts` for all options.

## Project layout

| Path              | Contents                                                     |
| ----------------- | ------------------------------------------------------------ |
| `src/clients/`    | Ergo node HTTP client and the `NodeSource` trait             |
| `src/types/`      | Wire types and the address codec                             |
| `src/engine/`     | Tracking keys, block/delta types, engine events and messages |
| `src/watcher/`    | Background mempool watcher                                   |
| `src/adapters/`   | Node I/O feeding the engine (in progress)                    |
| `src/api/`        | REST and WebSocket transport (in progress)                   |
| `src/sim/`        | Simulated chain for tests (`sim` feature)                    |
| `tests/fixtures/` | Recorded node responses                                      |
