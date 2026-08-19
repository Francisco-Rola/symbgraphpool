# Vegeta Ethereum trace-port evaluation

`./scripts/run-vegeta-s3-smoke.sh` runs three representative S3 blocks through all seven common
harness strategies:

- serial
- AriaFB-like
- Vegeta-like (canonical-order, no Rule-1 reordering)
- exact-access evaluation oracle
- SymbGraph static
- SymbGraph probability-only
- SymbGraph cost-aware

The workload replays concrete EVM storage accesses extracted from Vegeta's NSDI'25 S3 block range.
The trace-replay message carries concrete accesses for execution, but the symbolic candidate profile
**never references those fields**: its predicates can bind only `predicted_reads` and
`predicted_writes`. For each Ethereum method identity `(to, selector)`, those prediction arrays are
constructed only from preceding corpus blocks, subject to the frequency and top-K controls in the
grid. This keeps the exact trace available to the execution harness without turning SymbGraph into
the exact-access oracle.

The smoke deliberately keeps admission/regime bypass disabled so it remains a mechanism
comparison aligned with the existing baseline smoke. Production-control-plane experiments should
use a separate grid.

### Frozen exact-trace exceptions

The exact S3 reconstruction has a versioned exception list at
`evaluation/vegeta/s3-exact-trace-fallbacks.v1.txt`. Transactions in this file are not dropped:
the extractor substitutes their already-frozen `public-rpc`/`prestateTracer` access record and
records each exception in `manifest.json::trace_semantics_exceptions`. All other transactions
continue to use exact transaction-level SLOAD/SSTORE tracing.

The wrapper merges this frozen list with any additional comma-separated hashes supplied via
`VEGETA_S3_EXACT_FALLBACK_TXS`, with deduplication. This makes the known exceptions reproducible
without requiring a long shell environment value on every resume.

## S3 contract/code-family characterization

Before treating the trace replay as a faithful SymbGraph workload, characterize how much of S3 is
covered by a manageable set of destination addresses, `(destination, selector)` methods, storage
owners, and runtime-bytecode families.

Offline characterization requires only the extracted corpus:

```bash
bash scripts/run-vegeta-s3-characterization.sh
```

This writes:

- `benchmarks/corpora/vegeta-ethereum/s3/characterization/characterization.json`
- `benchmarks/corpora/vegeta-ethereum/s3/characterization/characterization.txt`

The conflict metric is deliberately explicit: two transactions form a conflict pair when they are
in the same block, touch the same canonical EVM storage key, and at least one transaction writes it.
Conflict pairs are also attributed to the storage-owning address whose key caused the conflict.

To determine how many distinct runtime-code families cover the workload, reuse a historical-capable
Ethereum RPC and add `--fetch-code`:

```bash
ETH_RPC_URL=https://ethereum-rpc.publicnode.com \
bash scripts/run-vegeta-s3-characterization.sh --fetch-code
```

The code phase is resumable through `characterization/code-cache.json`; after every successful
`eth_getCode` call the cache is atomically updated. By default each destination is queried at the
block where it first appears in the S3 corpus rather than against today's chain state. Use
`--code-block N` only when a single fixed historical snapshot is desired.

Runtime-code SHA-256 is used only as an equality fingerprint: equal bytecode belongs to one family.
It is **not** Ethereum's Keccak code hash. The base code-family view detects canonical EIP-1167
minimal proxies but intentionally keeps storage-owner and implementation families separate; use the
proxy-resolution pass below before selecting native source/profile families.

## Conflict-weighted native-port candidate ranking

Direct-destination and bytecode-family popularity is not enough to choose a faithful native port:
S3 transactions frequently reach important state through routers and other internal calls. Extend the
characterization with Geth's built-in `callTracer` and fetch code for every relevant address
(direct destination, storage owner, or callTracer callee):

```bash
ETH_RPC_URL=https://eth.drpc.org \
bash scripts/run-vegeta-s3-characterization.sh \
  --fetch-calls \
  --fetch-code \
  --native-port-candidates
```

