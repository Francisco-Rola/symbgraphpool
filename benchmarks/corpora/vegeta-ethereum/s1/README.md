# Vegeta Ethereum S1 corpus

Vegeta NSDI'25 Table 2 defines **S1** as Ethereum mainnet blocks **16,774,645 through
16,779,644 inclusive**: 5,000 blocks. The paper reports 739,863 transactions, a summed
longest-chain metric of 88,136, and ratio 8.39. S1 is the paper's principal single-node
performance and correctness workload.

Ethereum history is not vendored in this repository. Reconstruct it from an archive-capable RPC.

## 1. Probe the provider

Hosted-provider-compatible built-in tracing:

```bash
ETH_RPC_URL=https://YOUR_RPC \
VEGETA_S1_PROBE_ONLY=1 \
bash tools/legacy-scripts/run-vegeta-s1-collect.sh
```

For a self-hosted Geth node that permits arbitrary JavaScript tracers:

```bash
ETH_RPC_URL=http://127.0.0.1:8545 \
VEGETA_S1_TRACE_MODE=custom-js \
VEGETA_S1_PROBE_ONLY=1 \
bash tools/legacy-scripts/run-vegeta-s1-collect.sh
```

## 2. Collect the portable corpus

```bash
ETH_RPC_URL=https://YOUR_RPC \
bash tools/legacy-scripts/run-vegeta-s1-collect.sh
```

The default `public-rpc` mode uses Geth's built-in `prestateTracer` in normal and diff modes.
As with S3, it records conservative touched-storage reads and state-changing writes. All block files
are atomic and `--resume` is always enabled, so interrupting and rerunning the command is safe.

Outputs:

- `blocks/<height>.json`: one durable block checkpoint per Ethereum block;
- `corpus.jsonl`: ordered 5,000-block corpus assembled after collection finishes;
- `manifest.json`: trace semantics, exact paper range, paper targets, and observed metrics;
- `validation-report.json`: shape and provenance validation.

## 3. Collect exact SLOAD/SSTORE accesses

For publication/oracle work, prefer exact custom-JS tracing. A self-hosted archive Geth is the
practical fast path because `custom-js` traces once per block:

```bash
ETH_RPC_URL=http://127.0.0.1:8545 \
bash tools/legacy-scripts/run-vegeta-s1-exact-trace.sh
```

If a hosted provider allows arbitrary custom tracers only through `debug_traceTransaction`, use the
checkpointed transaction mode instead:

```bash
ETH_RPC_URL=https://YOUR_RPC \
VEGETA_S1_EXACT_TRACE_MODE=custom-js-tx \
VEGETA_S1_EXACT_TX_DELAY=0.02 \
bash tools/legacy-scripts/run-vegeta-s1-exact-trace.sh
```

That mode checkpoints every transaction under `s1-exact-sload-sstore/tx-traces/`, so a collection
that takes many hours or days can be stopped and resumed without losing completed traces.

The exact collector intentionally fails closed. If a provider cannot trace a particular transaction,
do not silently drop it. The optional `VEGETA_S1_EXACT_FALLBACK_TXS` / `VEGETA_S1_EXACT_FALLBACK_FILE`
mechanism exists only for audited exceptions and records those exceptions in the manifest.

## Validation policy

The block range and transaction ordering are corpus identity. The paper's aggregate transaction
count, longest-chain value, ratio, and WETH hotspot are provenance/instrumentation targets rather
than silently assumed ground truth. Once an independent canonical S1 transaction-count audit is
frozen, it can be promoted to a hard identity check in `tools/vegeta/vegeta_corpus.py`.

## Native translation fidelity gates

The S1 transaction-deficit report also exposes a **contention-oriented transaction diagnostic**:
reviewed successful state semantics among source transactions that actually participate in at least one
source conflict pair. This is reported alongside, not in place of, the frozen all-transaction semantic
replay gate. It helps distinguish scheduler/dependency fidelity from general Ethereum replay fidelity.

For unresolved high-impact ERC-721 selectors, owner-scoped public-log effect audits may be used to
classify mint/burn/transfer effects before any semantic adapter is added. These audits never consume
concrete historical storage keys and never promote selectors automatically.

After the RPC corpus/callTracer/code caches are frozen, S1 native preparation uses a separate reviewed
state-semantics audit. The source `prestateTracer` corpus measures conservative storage touches, not
only committed writes. Consequently a reviewed state query is a real dependency (`STATE_READ`), while
a pure computation is not; reviewed reverted state paths are retained only in the denominator-aligned
state-touch view and excluded from the successful/committed-path diagnostic.

Run the local-only audit with:

```bash
bash tools/legacy-scripts/run-vegeta-s1-semantic-coverage.sh
```

S1 now reports two explicit readiness profiles instead of overloading one transaction denominator:

- **`scheduler-fidelity`**: reviewed state-touch conflict coverage >=95%, median conflict-bearing-block
  coverage >=80%, and successful reviewed-state coverage among source conflict-participating
  transactions >=80%. This is the readiness profile for the Vegeta-derived contention/scheduler
  benchmark.
- **`semantic-replay`**: the same common conflict/implementation gates plus successful reviewed-state
  coverage across **all retained source transactions** >=80%. This remains the stronger general EVM
  semantic-replay claim and is reported separately rather than being silently weakened.

`run-vegeta-s1-prepare-native.sh` keeps `semantic-replay` as its backwards-compatible default. To
prepare the scheduler benchmark explicitly, run:

```bash
VEGETA_S1_READINESS_PROFILE=scheduler-fidelity \
  bash tools/legacy-scripts/run-vegeta-s1-prepare-native.sh
```

or equivalently:

```bash
bash tools/legacy-scripts/run-vegeta-s1-prepare-native-scheduler.sh
```

Both profiles are written to `native-plan/readiness.{json,txt}`. Passing scheduler-fidelity must never
be described as 80% semantic coverage of all Ethereum transactions. Concrete source access sets remain
offline audit inputs and are never copied into `native-plan.jsonl`.

The same wrapper also emits `native-plan/transaction-deficit.{json,txt}`.  This diagnostic keeps the
80% all-transaction publication denominator unchanged, but reports the successful reviewed-state
share among source storage-access transactions and source conflict participants separately.  It also
ranks mapped-owner opaque selectors and background fallbacks by *unique currently-deficit
transactions* so selector review is not driven by raw nested call-frame volume alone.

Blitkin's S1 proxy owner (`0xbd18...f2ac`) has two owner-scoped one-token mint adapters; neither
selector is enabled family-wide. `0x29a0eee8` is the reviewed `mint(uint8,uint8)` path. The follow-up
public effect audit resolves `0xc96602d9` to `allowlistMint(uint8,uint8,bytes32[])`: each of the 733
committed selector transactions emits exactly one ERC-721 mint to the EVM-visible `msg.sender`, all 17
reverted transactions emit no committed owner log, every call carries 0.05 ETH, and the committed
source token IDs are sequential 647..1379. Publication preparation still requires the independent
all-owner zero-address ERC-721 `Transfer` audit to agree with every successful translated mint count,
so an event-backed review cannot silently advance the native drop sequence incorrectly. Reverted
source call scopes are excluded from committed mint-event counting while remaining visible to
touched-state semantic coverage.

### MIA selector effect audit

```bash
time bash tools/legacy-scripts/run-vegeta-s1-mia-mint-audit.sh
```

The reviewed MIA owner (`0x8855...05b5`) uses an owner-scoped `0xfd883998` adapter only after the
public-log effect audit succeeds.  The observed S1 audit has 1,325 selector transactions: 1,323
committed calls each emit exactly one ERC-721 `Transfer(from=0)` to the EVM-visible `msg.sender`, the
two reverted calls emit no committed mint event, and there are no other owner mint transactions in
the S1 interval.  Token IDs are not sequential, so native execution must not infer `next_token_id`. The follow-up effect
audit establishes that ABI word 2 equals the emitted token ID for every committed selector call;
committed execution still cross-checks that public calldata value against the frozen `Transfer` audit
before mapping it through the collision-free native token-ID bijection. Reverted source scopes use the
same public calldata token ID inside the discarded native revert overlay, preserving the attempted NFT
key without inventing committed ownership. Full execution fails closed if the audit is missing, has
duplicate committed token IDs, or violates the one-mint/recipient/revert/calldata-event invariants.

### Sequential CW721 mint reconciliation

Full scheduler preparation fail-closes on every committed zero-address ERC-721 `Transfer` for the
reviewed sequential `cw721-drop` owners. The mint-sequence artifact uses schema v2 and records the
indexed recipient as well as token IDs/counts, allowing execution to cross-check calldata-derived
airdrops and a few owner-scoped public-event-backed effects without importing EVM storage slots.
Known owner-specific S1 airdrop/reserve selectors remain out of the family-wide selector table.
For signed `0x2955a21d`, calldata preserves the requested `numberOfTokens`, while the frozen committed
Transfer cardinality determines the native quantity when source execution mints fewer tokens than
requested; both values remain in call provenance. The validator's comparison domain is exactly the
owners contained in the frozen sequential mint audit, so unrelated `cw721-drop` instances cannot be
misreported as unexpected reviewed-owner mints.
