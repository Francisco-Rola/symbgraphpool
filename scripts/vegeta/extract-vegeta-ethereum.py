#!/usr/bin/env python3
"""Extract Vegeta's Ethereum block corpus from archive-capable Ethereum RPC endpoints.

Two trace modes are supported:

* ``custom-js`` uses Geth's arbitrary JavaScript tracer interface and records exact EVM
  ``SLOAD``/``SSTORE`` accesses plus opcode-step counts. This is the highest-fidelity mode, but
  many hosted/public RPC providers disable custom tracers.
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
        choices=[TRACE_MODE_CUSTOM_JS, TRACE_MODE_PUBLIC_RPC],
        default=TRACE_MODE_PUBLIC_RPC,
        help=(
            "public-rpc uses hosted-provider-compatible built-in prestateTracer calls; "
            "custom-js uses the exact Geth JavaScript SLOAD/SSTORE tracer"
        ),
    )
    parser.add_argument("--timeout", type=int, default=600)
    parser.add_argument("--rpc-retries", type=int, default=5)
    parser.add_argument("--retry-backoff", type=float, default=1.5)
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

    tracer = None
    if args.trace_mode == TRACE_MODE_CUSTOM_JS:
        tracer = (SCRIPT_DIR / "geth-rw-tracer.js").read_text(encoding="utf-8")
    client = RpcClient(
        args.rpc_url,
        args.timeout,
        retries=args.rpc_retries,
        retry_backoff=args.retry_backoff,
    )

    if args.probe_only:
        tag = hex(args.start_block)
        print(f"probe [{args.start_block}] fetch block + {args.trace_mode} trace", flush=True)
        block = client.call("eth_getBlockByNumber", [tag, True])
        if block is None:
            raise RuntimeError(f"Ethereum RPC has no block {args.start_block}")
        if args.trace_mode == TRACE_MODE_CUSTOM_JS:
            traces = trace_block_custom_js(client, tag, tracer, args.timeout)
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
        "access_semantics": access_semantics,
        "extractor": extractor,
        "compute_proxy": compute_proxy,
        "public_rpc_caveat": (
            "reads are all storage slots touched by prestateTracer; writes are storage slots whose "
            "state changed in diffMode. This is conservative and is not byte-for-byte equivalent "
            "to the custom SLOAD/SSTORE tracer."
            if args.trace_mode == TRACE_MODE_PUBLIC_RPC
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