`--native-port-candidates` forces `--code-scope relevant`. Call traces are cached one block per file
under `characterization/call-cache/`; a rerun validates block/transaction identity and reuses every
completed block. `code-cache.json` remains address based, so addresses fetched by the earlier direct
code-family pass are reused and only newly discovered storage owners/internal callees require
`eth_getCode`.

The additional outputs are:

- `characterization/native-port-candidates.json`
- `characterization/native-port-candidates.txt`

The ranking is deliberately conflict-first. For each canonical storage key, same-block conflicting
transaction pairs are attributed to the storage-owning address and then grouped by that address's
historical runtime-bytecode SHA-256. Candidate families are ranked by the number of **unique
conflict pairs covered**. The report separately includes root/internal call counts, selectors, and
transaction coverage from `callTracer`; those invocation statistics are not treated as proof that a
particular call frame executed a particular SLOAD/SSTORE.

Geth `callTracer` exposes `DELEGATECALL` code targets, so the report also records delegatecall family
edges as implementation candidates. This base report intentionally does **not** rewrite a
proxy/storage-owner family to an implementation family; use the proxy-resolved mapping pass below
for standard EIP-1167/EIP-1967 cases.

## Proxy-resolved native source/profile-family mapping

The conflict-weighted report still ranks the bytecode attached to the storage-owning address. For a
proxy, however, that address owns the state while another implementation's source is the code that
must receive the LLM symbolic analysis. Resolve the standard cases with:

```bash
ETH_RPC_URL=https://eth.drpc.org \
bash scripts/run-vegeta-s3-characterization.sh \
  --fetch-calls \
  --fetch-code \
  --native-family-mapping-candidates
```

`--native-family-mapping-candidates` implies `--native-port-candidates` and forces relevant code
scope. The existing call/code caches are reused. The new pass probes only storage owners that caused
at least one same-block conflict and caches historical EIP-1967 implementation/beacon slots in:

- `characterization/proxy-resolution-cache.json`

A non-zero EIP-1967 implementation slot or exact canonical EIP-1167 runtime structure may rewrite
the **recommended source-analysis/profile family** to the implementation runtime-code family. The
storage owner remains the storage namespace; the tool never merges proxy instance state with the
implementation address. If an implementation address was not already present in `code-cache.json`,
its historical runtime code is fetched and added resumably.

The finalized mapping outputs are:

- `characterization/native-family-mapping-candidates.json`
- `characterization/native-family-mapping-candidates.txt`

Generic `DELEGATECALL` edges without structural EIP-1167/EIP-1967 evidence are deliberately not
rewritten. They remain `observed-delegatecall-candidate` records because an arbitrary contract can
delegate to a plugin/library and the independent storage trace cannot identify which frame caused a
particular SLOAD/SSTORE. EIP-1967 beacon addresses are reported but not dereferenced automatically.

The mapping report regroups unique conflict pairs by the recommended profile family, gives cumulative
coverage and the minimum number of profile families needed to reach 90%, 95%, and 98% of the observed
conflict pairs, and emits selector-set interface hints (for example fungible-token-like,
wrapped-native-token-like, constant-product-AMM-pair-like, or NFT-like). These hints are triage only;
contract identity and the final SymbGraph artifacts still require real source inspection and genuine
LLM symbolic analysis.


## Source/interface dossier and provisional native-family map

Once the proxy-resolved mapping is available, freeze a conflict-coverage target and inspect a
tractable set of representative Ethereum source/profile families before writing native contracts.
The default target is 95%, which selects the minimum family count reported by
`native-family-mapping-candidates.json` for that target:

```bash
bash scripts/run-vegeta-s3-native-family-dossier.sh
```

The offline pass emits a deterministic skeleton from the existing characterization only:

- `characterization/native-family-dossier.json`
- `characterization/native-family-dossier.txt`
- `characterization/native-family-map.json`

The dossier keeps the proxy/storage distinction explicit. For a structurally resolved EIP-1167 or
EIP-1967 owner, the representative **source address** is the implementation while the representative
**storage owner** remains the proxy address. Direct-code and unresolved generic-DELEGATECALL cases
continue to use the storage-owning address as their source-inspection starting point.

