# Benchmarking resources

This directory contains source-controlled workloads for developing and evaluating the adaptive
conflict graph implementation.

The benchmark contracts live in a separate Cargo workspace. This keeps CosmWasm dependencies out
of the core graph workspace while preserving reproducible, independently compilable contract
sources.

```text
benchmarks/
├── Cargo.toml
├── contracts/
│   ├── conflictlab/src/lib.rs
│   └── miniwarehouse/src/lib.rs
└── symbolic/
    ├── conflictlab.symbolic.json
    └── miniwarehouse.symbolic.json
```

## Workloads

### ConflictLab

ConflictLab is the debugging workload. Its entrypoints deliberately isolate graph features:

- input-keyed balances and counters;
- composite allowance keys;
- singleton `CONFIG` fields;
- same-profile conflicts;
- conditional guards;
- delegated execution (`ReceiveTransfer` to `Credit`);
- state-derived keys (`CancelOrder`);
- wildcard map scans (`ResetAllBalances`).

The source is a single Rust file:

```text
benchmarks/contracts/conflictlab/src/lib.rs
```

Its analyzer-compatible symbolic profile is:

```text
benchmarks/symbolic/conflictlab.symbolic.json
```

### MiniWarehouse

MiniWarehouse is a compact, explicitly non-compliant TPC-C-inspired integration workload. It
contains warehouse, district, customer, stock, order, new-order, order-line, and history records.
It exercises multi-resource transactions, variable-length order lines, remote stock, prefix scans,
and state-derived customer keys.

The source is a single Rust file:

```text
benchmarks/contracts/miniwarehouse/src/lib.rs
```

Its analyzer-compatible symbolic profile is:

```text
benchmarks/symbolic/miniwarehouse.symbolic.json
```

The symbolic JSON files were produced by analyzing the contract sources directly. They are parser
inputs, not programmer-supplied ground-truth access tables. Each access includes source evidence,
dependency classification, guards, and key provenance in the same schema used by the Astroport
fixture.

## Compile and test the benchmark contracts

From the repository root:

The benchmark workspace pins two transitive RustCrypto dependencies:

- `base64ct = 1.6.0`
- `zeroize = 1.8.2`

The CosmWasm cryptography stack accepts compatible `1.x` releases, but newer releases now require
Rust 1.85 and Edition 2024. These direct exact-version constraints keep dependency resolution
compatible with the repository MSRV of Rust 1.75.

If a previous failed resolution created `benchmarks/Cargo.lock`, repair it once with:

```bash
cargo update --manifest-path benchmarks/Cargo.toml -p base64ct --precise 1.6.0
cargo update --manifest-path benchmarks/Cargo.toml -p zeroize --precise 1.8.2
```

Confirm the selected versions:

```bash
cargo tree --manifest-path benchmarks/Cargo.toml -i base64ct
cargo tree --manifest-path benchmarks/Cargo.toml -i zeroize
```

The output should contain `base64ct v1.6.0` and `zeroize v1.8.2`. Then run:

```bash
cargo fmt --manifest-path benchmarks/Cargo.toml --all -- --check
cargo clippy --manifest-path benchmarks/Cargo.toml --workspace --all-targets -- -D warnings
cargo test --manifest-path benchmarks/Cargo.toml --workspace
```

To build WebAssembly artifacts:

```bash
rustup target add wasm32-unknown-unknown
cargo build \
  --manifest-path benchmarks/Cargo.toml \
  --workspace \
  --release \
  --target wasm32-unknown-unknown
```

The resulting `.wasm` files are written under:

```text
benchmarks/target/wasm32-unknown-unknown/release/
```

## Validate all symbolic fixtures

The root workspace contains parser and profile-graph tests for Astroport, ConflictLab, and
MiniWarehouse:

```bash
cargo test --workspace --all-targets
```

Run only the benchmark symbolic-profile parser tests:

```bash
cargo test -p acg-symbolic-json --test benchmarks -- --nocapture
```

Run only the benchmark graph tests:

```bash
cargo test -p acg-profile-graph --test benchmarks -- --nocapture
```

## Compile the ConflictLab profile graph

Build the CLI once:

```bash
cargo build -p acg-cli
export ACG=target/debug/acg-profilec
```

Compile ConflictLab:

```bash
$ACG compile \
  --input benchmarks/symbolic/conflictlab.symbolic.json \
  --output /tmp/conflictlab.profile-graph.json \
  --runtime cosmwasm \
  --code-hash 1111111111111111111111111111111111111111111111111111111111111111
```

Expected compiler summary:

```text
compiled 18 profiles and 63 profile edges into /tmp/conflictlab.profile-graph.json
```

Inspect the graph:

```bash
$ACG inspect --input /tmp/conflictlab.profile-graph.json
```

Expected output:

```text
profiles: 18
edges: 63
conditional_edges: 46
unconditional_edges: 0
unknown_edges: 17
```

Inspect the delegated profile:

```bash
jq '
  .profiles[]
  | select(.entrypoint_name == "execute::ReceiveTransfer")
  | { delegates_to, accesses }
' /tmp/conflictlab.profile-graph.json
```

The loaded profile should contain the two inherited `BALANCES` accesses from `execute::Credit`,
with a non-empty delegation path.

Inspect the wildcard reset edge:

```bash
RESET_KEY=$(
  jq -r '.profiles[] | select(.entrypoint_name == "execute::ResetAllBalances") | .stable_key' \
    /tmp/conflictlab.profile-graph.json
)
BALANCE_KEY=$(
  jq -r '.profiles[] | select(.entrypoint_name == "query::Balance") | .stable_key' \
    /tmp/conflictlab.profile-graph.json
)

jq --arg left "$RESET_KEY" --arg right "$BALANCE_KEY" '
  .edges[]
  | select(
      (.source == $left and .target == $right)
      or (.source == $right and .target == $left)
    )
' /tmp/conflictlab.profile-graph.json
```

That edge should be `unknown` and contain an unresolved key match because the contract scans and
removes every balance key.

## Compile the MiniWarehouse profile graph

```bash
$ACG compile \
  --input benchmarks/symbolic/miniwarehouse.symbolic.json \
  --output /tmp/miniwarehouse.profile-graph.json \
  --runtime cosmwasm \
  --code-hash 2222222222222222222222222222222222222222222222222222222222222222
```

Expected compiler summary:

```text
compiled 14 profiles and 44 profile edges into /tmp/miniwarehouse.profile-graph.json
```

Inspect it:

```bash
$ACG inspect --input /tmp/miniwarehouse.profile-graph.json
```

Expected output:

```text
profiles: 14
edges: 44
conditional_edges: 37
unconditional_edges: 0
unknown_edges: 7
```

Inspect `NewOrder`:

```bash
jq '
  .profiles[]
  | select(.entrypoint_name == "execute::NewOrder")
  | {
      access_count: (.accesses | length),
      resources: [.accesses[].resource] | unique,
      accesses
    }
' /tmp/miniwarehouse.profile-graph.json
```

Expected access count: `10`. The resources should include `WAREHOUSES`, `DISTRICTS`, `CUSTOMERS`,
`STOCK`, `ORDERS`, `NEW_ORDERS`, and `ORDER_LINES`.

Inspect the deliberately unresolved `Delivery`/`OrderStatus` relationship:

```bash
DELIVERY_KEY=$(
  jq -r '.profiles[] | select(.entrypoint_name == "execute::Delivery") | .stable_key' \
    /tmp/miniwarehouse.profile-graph.json
)
STATUS_KEY=$(
  jq -r '.profiles[] | select(.entrypoint_name == "query::OrderStatus") | .stable_key' \
    /tmp/miniwarehouse.profile-graph.json
)

jq --arg left "$DELIVERY_KEY" --arg right "$STATUS_KEY" '
  .edges[]
  | select(
      (.source == $left and .target == $right)
      or (.source == $right and .target == $left)
    )
' /tmp/miniwarehouse.profile-graph.json
```

The edge should be `unknown`. Schema 3.1 can represent equality between concrete keys but cannot
yet express an exact-key-versus-prefix relationship for `ORDER_LINES`. `Delivery` also obtains its
customer ID from the stored order, making that customer key state-derived.

## Determinism checks

Compile either symbolic input twice and compare the files:

```bash
$ACG compile \
  --input benchmarks/symbolic/conflictlab.symbolic.json \
  --output /tmp/conflictlab-a.json \
  --runtime cosmwasm \
  --code-hash 1111111111111111111111111111111111111111111111111111111111111111

$ACG compile \
  --input benchmarks/symbolic/conflictlab.symbolic.json \
  --output /tmp/conflictlab-b.json \
  --runtime cosmwasm \
  --code-hash 1111111111111111111111111111111111111111111111111111111111111111

cmp /tmp/conflictlab-a.json /tmp/conflictlab-b.json
sha256sum /tmp/conflictlab-a.json /tmp/conflictlab-b.json
```

`cmp` should produce no output and both hashes should match.

## Intended evaluation progression

Use the workloads in this order:

1. **ConflictLab** for exact predicate and transaction-edge debugging.
2. **MiniWarehouse** for multi-key, variable-size, and contention experiments.
3. **Astroport** for realistic source complexity.

The benchmark contracts are research resources. They are intentionally compact and have not been
audited for deployment with real assets.
