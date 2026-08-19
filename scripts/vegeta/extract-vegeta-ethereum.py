#!/usr/bin/env python3
"""Extract Vegeta's Ethereum block corpus from archive-capable Ethereum RPC endpoints.

Three trace modes are supported:

* ``custom-js`` uses Geth's arbitrary JavaScript tracer interface at block granularity and records
  exact EVM ``SLOAD``/``SSTORE`` accesses plus opcode-step counts.
* ``custom-js-tx`` uses the same exact JavaScript tracer via ``debug_traceTransaction`` one
  transaction at a time. This avoids hosted-provider block-trace timeouts and durably checkpoints
  every completed transaction so long S3 reconstructions are resumable.
* ``public-rpc`` uses only Geth's built-in ``prestateTracer``. A normal prestate trace gives every
  storage slot touched while executing a transaction; a second trace with ``diffMode`` identifies
  storage slots whose values changed. The resulting ``reads`` are conservative touched-storage
  dependencies and ``writes`` are state-changing storage slots. Transaction receipts provide gas
  used and failure status, and gas used is retained as the deterministic compute-cost proxy.

The public-rpc mode deliberately records its weaker access semantics in the generated manifest. It
is intended to make Vegeta's public Ethereum block ranges reconstructible through hosted providers
without silently pretending that built-in tracing is identical to the custom SLOAD/SSTORE tracer.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
ROOT = SCRIPT_DIR.parents[1]
sys.path.insert(0, str(SCRIPT_DIR))

from vegeta_corpus import (  # noqa: E402
    SCHEMA_VERSION,
    S3_END_BLOCK,
    S3_EXPECTED_BLOCKS,
    S3_EXPECTED_LONGEST_CHAIN_SUM,
    S3_EXPECTED_RATIO,
    S3_EXPECTED_TRANSACTIONS,
    S3_START_BLOCK,
    compute_metrics,
    write_jsonl,
)

TRACE_MODE_CUSTOM_JS = "custom-js"
TRACE_MODE_CUSTOM_JS_TX = "custom-js-tx"
TRACE_MODE_PUBLIC_RPC = "public-rpc"


class RpcClient:
    def __init__(
        self,
        url: str,
        timeout: int,
        retries: int = 5,
        retry_backoff: float = 1.5,
    ):
        self.url = url
        self.timeout = timeout
        self.retries = retries
        self.retry_backoff = retry_backoff
        self.request_id = 0

    def call(self, method: str, params: list):
        last_error = None
        for attempt in range(self.retries + 1):
            self.request_id += 1
            payload = json.dumps(
                {"jsonrpc": "2.0", "id": self.request_id, "method": method, "params": params}
            ).encode()
            request = urllib.request.Request(
                self.url,
                data=payload,
                headers={"Content-Type": "application/json", "User-Agent": "symbgraphpool-vegeta-extractor/1"},
            )
            try:
                with urllib.request.urlopen(request, timeout=self.timeout) as response:
                    body = json.load(response)
            except urllib.error.HTTPError as error:
                detail = ""
                try:
                    detail = error.read().decode("utf-8", errors="replace")[:1000]
                except Exception:  # pragma: no cover - diagnostic only
                    pass
                last_error = RuntimeError(
                    f"RPC {method} HTTP {error.code}: {detail or error.reason}"
                )
                retryable = error.code in {408, 425, 429, 500, 502, 503, 504}
                if not retryable or attempt >= self.retries:
                    raise last_error from error
            except (urllib.error.URLError, TimeoutError) as error:
                last_error = RuntimeError(f"RPC {method} failed: {error}")
                if attempt >= self.retries:
                    raise last_error from error
            else:
                if body.get("error") is not None:
                    error = body["error"]
                    code = error.get("code") if isinstance(error, dict) else None
                    message = error.get("message") if isinstance(error, dict) else str(error)
                    rpc_error = RuntimeError(
                        f"RPC {method} returned error"
                        + (f" {code}" if code is not None else "")
                        + f": {message}"
                    )
                    # Hosted providers sometimes surface transient capacity/rate-limit failures as
                    # JSON-RPC errors instead of HTTP 429/5xx. Retry only the common server-error
                    # range; method-not-found/invalid-params failures should fail immediately.
                    if isinstance(code, int) and -32099 <= code <= -32000 and attempt < self.retries:
                        last_error = rpc_error
                    else:
                        raise rpc_error
                else:
                    return body.get("result")

            delay = self.retry_backoff * (2**attempt)
            print(
                f"RPC {method} attempt {attempt + 1} failed; retrying in {delay:.1f}s: {last_error}",
                file=sys.stderr,
                flush=True,
            )
            time.sleep(delay)
        assert last_error is not None
        raise last_error


def parse_quantity(value) -> int:
    if isinstance(value, int):
        return value
    if isinstance(value, str):
        return int(value, 16) if value.startswith("0x") else int(value)
    return 0


def normalize_trace_item(item: dict) -> tuple[str | None, dict]:
    if not isinstance(item, dict):
        raise RuntimeError(f"trace item is not an object: {type(item).__name__}")
    if item.get("error") is not None and "result" not in item:
        raise RuntimeError(f"trace item returned error: {item['error']}")
    if "result" in item:
        return item.get("txHash") or item.get("transactionHash"), item["result"]
    return item.get("txHash") or item.get("transactionHash"), item


def _normalize_hex_component(value: str, width: int, field: str) -> str:
    if not isinstance(value, str):
        raise RuntimeError(f"{field} is not a hex string: {value!r}")
    normalized = value.lower()
    if normalized.startswith("0x"):
        normalized = normalized[2:]
    if len(normalized) > width:
        raise RuntimeError(f"{field} is wider than {width} hex digits: {value}")
    try:
        int(normalized or "0", 16)
    except ValueError as error:
        raise RuntimeError(f"{field} is not hexadecimal: {value}") from error
    return normalized.rjust(width, "0")


def canonical_storage_key(address: str, slot: str) -> str:
    return (
        "evm/"
        + _normalize_hex_component(address, 40, "storage account")
        + "/"
        + _normalize_hex_component(slot, 64, "storage slot")
    )


def storage_keys_from_prestate(state: dict | None) -> set[str]:
    """Return canonical keys for all storage leaves represented in a prestate-style object."""

    keys: set[str] = set()
    if state is None:
        return keys
    if not isinstance(state, dict):
        raise RuntimeError(f"prestate result is not an object: {type(state).__name__}")
    for address, account in state.items():
        if not isinstance(account, dict):
            continue
        storage = account.get("storage") or {}
        if not isinstance(storage, dict):
            raise RuntimeError(f"prestate storage for {address} is not an object")
        for slot in storage:
            keys.add(canonical_storage_key(address, slot))
    return keys


def _validate_tx_hash(expected: str, observed: str | None, label: str, block_number: int, index: int):
    if observed is not None and observed.lower() != expected.lower():
        raise RuntimeError(
            f"block {block_number} tx {index}: {label} hash {observed} does not match "
            f"transaction {expected}"
        )


def build_public_trace_items(
    block: dict,
    touched_traces: list[dict],
    diff_traces: list[dict],
    receipts: list[dict],
) -> list[dict]:
    """Convert built-in prestate traces + receipts into the extractor's canonical trace shape.

    ``reads`` intentionally contains all touched storage slots. That is conservative because
    prestateTracer does not distinguish SLOAD from SSTORE. ``writes`` contains storage slots that
    changed according to diffMode. The manifest explicitly records these semantics.
    """

    transactions = block.get("transactions", [])
    block_number = parse_quantity(block.get("number", 0))
    counts = {
        "transactions": len(transactions),
        "prestate traces": len(touched_traces),
        "diff traces": len(diff_traces),
        "receipts": len(receipts),
    }
    if len(set(counts.values())) != 1:
        rendered = ", ".join(f"{name}={count}" for name, count in counts.items())
        raise RuntimeError(f"block {block_number}: public trace count mismatch ({rendered})")

    public_items = []
    for index, (tx, touched_item, diff_item, receipt) in enumerate(
        zip(transactions, touched_traces, diff_traces, receipts)
    ):
        tx_hash = tx["hash"].lower()
        touched_hash, touched = normalize_trace_item(touched_item)
        diff_hash, diff = normalize_trace_item(diff_item)
        receipt_hash = receipt.get("transactionHash") if isinstance(receipt, dict) else None
        _validate_tx_hash(tx_hash, touched_hash, "prestate trace", block_number, index)
        _validate_tx_hash(tx_hash, diff_hash, "diff trace", block_number, index)
        _validate_tx_hash(tx_hash, receipt_hash, "receipt", block_number, index)

        touched_keys = storage_keys_from_prestate(touched)
        if not isinstance(diff, dict):
            raise RuntimeError(f"block {block_number} tx {index}: diff trace is not an object")
        write_keys = storage_keys_from_prestate(diff.get("pre"))
        write_keys.update(storage_keys_from_prestate(diff.get("post")))

        gas_used = parse_quantity(receipt.get("gasUsed", 0))
        status = receipt.get("status")
        failed = status is not None and parse_quantity(status) == 0
        public_items.append(
            {
                "txHash": tx_hash,
                "result": {
                    "reads": sorted(touched_keys),
                    "writes": sorted(write_keys),
                    # Hosted built-in tracers do not expose an opcode count. Reuse gas used as a
                    # deterministic per-transaction compute-cost proxy; the manifest records this.
                    "steps": gas_used,
                    "gasUsed": gas_used,
                    "error": "reverted" if failed else "",
                },
            }
        )
    return public_items


def build_block_record(block: dict, traces: list[dict]) -> dict:
    transactions = block.get("transactions", [])
    if len(transactions) != len(traces):
        raise RuntimeError(
            f"block {parse_quantity(block['number'])}: {len(transactions)} transactions but "
            f"{len(traces)} traces"
        )

    records = []
    for index, (tx, trace_item) in enumerate(zip(transactions, traces)):
        trace_hash, trace = normalize_trace_item(trace_item)
        tx_hash = tx["hash"].lower()
        if trace_hash is not None and trace_hash.lower() != tx_hash:
            raise RuntimeError(
                f"block {parse_quantity(block['number'])} tx {index}: trace hash {trace_hash} "
                f"does not match transaction {tx_hash}"
            )
        data = tx.get("input") or "0x"
        selector = data[:10].lower() if len(data) >= 10 else "0x"
        records.append(
            {
                "tx_index": index,
                "tx_hash": tx_hash,
                "from": (tx.get("from") or "0x").lower(),
                "to": (tx.get("to") or "<create>").lower(),
                "selector": selector,
                "value": tx.get("value", "0x0"),
                "gas_limit": parse_quantity(tx.get("gas", "0x0")),
                "gas_used": parse_quantity(trace.get("gasUsed", 0)),
                "opcode_steps": int(trace.get("steps", 0)),
                "failed": bool(trace.get("error")),
                "reads": sorted(set(trace.get("reads", []))),
                "writes": sorted(set(trace.get("writes", []))),
            }
        )
    return {
        "schema_version": SCHEMA_VERSION,
        "block_number": parse_quantity(block["number"]),
        "block_hash": block["hash"].lower(),
        "timestamp": parse_quantity(block.get("timestamp", "0x0")),
        "transactions": records,
    }


def fetch_block_receipts(client: RpcClient, tag: str, block: dict) -> list[dict]:
    """Fetch receipts efficiently, falling back to per-transaction RPC when necessary."""

    try:
        receipts = client.call("eth_getBlockReceipts", [tag])
    except RuntimeError as error:
        print(
            f"eth_getBlockReceipts unavailable for {tag}; falling back to "
            f"eth_getTransactionReceipt ({error})",
            file=sys.stderr,
            flush=True,
        )
        receipts = [
            client.call("eth_getTransactionReceipt", [tx["hash"]])
            for tx in block.get("transactions", [])
        ]
    if receipts is None:
        raise RuntimeError(f"Ethereum RPC returned no receipts for block {tag}")
    return receipts


def trace_block_custom_js(client: RpcClient, tag: str, tracer: str, timeout: int) -> list[dict]:
    return client.call(
        "debug_traceBlockByNumber",
        [tag, {"tracer": tracer, "timeout": f"{timeout}s"}],
    )


def _tx_trace_cache_path(cache_dir: Path, block_number: int, index: int, tx_hash: str) -> Path:
    short_hash = tx_hash.lower().removeprefix("0x")[:16]
    return cache_dir / str(block_number) / f"{index:04d}-{short_hash}.json"


def _load_cached_tx_trace(path: Path, tx_hash: str, tracer_sha256: str) -> dict | None:
    try:
        item = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    if item.get("tx_hash", "").lower() != tx_hash.lower():
        return None
    if item.get("tracer_sha256") != tracer_sha256:
        return None
    result = item.get("result")
    return result if isinstance(result, dict) else None


def _write_cached_tx_trace(
    path: Path,
    tx_hash: str,
    tracer_sha256: str,
    result: dict,
) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = {
        "schema_version": 1,
        "trace_mode": TRACE_MODE_CUSTOM_JS_TX,
        "tx_hash": tx_hash.lower(),
        "tracer_sha256": tracer_sha256,
        "result": result,
    }
    temp = path.with_suffix(path.suffix + ".tmp")
    temp.write_text(json.dumps(payload, sort_keys=True) + "\n", encoding="utf-8")
    temp.replace(path)


def load_fallback_transactions(
    corpus_path: Path,
    tx_hashes: set[str],
) -> tuple[dict[str, dict], dict]:
    """Load explicitly named fallback transactions from a previously frozen corpus.

    Every requested hash must exist exactly once. The sibling manifest is required so a hybrid
    corpus records the fallback semantics instead of silently presenting those accesses as exact
    SLOAD/SSTORE observations.
    """

    wanted = {h.lower() for h in tx_hashes}
    if not wanted:
        return {}, {}

    manifest_path = corpus_path.parent / "manifest.json"
    if not corpus_path.exists():
        raise RuntimeError(f"fallback corpus does not exist: {corpus_path}")
    if not manifest_path.exists():
        raise RuntimeError(
            f"fallback corpus manifest does not exist: {manifest_path}; "
            "fallback semantics must be explicit"
        )

    fallback_manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    found: dict[str, dict] = {}
    provenance: dict[str, dict] = {}

    for line in corpus_path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        block = json.loads(line)
        block_number = int(block["block_number"])
        for tx in block.get("transactions") or []:
            tx_hash = str(tx.get("tx_hash") or "").lower()
            if tx_hash not in wanted:
                continue
            if tx_hash in found:
                raise RuntimeError(f"fallback transaction appears more than once: {tx_hash}")
            found[tx_hash] = {
                "reads": sorted(set(tx.get("reads") or [])),
                "writes": sorted(set(tx.get("writes") or [])),
                "steps": int(tx.get("opcode_steps") or tx.get("gas_used") or 0),
                "gasUsed": int(tx.get("gas_used") or 0),
                "error": "reverted" if bool(tx.get("failed")) else "",
            }
            provenance[tx_hash] = {
                "tx_hash": tx_hash,
                "block_number": block_number,
                "tx_index": int(tx.get("tx_index", -1)),
                "source_corpus": str(corpus_path),
                "source_manifest": str(manifest_path),
                "access_semantics": fallback_manifest.get("access_semantics"),
                "trace_mode": fallback_manifest.get("trace_mode"),
                "compute_proxy": fallback_manifest.get("compute_proxy"),
            }

    missing = sorted(wanted - set(found))
    if missing:
        raise RuntimeError(
            "fallback transaction hash(es) not found in corpus: " + ", ".join(missing)
        )
    return found, provenance


def trace_block_custom_js_transactions(
    client: RpcClient,
    block: dict,
    tracer: str,
    timeout: int,
    cache_dir: Path | None = None,
    resume: bool = False,
    tx_delay: float = 0.0,
    fallback_traces: dict[str, dict] | None = None,
) -> list[dict]:
    """Trace one block transaction-by-transaction with exact SLOAD/SSTORE semantics.

    Each successful transaction is atomically checkpointed before the next RPC request. If a hosted
    provider times out or rate-limits later in the block, rerunning with ``--resume`` reuses only
    cache entries generated by the same tracer source hash.
    """

    transactions = block.get("transactions", [])
    block_number = parse_quantity(block.get("number", 0))
    tracer_sha256 = hashlib.sha256(tracer.encode("utf-8")).hexdigest()
    traces: list[dict] = []
    fallback_traces = fallback_traces or {}

    for index, tx in enumerate(transactions):
        tx_hash = tx["hash"].lower()
        cache_path = (
            _tx_trace_cache_path(cache_dir, block_number, index, tx_hash)
            if cache_dir is not None
            else None
        )
        result = None
        if resume and cache_path is not None and cache_path.exists():
            result = _load_cached_tx_trace(cache_path, tx_hash, tracer_sha256)
            if result is not None:
                print(
                    f"[{block_number}] tx {index + 1}/{len(transactions)} reuse {tx_hash}",
                    flush=True,
                )

        if result is None and tx_hash in fallback_traces:
            result = fallback_traces[tx_hash]
            print(
                f"[{block_number}] tx {index + 1}/{len(transactions)} "
                f"fallback frozen-corpus {tx_hash}",
                flush=True,
            )

        if result is None:
            print(
                f"[{block_number}] tx {index + 1}/{len(transactions)} trace {tx_hash}",
                flush=True,
            )
            result = client.call(
                "debug_traceTransaction",
                [tx_hash, {"tracer": tracer, "timeout": f"{timeout}s"}],
            )
            if not isinstance(result, dict):
                raise RuntimeError(
                    f"block {block_number} tx {index}: debug_traceTransaction returned "
                    f"{type(result).__name__}, expected object"
                )
            if cache_path is not None:
                _write_cached_tx_trace(cache_path, tx_hash, tracer_sha256, result)
            if tx_delay:
                time.sleep(tx_delay)

        traces.append({"txHash": tx_hash, "result": result})

    return traces


def trace_block_public_rpc(client: RpcClient, tag: str, block: dict) -> list[dict]:
    # Built-in tracers are accepted by hosted providers that reject arbitrary JavaScript tracers.
    # Disable code to reduce response size; storage must stay enabled.
    touched = client.call(
        "debug_traceBlockByNumber",
        [
            tag,
            {
                "tracer": "prestateTracer",
                "tracerConfig": {"disableCode": True, "disableStorage": False},
            },
        ],
    )
    diff = client.call(
        "debug_traceBlockByNumber",
        [
            tag,
            {
                "tracer": "prestateTracer",
                "tracerConfig": {
                    "diffMode": True,
                    "disableCode": True,
                    "disableStorage": False,
                },
            },
        ],
    )
    receipts = fetch_block_receipts(client, tag, block)
    if touched is None or diff is None:
        raise RuntimeError(f"Ethereum RPC returned no built-in prestate traces for block {tag}")
    return build_public_trace_items(block, touched, diff, receipts)


def manifest_trace_metadata(trace_mode: str) -> tuple[str, str, str]:
    if trace_mode == TRACE_MODE_CUSTOM_JS:
        return (
            "evm-storage-sload-sstore-v1",
            "geth debug_traceBlockByNumber + custom JavaScript SLOAD/SSTORE tracer",
            "opcode_steps",
        )
    if trace_mode == TRACE_MODE_CUSTOM_JS_TX:
        return (
            "evm-storage-sload-sstore-v1",
            "geth debug_traceTransaction + custom JavaScript SLOAD/SSTORE tracer",
            "opcode_steps",
        )
    if trace_mode == TRACE_MODE_PUBLIC_RPC:
        return (
            "evm-storage-prestate-touched+state-changing-writes-v1",
            "debug_traceBlockByNumber + built-in prestateTracer (normal + diffMode) + receipts",
            "gas_used",
        )
    raise ValueError(trace_mode)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rpc-url", default=os.environ.get("ETH_RPC_URL"))
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--start-block", type=int, default=S3_START_BLOCK)
    parser.add_argument("--end-block", type=int, default=S3_END_BLOCK)
    parser.add_argument(
        "--trace-mode",
        choices=[TRACE_MODE_CUSTOM_JS, TRACE_MODE_CUSTOM_JS_TX, TRACE_MODE_PUBLIC_RPC],
        default=TRACE_MODE_PUBLIC_RPC,
        help=(
            "public-rpc uses hosted-provider-compatible built-in prestateTracer calls; "
            "custom-js uses an exact block-level JavaScript SLOAD/SSTORE tracer; "
            "custom-js-tx uses the same exact tracer one transaction at a time with checkpointing"
        ),
    )
    parser.add_argument("--timeout", type=int, default=600)
    parser.add_argument("--rpc-retries", type=int, default=5)
    parser.add_argument("--retry-backoff", type=float, default=1.5)
    parser.add_argument(
        "--tx-delay",
        type=float,
        default=0.0,
        help="seconds to sleep after each fresh custom-js-tx RPC call (useful for hosted rate limits)",
    )
    parser.add_argument(
        "--tx-cache-dir",
        type=Path,
        default=None,
        help="custom-js-tx checkpoint directory (default: <output-dir>/tx-traces)",
    )
    parser.add_argument(
        "--fallback-corpus",
        type=Path,
        default=None,
        help=(
            "previously frozen corpus used only for explicitly named --fallback-tx-hash "
            "transactions"
        ),
    )
    parser.add_argument(
        "--fallback-tx-hash",
        action="append",
        default=[],
        help=(
            "transaction hash to source from --fallback-corpus instead of custom tracing; "
            "repeat for multiple explicit exceptions"
        ),
    )
    parser.add_argument("--resume", action="store_true")
    parser.add_argument(
        "--probe-only",
        action="store_true",
        help="trace only --start-block and validate provider compatibility without writing a corpus",
    )
    args = parser.parse_args()
    if not args.rpc_url:
        parser.error("--rpc-url or ETH_RPC_URL is required")
    if args.end_block < args.start_block:
        parser.error("--end-block must be >= --start-block")
    if args.rpc_retries < 0:
        parser.error("--rpc-retries must be >= 0")
    if args.retry_backoff < 0:
        parser.error("--retry-backoff must be >= 0")
    if args.tx_delay < 0:
        parser.error("--tx-delay must be >= 0")

    tracer = None
    if args.trace_mode in {TRACE_MODE_CUSTOM_JS, TRACE_MODE_CUSTOM_JS_TX}:
        tracer = (SCRIPT_DIR / "geth-rw-tracer.js").read_text(encoding="utf-8")
    client = RpcClient(
        args.rpc_url,
        args.timeout,
        retries=args.rpc_retries,
        retry_backoff=args.retry_backoff,
    )
    tx_cache_dir = (
        args.tx_cache_dir
        if args.tx_cache_dir is not None
        else args.output_dir / "tx-traces"
    )
    fallback_hashes = {h.lower() for h in args.fallback_tx_hash}
    if fallback_hashes and args.trace_mode != TRACE_MODE_CUSTOM_JS_TX:
        parser.error("--fallback-tx-hash is supported only with --trace-mode custom-js-tx")
    if fallback_hashes and args.fallback_corpus is None:
        parser.error("--fallback-corpus is required when --fallback-tx-hash is used")
    fallback_traces, fallback_provenance = (
        load_fallback_transactions(args.fallback_corpus, fallback_hashes)
        if fallback_hashes
        else ({}, {})
    )

    if args.probe_only:
        tag = hex(args.start_block)
        print(f"probe [{args.start_block}] fetch block + {args.trace_mode} trace", flush=True)
        block = client.call("eth_getBlockByNumber", [tag, True])
        if block is None:
            raise RuntimeError(f"Ethereum RPC has no block {args.start_block}")
        if args.trace_mode == TRACE_MODE_CUSTOM_JS:
            traces = trace_block_custom_js(client, tag, tracer, args.timeout)
        elif args.trace_mode == TRACE_MODE_CUSTOM_JS_TX:
            traces = trace_block_custom_js_transactions(
                client,
                block,
                tracer,
                args.timeout,
                cache_dir=tx_cache_dir,
                resume=args.resume,
                tx_delay=args.tx_delay,
                fallback_traces=fallback_traces,
            )
        else:
            traces = trace_block_public_rpc(client, tag, block)
        record = build_block_record(block, traces)
        print(
            json.dumps(
                {
                    "probe": "ok",
                    "trace_mode": args.trace_mode,
                    "block_number": record["block_number"],
                    "transactions": len(record["transactions"]),
                    "storage_accesses": sum(
                        len(tx["reads"]) + len(tx["writes"]) for tx in record["transactions"]
                    ),
                },
                indent=2,
                sort_keys=True,
            )
        )
        return 0

    blocks_dir = args.output_dir / "blocks"
    blocks_dir.mkdir(parents=True, exist_ok=True)

    for number in range(args.start_block, args.end_block + 1):
        output = blocks_dir / f"{number}.json"
        if args.resume and output.exists():
            print(f"[{number}] reuse {output}")
            continue
        print(f"[{number}] fetch block + {args.trace_mode} trace", flush=True)
        tag = hex(number)
        block = client.call("eth_getBlockByNumber", [tag, True])
        if block is None:
            raise RuntimeError(f"Ethereum RPC has no block {number}")
        if args.trace_mode == TRACE_MODE_CUSTOM_JS:
            traces = trace_block_custom_js(client, tag, tracer, args.timeout)
        elif args.trace_mode == TRACE_MODE_CUSTOM_JS_TX:
            traces = trace_block_custom_js_transactions(
                client,
                block,
                tracer,
                args.timeout,
                cache_dir=tx_cache_dir,
                resume=args.resume,
                tx_delay=args.tx_delay,
                fallback_traces=fallback_traces,
            )
        else:
            traces = trace_block_public_rpc(client, tag, block)
        record = build_block_record(block, traces)
        temp = output.with_suffix(".json.tmp")
        temp.write_text(json.dumps(record, sort_keys=True) + "\n", encoding="utf-8")
        temp.replace(output)

    blocks = [
        json.loads((blocks_dir / f"{number}.json").read_text(encoding="utf-8"))
        for number in range(args.start_block, args.end_block + 1)
    ]
    corpus_path = args.output_dir / "corpus.jsonl"
    write_jsonl(corpus_path, blocks)
    metrics = compute_metrics(blocks)
    access_semantics, extractor, compute_proxy = manifest_trace_metadata(args.trace_mode)
    manifest = {
        "schema_version": SCHEMA_VERSION,
        "dataset": "vegeta-s3"
        if (args.start_block, args.end_block) == (S3_START_BLOCK, S3_END_BLOCK)
        else "vegeta-custom",
        "source": {
            "paper": "Vegeta: Enabling Parallel Smart Contract Execution in Leaderless Blockchains, NSDI 2025",
            "paper_dataset_tag": "S3"
            if (args.start_block, args.end_block) == (S3_START_BLOCK, S3_END_BLOCK)
            else None,
            "block_range": [args.start_block, args.end_block],
        },
        "trace_mode": args.trace_mode,
        "trace_granularity": (
            "transaction" if args.trace_mode == TRACE_MODE_CUSTOM_JS_TX else "block"
        ),
        "access_semantics": (
            access_semantics
            if not fallback_provenance
            else access_semantics + "+explicit-fallback-exceptions"
        ),
        "extractor": (
            extractor
            if not fallback_provenance
            else extractor + " + explicit frozen-corpus transaction fallback(s)"
        ),
        "compute_proxy": (
            compute_proxy
            if not fallback_provenance
            else compute_proxy + "+explicit-fallback-exceptions"
        ),
        "trace_semantics_exceptions": [
            fallback_provenance[h] for h in sorted(fallback_provenance)
        ],
        "custom_tracer_sha256": (
            hashlib.sha256(tracer.encode("utf-8")).hexdigest() if tracer is not None else None
        ),
        "transaction_trace_cache": (
            str(tx_cache_dir.relative_to(args.output_dir))
            if args.trace_mode == TRACE_MODE_CUSTOM_JS_TX
            and tx_cache_dir.is_relative_to(args.output_dir)
            else str(tx_cache_dir) if args.trace_mode == TRACE_MODE_CUSTOM_JS_TX else None
        ),
        "public_rpc_caveat": (
            "reads are all storage slots touched by prestateTracer; writes are storage slots whose "
            "state changed in diffMode. This is conservative and is not byte-for-byte equivalent "
            "to the custom SLOAD/SSTORE tracer."
            if args.trace_mode == TRACE_MODE_PUBLIC_RPC
            else None
        ),
        "fallback_caveat": (
            "The listed trace_semantics_exceptions are explicit transaction-level fallbacks from "
            "the frozen fallback corpus. They are retained rather than dropped or fabricated and "
            "must not be described as exact SLOAD/SSTORE observations."
            if fallback_provenance
            else None
        ),
        "paper_targets": {
            "blocks": S3_EXPECTED_BLOCKS,
            "transactions": S3_EXPECTED_TRANSACTIONS,
            "longest_chain_sum": S3_EXPECTED_LONGEST_CHAIN_SUM,
            "ratio": S3_EXPECTED_RATIO,
        },
        "observed": metrics,
        "corpus": "corpus.jsonl",
    }
    (args.output_dir / "manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(json.dumps(metrics, indent=2, sort_keys=True))
    print(f"wrote {corpus_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