Fetch verified Ethereum source/ABI evidence for the selected representative addresses with:

```bash
bash scripts/run-vegeta-s3-native-family-dossier.sh --fetch-source
```

Source lookup uses Sourcify API v2 first. If the representative address is not available there and
`ETHERSCAN_API_KEY` is set, the Etherscan API v2 `getsourcecode` endpoint is used as a fallback.
Lookup payloads and normalized summaries are cached atomically in:

- `characterization/source-resolution-cache.json`

The generated `native-family-map.json` is intentionally **provisional**. Selector-set hints may
recommend `cw20-base`, `wrapped-native-token`, `astroport-pair`, or `cw721-base`; unclassified
families remain `manual-review`. The file deliberately leaves `native_code_family`,
`native_contract_source`, and `symbolic_analysis` unset. Those fields must be filled only after real
source inspection, choosing the native CosmWasm implementation, and producing the genuine LLM
symbolic-analysis artifact. Multiple Ethereum profile families may eventually reuse one native code
family, but their storage-owner instances must remain distinct state namespaces.

## Frozen native S3 translation plan (pre-execution)

After the 95%-coverage source/interface dossier is reviewed, use the checked-in
`evaluation/vegeta/s3-native-family-map.v1.json` rather than regenerating native archetype guesses.
The frozen map assigns the eleven selected Ethereum profile families to seven native CosmWasm code
families:

- `cw20-base`
- `controlled-cw20`
- `fee-token-cw20`
- `astroport-pair`
- `wrapped-native-token`
- `cw721-mintable`
- `xen-like`

This is a **source-family coverage map**, not a claim that a native translation already reproduces
95.45% of Ethereum conflict pairs. The native topology must be measured after the real CosmWasm
contracts execute.

Build and validate the full 101-block pre-execution plan with:

```bash
bash scripts/run-vegeta-s3-native-translation-evaluation.sh
```

The planner requires the already-generated S3 `code-cache.json`, `call-cache/`, and
`native-family-mapping-candidates.json`. It retains every transaction in its original block/order.
Mapped `CALL`/`STATICCALL` frames become semantic native actions. `DELEGATECALL`/`CALLCODE` frames
inherit the caller's storage namespace and are represented as inlined implementation/helper actions.
The planner also recognizes state-free/basic EVM behavior that does not need a CosmWasm symbolic
profile: historical precompile calls become deterministic system helpers, successful calls to
historically empty-code addresses with non-zero value become bank-transfer actions, and zero-value
empty-code calls become explicit no-op system actions. Remaining unsupported long-tail frames stay
as `background-fallback`; nothing is dropped.

Outputs are written under `benchmarks/corpora/vegeta-ethereum/s3/native-plan/`:

- `native-plan.jsonl` -- one native translation record per source block;
- `native-instance-catalog.json` -- one distinct native instance namespace per mapped Ethereum
  storage owner, even when several owners reuse one native code family;
- `translation-coverage.{json,txt}` -- transaction/call/storage/source-conflict coverage, explicit
  system-action counts, block-balanced conflict coverage, semantic transaction classes, and the
  implementation readiness gate;
- `preexecution-gate-report.{json,txt}` -- measured aggregate/block-balanced/semantic-volume gate inputs and currently configured thresholds;
- `validation-report.{json,txt}` -- retention/order/leakage/integrity checks;
- `manifest.json` -- provenance and family-map hash.

The planner deliberately does **not** copy historical `reads`/`writes` into `native-plan.jsonl`.
Concrete access sets are used only inside the offline coverage computation. This prevents the native
plan from becoming an oracle side channel into SymbGraph prediction.

At this stage validation is expected to pass while `native execution ready: no`: the seven native
source slots and their genuine LLM symbolic-analysis artifacts have not been implemented yet. Once
all fourteen source/profile paths are populated, enforce the production gate with:

```bash
python3 scripts/vegeta/validate-native-s3-plan.py \
  --require-execution-ready
```

Only after that gate passes should the translated workload be wired into the seven-strategy runtime.
The subsequent evaluation must report native conflict-pair precision/recall and critical-chain
fidelity; the 95.45% source-family coverage number must not be substituted for those measurements.


### Block-balanced and background-gap evaluation

Aggregate conflict-pair coverage can be dominated by a small number of unusually hot blocks, so the
pre-execution report also computes block-balanced source-conflict metrics over conflict-bearing
blocks: median/p10/p25 coverage, counts above 50/75/90/95%, and the share of source conflicts in the
hottest 1/5/10 blocks. Transaction coverage is split into `fully-semantic`,
`mixed-semantic-fallback`, and `background-only` instead of counting any transaction with one mapped
frame as fully translated.

The stronger gate mechanism is configured by:

- `evaluation/vegeta/s3-native-preexecution-gates.v1.json`

After reviewing the system-action/background-gap measurements, the pre-execution policy is frozen:
aggregate source-conflict coverage must be at least 95%, median conflict-bearing-block coverage at
least 80%, semantic transaction coverage at least 75%, and semantic call-frame coverage at least 50%.
The first two measurements come directly from `translation-coverage.json`; the semantic-volume gates
are evaluated against the selector-granular final mapping simulation described below. These are
workload-fidelity gates, not performance thresholds, and should not be retuned after SymbGraph
performance is observed.

Rank the remaining semantic background gap with:

```bash
bash scripts/run-vegeta-s3-background-gap.sh
```

or source-resolve the highest-impact remaining runtime-code families with:

```bash
bash scripts/run-vegeta-s3-background-gap.sh --fetch-source
```

The gap dossier groups only remaining `background-fallback` frames by runtime bytecode family and
ranks them greedily by marginal recovery of `background-only` transactions (fallback-frame count is
a tie-breaker). It emits:

- `native-plan/background-gap-dossier.{json,txt}`;
- `native-plan/background-family-fold-candidates.json`.

Verified ABI evidence may nominate a long-tail family for reuse as an existing `cw20-base`,
`controlled-cw20`, `cw721-mintable`, `astroport-pair`, `wrapped-native-token`, or `xen-like` family.
Router/helper-shaped contracts are explicitly reported as manual/new-family candidates rather than
being forced into one of the seven. These are review candidates only: verified interface similarity
does not automatically establish storage semantics or authorize changing the frozen family map.

`run-vegeta-s3-native-translation-evaluation.sh` now runs the offline gap dossier automatically. Set
`VEGETA_S3_BACKGROUND_FETCH_SOURCE=1` to enable source lookup during that evaluation. Source results
are resumably cached as `characterization/background-source-resolution-cache.json`.

### Selector-granular finalization and background proxy resolution

Do not apply `background-family-fold-candidates.json` as a whole-family rewrite. A verified contract
may expose a standard ERC20/ERC721 surface *and* application-specific staking, governance, minting,
or marketplace methods. The final reconnaissance pass therefore maps only source-verified
`(runtime bytecode family, selector)` entrypoints whose ABI semantics are compatible with the target
native archetype. Unsupported selectors on the same Ethereum contract stay explicit fallback.

Run the offline selector-granular simulation with:

```bash
bash scripts/run-vegeta-s3-finalize-native-map.sh
```

For high-impact background families identified as proxies, structurally probe their historical
EIP-1967 implementation/beacon slots and source-resolve newly discovered implementation addresses:

```bash
ETH_RPC_URL=<archive-rpc> \
  bash scripts/run-vegeta-s3-finalize-native-map.sh \
  --fetch-proxies \
  --fetch-source
```

Exact EIP-1167 runtime structure and non-zero EIP-1967 implementation slots may redirect the source
semantics used for a proxy instance. Generic `DELEGATECALL` edges remain diagnostics only. Shared
proxy bytecode does not imply shared implementation: selector rules are address-scoped whenever
individual proxy instances resolve differently, and the proxy/storage address remains the native
state namespace.

The finalizer can reuse the seven frozen native families entrypoint-by-entrypoint and may propose
three additional **candidate** archetypes when verified source supports them:

- `cw1155-like` for ERC-1155 transfer/balance/approval entrypoints;
- `marketplace-router` for verified Seaport-like marketplace composition entrypoints;
- `operator-filter-helper` for verified operator-filter registry/helper calls.

These three are simulation candidates only; they are not execution-ready contract implementations.
The finalizer also emits an explicit diagnostic for the highest-ranked still-unidentified fallback
family instead of guessing its semantics. For Vegeta S3 specifically, the diagnosed rank-1 runtime
family has no verified source/ABI. A narrow address-scoped exception is therefore fail-closed and
call-shape based: empty-calldata positive-value root calls with no descendants are represented as
`system::custodial_value_deposit`; the two observed zero-value batch-dispatch selectors are represented
only as composition-root system actions when their descendants match native-value sends or ERC20
`transfer` calls respectively. These rules do not infer function names, hidden state, or full contract
equivalence, and they are omitted if the audited shape changes.

Outputs under `native-plan/` are:

- `final-native-family-map.v2.json` -- base families plus selector-granular reuse/candidate rules;
- `selector-semantic-map.json` -- compact selector rules used by the simulator;
- `background-proxy-resolution.json` -- structural proxy-resolution records;
- `background-rank1-diagnostic.{json,txt}` -- call/value/selector composition of the largest
  remaining unidentified family;
- `final-mapping-simulation.{json,txt}` -- simulated semantic transaction/frame coverage and the
  four frozen gate measurements.

`run-vegeta-s3-native-translation-evaluation.sh` runs this finalization before validation. Set
`VEGETA_S3_FINAL_FETCH_PROXY=1` to enable structural proxy probes and
`VEGETA_S3_FINAL_FETCH_SOURCE=1` (or `VEGETA_S3_BACKGROUND_FETCH_SOURCE=1`) to source-resolve newly
found implementations. The validator consumes `final-mapping-simulation.json` for the 75% semantic
transaction and 50% semantic call-frame gates. If the compact selector map does not clear a frozen
gate, validation fails *before* any native Wasm is written.

The simulation is still not a native conflict-topology result. It uses public call/calldata/runtime
identity plus verified ABI/source metadata only; it does not embed or expose historical concrete
read/write sets. Native conflict precision/recall and critical-chain fidelity remain post-execution
measurements after real CosmWasm contracts and genuine LLM symbolic analyses exist.


## Native implementation + source-derived symbolic phase

After the selector-granular pre-execution gates are frozen, validate the concrete native workload
implementation with:

```bash
bash scripts/run-vegeta-s3-native-implementation-validation.sh
```

This phase checks ten real CosmWasm code families: the seven conflict-dominant families plus the
three selector-level semantic extensions (`cw1155-like`, `marketplace-router`, and
`operator-filter-helper`). The checked-in
`evaluation/vegeta/s3-native-implementation-manifest.v1.json` binds each family to its Cargo package,
source file, Wasm artifact and source-derived symbolic JSON.

The validator is deliberately fail-closed. It verifies each symbolic artifact's complete-source
SHA-256, line-scoped evidence, required profile set, declared storage resources and explicit denial
of historical trace-key inputs. It also checks that generated `final-native-family-map.v2.json` and
`selector-semantic-map.json` do not reference an unimplemented non-system family. The shell wrapper
then runs every native contract unit test, compiles every family to `wasm32-unknown-unknown`, parses
and normalizes all symbolic artifacts through `acg-symbolic-json`, and reruns the validator requiring
the Wasm artifacts to exist.

Successful implementation validation means the workload has concrete native state semantics and
source-derived SymbGraph profiles. It still does **not** claim native conflict-topology fidelity;
that requires executing the translated S3 workload and comparing the resulting native concrete
accesses with the source-trace topology.

## Native S3 atomic bundle execution and topology fidelity

After `run-vegeta-s3-native-implementation-validation.sh` passes, execute the translated workload
against the real native-family Wasm contracts with:

```bash
bash scripts/run-vegeta-s3-native-execution.sh
```

The execution stage compiles `native-plan.jsonl` plus the verified selector map into atomic
per-Ethereum-transaction bundles. The CosmWasm engine executes each bundle on one transactional
overlay: execute calls retain the historical frame caller, smart queries observe earlier writes in
the same bundle, bank sends share the same atomic transaction, and any source-reverted Ethereum
transaction is executed without committing its native write set. Background actions that still have
no defensible semantic mapping remain explicit skips rather than being assigned fabricated storage.

Historical EVM concrete read/write keys are **not** used to build or execute the native bundles.
They are loaded only after native execution by `measure-native-s3-fidelity.py`, which compares the
independently observed CosmWasm access trace against the source trace at transaction-pair level.
The report includes conflict-pair precision/recall/F1, an ordered conflict-DAG critical-path sum,
and the Vegeta-compatible per-block hot-key-chain sum. Amounts are deliberately normalized to a
bounded `Uint128` domain and historical balances/allowances are deterministically primed outside
the measured 101 blocks; this preserves storage-key/control-path topology without claiming exact
Ethereum value-state reconstruction.

Outputs are written to `benchmarks/corpora/vegeta-ethereum/s3/native-execution/`:

- `execution-manifest.json` / `execution-plan.jsonl`: executable bundles and out-of-measurement priming;
- `native-accesses.jsonl`: concrete access traces from all 101 serially executed native blocks;
- `native-topology-fidelity.json` / `.txt`: conflict precision/recall and critical-chain fidelity.

The fidelity report is a translated-workload result, not byte-for-byte EVM equivalence. In
particular, unsupported long-tail frames remain absent from native accesses and should appear as
false negatives rather than being hidden by oracle-key replay.


### Native execution token-ID normalization

Ethereum ERC-721/ERC-1155 token IDs are `uint256`, while the compact native S3 contracts use
`u64` map keys.  The execution preparer therefore uses a **per-instance dense bijection** from each
distinct observed source token ID to a native `u64`.  It never truncates or applies modulo arithmetic
to token IDs: equal source IDs remain equal and distinct source IDs within the same contract instance
remain distinct.  This avoids introducing artificial NFT/ERC1155 conflicts while keeping the native
contract key type bounded.  The execution manifest records the mapping policy and distinct-ID counts.


### Native execution allowance normalization

ERC20-style approvals use a separate normalization from transfer amounts.  A zero approval remains
zero, while any positive approval becomes the large `SEED` sentinel.  This is deliberate: applying
the transfer modulo independently to approvals can invert the source authorization relation
`allowance >= transferFrom amount` even for a canonically successful Ethereum transaction (notably
`uint256::MAX` approvals).  The sentinel preserves the allowance key, zero/revoke behavior, and
successful authorization control path without importing historical allowance values into the native
workload.  `transferFrom` continues to decrement the native allowance, so allowance reads/writes and
dependency keys remain real contract accesses.


### Native ERC721 ownership and authorization setup

ERC721 `transferFrom(address,address,uint256)` and three-argument `safeTransferFrom` both take
`(from, to, tokenId)`, so token identity is decoded from ABI word 2 before applying the collision-free
per-instance token-ID remap.  The first explicit source `from` address supplies the initial native
owner.  If the historical caller differs from that owner (for example a marketplace conduit), the
owner/operator approval is primed outside the measured workload.  The measured native transfer still
runs the real approval/operator checks and records their storage reads; later transfers do not rewrite
initial ownership during setup.


### Source-reverted native bundles

A transaction marked failed by the canonical S3 trace is replayed on a non-committing native overlay.
If one of its translated native calls also returns an error, the bundle now stops at that call and
retains the accesses observed up to and including the failure, all marked `reverted`.  This is an
expected source-revert outcome rather than a benchmark abort.  Successful source transactions remain
strict: any native error still fails the run.  `native-accesses.jsonl` records the failed native call
index, originating action ID, and error string when this occurs.

This distinction matters for transactions such as failed marketplace/NFT operations: forcing a
source-reverted call to succeed would fabricate an execution path, while dropping the transaction
would lose the dependency reads that led to the revert.


### Strict native failure diagnostics

Successful source transactions remain fail-closed.  When one fails natively, the engine now preserves
the exact bundle-call index in `EngineError::BundleCallFailed`, and the S3 executor reports the
translated call's native family, instance, sender, originating action ID, kind, message, and underlying
contract/VM error.  This is diagnostic-only and does not relax execution semantics; it avoids having
to infer which call in a multi-contract bundle diverged.


### RPC-backed native ERC721 initial state

Publication native-S3 runs now reconstruct recognized ERC721 logical state at the predecessor block
(`16774644` for S3) through standard high-level `eth_call` queries: `ownerOf(tokenId)`,
`getApproved(tokenId)`, and `isApprovedForAll(owner,operator)`.  The results are normalized into the
native contract's token-ID namespace and applied only during out-of-measurement priming.  No EVM
storage slots or concrete trace read/write keys are queried or copied.  The measured workload still
executes the real native ownership and approval checks.

This replaces the earlier "first transfer implies initial owner/operator approval" heuristic for
publication runs.  Responses are resumably cached in
`native-execution/evm-initial-state-cache.json`, so subsequent replays can be offline.  Set
`ETH_RPC_URL` to an archive-capable Ethereum endpoint for the first run.  The explicit
`VEGETA_S3_NATIVE_INITIAL_STATE_MODE=heuristic` fallback is retained only for diagnostics and is
recorded as such in the execution manifest.


### Caught internal EVM reverts

A successful Ethereum transaction may contain an internal `CALL`/`DELEGATECALL` that reverts while a
successful ancestor catches the failure.  The native execution plan now tags every translated action
under such a subtree with the **top-most failed callTracer action ID**.  The executor runs all
contiguous translated calls in that subtree against one nested transaction overlay cloned from the
outer transaction's current state.  The nested overlay sees prior successful writes and its own
in-scope writes, but is discarded at the end of the failed source scope.  Its concrete native access
records are retained with `reverted=true`; execution then resumes with the successful outer bundle.

This is distinct from a top-level source revert: successful source transactions remain strict outside
the explicitly traced failed subtrees.  The execution output records each reverted internal scope and
any native call at which that scope itself stopped.


### Exact EVM `msg.sender` provenance

Publication native-S3 execution distinguishes geth callTracer's raw frame `from` from the
callee-visible EVM `msg.sender`.  For ordinary `CALL`/`STATICCALL` frames they coincide, but
`DELEGATECALL` preserves the parent execution scope's `msg.sender` (EIP-7).  The native-plan builder
therefore records both `ethereum_caller` (the trace frame initiator, retained for provenance) and
`ethereum_msg_sender` (the value native contract execution must use).

The execution wrapper refreshes `native-plan.jsonl` from the local call-cache before each run and the
preparer rejects stale publication actions that lack `ethereum_msg_sender`.  The former raw-frame/
parent-context fallback remains available only through `VEGETA_S3_NATIVE_CALLER_MODE=heuristic` for
diagnostics and is rejected by the final execution validator.

This distinction is essential for proxy and implementation calls.  A proxy may appear as
callTracer's `from` on its `DELEGATECALL` into an implementation while the implementation still sees
the original user as `msg.sender`; feeding the proxy address into a native ERC721 authorization
check manufactures an `unauthorized` result that did not exist in the source execution.


### Fungible `totalSupply` execution compatibility

The native execution translator emits `query::total_supply` for ERC-20 selector `0x18160ddd`
across the fungible families. `controlled-cw20` now exposes its already-maintained `TOTAL_SUPPLY`
item through that query. `fee-token-cw20` now maintains a fixed supply initialized from its seed
balances and exposes the same query; fee-on-transfer operations redistribute balances without
changing supply. Their source-derived symbolic artifacts include the singleton initialization/read
and the implementation manifest requires the query profile, preventing translator/contract message
schema drift from reappearing.


### Native topology FP/FN attribution

`measure-native-s3-fidelity.py` now emits `native-topology-attribution.json` and
`native-topology-attribution.txt` after the aggregate topology report.  The executor preserves a
per-bundle-call access span and annotates each concrete native access with its originating family,
instance, semantic action, and source action ID.  This lets the report rank false-positive conflict
pairs by native family, instance, decoded storage namespace, semantic action, concrete key, and
block.

A conflict pair caused by multiple keys receives one unit of fractional credit split across those
keys, so the ranking is comparable to the total false-positive pair count rather than double
counting every multi-key conflict.  The report separately counts pair-key incidences and identifies
false-positive edges that participate in at least one native longest conflict-DAG path.  It also
computes a true-positive-only native critical path by removing all native-only edges, which isolates
how much the native critical path depends on false-positive edges.

False negatives are attributed to their source EVM storage owner/key and, when available, joined to
the Ethereum profile family using native-plan storage-context metadata.  All source concrete-key
attribution happens only after native execution; it is diagnostic and is never fed into the native
planner, executor, initial-state priming, or symbolic profiles.  Decoded CosmWasm resource names are
best-effort namespace labels, not semantic ground truth.


### Storage-comparable fidelity and native bank augmentation

The S3 source corpus used by this evaluation records EVM storage keys (touched storage as reads,
changed storage as writes). It does not provide a like-for-like account-balance key for native
CosmWasm bank state. Consequently, topology precision/recall use a strict storage-to-storage
comparison: source EVM storage versus native contract storage. Native bank accesses are not removed
from execution; they remain concrete dependencies and are reported separately as
`full_native_augmentation`, including additional pair count and critical-path delta.

This split was prompted by the first FP attribution run, where bank-ledger edges dominated the
reported false positives. Counting those edges as storage false positives conflates a measurement
domain mismatch with semantic translation error.

The wrapped-native-token implementation also no longer manufactures a contract-local
`TOTAL_SUPPLY` singleton. Its `total_supply` query derives supply from the contract's native
collateral balance, while deposit/withdraw mutate per-account wrapped balances and the native bank
ledger. The source-derived symbolic artifact is refreshed and explicitly notes that bank-ledger
dependencies are outside the current contract-local symbolic JSON schema.


### Exact SLOAD/SSTORE source ground truth via transaction-level tracing

Hosted RPCs may time out on `debug_traceBlockByNumber` with the JavaScript SLOAD/SSTORE tracer even
when the same tracer succeeds through `debug_traceTransaction`. The extractor therefore supports
`--trace-mode custom-js-tx`: it traces transactions sequentially in canonical block order, writes an
atomic checkpoint after every successful transaction, and reuses checkpoints only when both the
transaction hash and tracer-source SHA-256 match.

For publication ground truth, reconstruct into a separate directory rather than overwriting the
public-prestate corpus:

```bash
ETH_RPC_URL=https://YOUR_ARCHIVE_RPC \
bash scripts/run-vegeta-s3-exact-trace.sh
```

The default exact output is
`benchmarks/corpora/vegeta-ethereum/s3-exact-sload-sstore/`. A failed/rate-limited run can simply be
rerun; completed transaction traces are reused. `VEGETA_S3_EXACT_TX_DELAY` can pace requests for
hosted-provider limits.

The exact corpus records actual executed SLOAD/SSTORE keys (`evm-storage-sload-sstore-v1`) and
opcode-step counts. The existing `public-rpc` corpus remains a reproducibility fallback with
prestate-touched/state-changing semantics. Native execution can be re-evaluated against the exact
corpus without changing the frozen native family mapping:

```bash
VEGETA_S3_CORPUS=benchmarks/corpora/vegeta-ethereum/s3-exact-sload-sstore/corpus.jsonl \
ETH_RPC_URL="$ETH_RPC_URL" \
bash scripts/run-vegeta-s3-native-execution.sh
```

Source concrete keys remain evaluation-only ground truth; they are not consumed by symbolic
analysis, native planning, initial-state priming, or execution.
