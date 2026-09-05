#!/usr/bin/env python3
"""Generate publication native-CosmWasm workloads for the common Wasmd evaluator.

Two workloads are provided:
  miniwarehouse: TPC-C-inspired multi-record application with a tunable hot warehouse.
  native-mix: blockchain-shaped CW20 + CW721 + AMM traffic over existing repository contracts.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import random
import shutil
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TARGET = "benchmarks/target/wasm32-unknown-unknown/release"


def h(tag: str, block: int, idx: int) -> str:
    return "0x" + hashlib.sha256(f"{tag}:{block}:{idx}".encode()).hexdigest()


def call(family: str, instance: str, sender: str, msg: dict) -> dict:
    return {"kind": "execute", "family": family, "instance_id": instance,
            "sender": sender, "msg": msg, "funds": []}


def write_dataset(out: Path, manifest: dict, blocks: list[dict], metadata: dict) -> None:
    out.mkdir(parents=True, exist_ok=True)
    manifest["blocks"] = len(blocks)
    manifest["transactions"] = sum(len(b["transactions"]) for b in blocks)
    (out / "execution-manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    with (out / "execution-plan.jsonl").open("w") as f:
        for row in blocks:
            f.write(json.dumps(row, sort_keys=True) + "\n")
    metadata = dict(metadata)
    metadata.update({"blocks": manifest["blocks"], "transactions": manifest["transactions"]})
    (out / "workload.json").write_text(json.dumps(metadata, indent=2, sort_keys=True) + "\n")


def choose_warehouse(rng: random.Random, count: int, hot_bps: int) -> int:
    if hot_bps and rng.randrange(10_000) < hot_bps:
        return 1
    return rng.randint(1, count)


def generate_miniwarehouse(args: argparse.Namespace) -> None:
    out: Path = args.output_dir
    sym = out / "symbolic"
    sym.mkdir(parents=True, exist_ok=True)
    symbolic = ROOT / "benchmarks/symbolic/miniwarehouse.symbolic.json"
    shutil.copyfile(symbolic, sym / symbolic.name)

    rng = random.Random(args.seed)
    admin = "native-s3-admin"
    client = "native-s3-mw-client"
    wh = args.warehouses
    districts = args.districts
    customers = args.customers
    items = args.items
    logical = {admin, client}
    priming = []
    for w in range(1, wh + 1):
        priming.append(call("miniwarehouse", "miniwarehouse", admin,
                            {"seed_warehouse": {"warehouse_id": w, "tax_bps": 100}}))
        for d in range(1, districts + 1):
            priming.append(call("miniwarehouse", "miniwarehouse", admin,
                                {"seed_district": {"warehouse_id": w, "district_id": d,
                                                   "tax_bps": 50, "next_order_id": 1}}))
            for c in range(1, customers + 1):
                priming.append(call("miniwarehouse", "miniwarehouse", admin,
                                    {"seed_customer": {"warehouse_id": w, "district_id": d,
                                                       "customer_id": c, "discount_bps": 0}}))
        for item in range(1, items + 1):
            priming.append(call("miniwarehouse", "miniwarehouse", admin,
                                {"seed_stock": {"warehouse_id": w, "item_id": item,
                                                "quantity": 1_000_000_000}}))

    next_order = defaultdict(lambda: 1)
    history = 1
    blocks = []
    for boff in range(args.blocks):
        txs = []
        for i in range(args.transactions):
            selector = rng.randrange(100)
            w = choose_warehouse(rng, wh, args.hot_warehouse_bps)
            d = rng.randint(1, districts)
            c = rng.randint(1, customers)
            if selector < 50:
                oid = next_order[(w, d)]
                next_order[(w, d)] += 1
                used = set()
                lines = []
                for _ in range(args.order_lines):
                    item = rng.randint(1, items)
                    while item in used:
                        item = rng.randint(1, items)
                    used.add(item)
                    supply = w
                    if wh > 1 and rng.randrange(10_000) < args.remote_stock_bps:
                        choices = [x for x in range(1, wh + 1) if x != w]
                        supply = rng.choice(choices)
                    lines.append({"item_id": item, "supply_warehouse_id": supply,
                                  "quantity": 1 + rng.randrange(5), "unit_price": "1"})
                msg = {"new_order": {"warehouse_id": w, "district_id": d,
                                      "customer_id": c, "order_id": oid, "lines": lines}}
            elif selector < 90:
                msg = {"payment": {"warehouse_id": w, "district_id": d,
                                    "customer_id": c, "amount": "1", "history_id": history}}
                history += 1
            else:
                msg = {"restock": {"warehouse_id": w, "item_id": rng.randint(1, items),
                                    "quantity": 10}}
            txs.append({"tx_index": i, "tx_hash": h("miniwarehouse", boff + 1, i),
                        "source_failed": False, "source_compute_proxy": 1,
                        "calls": [call("miniwarehouse", "miniwarehouse", client, msg)]})
        blocks.append({"block_number": boff + 1, "timestamp": 1_810_000_000 + boff * 6,
                       "transactions": txs})

    manifest = {
        "schema_version": 2,
        "dataset": f"miniwarehouse-w{wh}-hot{args.hot_warehouse_bps}",
        "wasm_artifacts": {"miniwarehouse": f"{TARGET}/acg_benchmark_miniwarehouse.wasm"},
        "instances": [{"instance_id": "miniwarehouse", "family": "miniwarehouse",
                       "instantiate_msg": {"admin": admin}}],
        "bank_seeds": [], "priming_calls": priming,
        "logical_addresses": sorted(logical), "first_timestamp": 1_810_000_000,
    }
    write_dataset(out, manifest, blocks, {
        "schema_version": 1, "kind": "miniwarehouse", "warehouses": wh,
        "districts_per_warehouse": districts, "customers_per_district": customers,
        "items_per_warehouse": items, "hot_warehouse_probability_bps": args.hot_warehouse_bps,
        "remote_stock_probability_bps": args.remote_stock_bps,
        "mix": {"new_order_pct": 50, "payment_pct": 40, "restock_pct": 10},
        "note": "TPC-C-inspired application; not TPC-C compliant",
    })
    print(f"generated MiniWarehouse: {manifest['dataset']} priming={len(priming)}")


def generate_native_mix(args: argparse.Namespace) -> None:
    out: Path = args.output_dir
    users = [f"native-s3-user-{i:04d}" for i in range(args.users)]
    admin = "native-s3-admin"
    pools = [f"pool-{i}" for i in range(args.pools)]
    token_instance = "token"
    nft_instance = "nft"
    priming = []
    instances = [
        {"instance_id": token_instance, "family": "controlled-cw20",
         "instantiate_msg": {"admin": admin,
                             "initial_balances": [{"address": u, "amount": "1000000000"} for u in users]}},
        {"instance_id": nft_instance, "family": "cw721-mintable",
         "instantiate_msg": {"admin": admin, "name": "ACG NFT", "symbol": "ACG"}},
    ]
    for p in pools:
        instances.append({"instance_id": p, "family": "astroport-pair",
                          "instantiate_msg": {"asset0": f"{p}-a", "asset1": f"{p}-b"}})
        priming.append(call("astroport-pair", p, admin,
                            {"sync": {"reserve0": "1000000000000", "reserve1": "1000000000000"}}))

    token_count = max(args.transactions * args.blocks, args.nft_tokens)
    nft_owner = {}
    for tid in range(1, args.nft_tokens + 1):
        owner = users[tid % len(users)]
        nft_owner[tid] = owner
        priming.append(call("cw721-mintable", nft_instance, admin,
                            {"mint": {"owner": owner, "token_id": tid, "token_uri": None}}))

    blocks = []
    nft_cursor = 1
    for boff in range(args.blocks):
        txs = []
        for i in range(args.transactions):
            mod = i % 4
            if mod in (0, 1):  # 50% token transfers
                sidx = (boff * args.transactions + i) % len(users)
                ridx = (sidx + 1 + (i % max(1, len(users) - 1))) % len(users)
                sender, recipient = users[sidx], users[ridx]
                c = call("controlled-cw20", token_instance, sender,
                         {"transfer": {"recipient": recipient, "amount": "1"}})
            elif mod == 2:  # 25% NFT transfers
                tid = nft_cursor
                nft_cursor += 1
                if nft_cursor > args.nft_tokens:
                    nft_cursor = 1
                sender = nft_owner[tid]
                recipient = users[(users.index(sender) + 1 + boff) % len(users)]
                nft_owner[tid] = recipient
                c = call("cw721-mintable", nft_instance, sender,
                         {"transfer_nft": {"recipient": recipient, "token_id": tid}})
            else:  # 25% AMM swaps, intentionally partitioned by pool
                p = pools[(boff * args.transactions + i) % len(pools)]
                c = call("astroport-pair", p, users[i % len(users)],
                         {"swap": {"offer_index": i & 1, "amount_in": "10", "min_out": "0",
                                   "recipient": users[(i + 3) % len(users)]}})
            txs.append({"tx_index": i, "tx_hash": h("native-mix", boff + 1, i),
                        "source_failed": False, "source_compute_proxy": 1, "calls": [c]})
        blocks.append({"block_number": boff + 1, "timestamp": 1_820_000_000 + boff * 6,
                       "transactions": txs})

    manifest = {
        "schema_version": 2, "dataset": f"native-mix-p{args.pools}-u{args.users}",
        "wasm_artifacts": {
            "controlled-cw20": f"{TARGET}/acg_benchmark_native_s3_controlled_cw20.wasm",
            "cw721-mintable": f"{TARGET}/acg_benchmark_native_s3_cw721_mintable.wasm",
            "astroport-pair": f"{TARGET}/acg_benchmark_native_s3_astroport_pair.wasm",
        },
        "instances": instances, "bank_seeds": [], "priming_calls": priming,
        "logical_addresses": sorted({admin, *users}), "first_timestamp": 1_820_000_000,
    }
    write_dataset(out, manifest, blocks, {
        "schema_version": 1, "kind": "native-mix", "users": args.users, "pools": args.pools,
        "nft_tokens": args.nft_tokens,
        "mix": {"cw20_transfer_pct": 50, "cw721_transfer_pct": 25, "amm_swap_pct": 25},
        "note": "native blockchain-shaped workload built from repository CosmWasm contracts",
    })
    print(f"generated NativeMix: {manifest['dataset']} priming={len(priming)}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="kind", required=True)
    mw = sub.add_parser("miniwarehouse")
    mw.add_argument("--output-dir", type=Path, required=True)
    mw.add_argument("--blocks", type=int, default=20)
    mw.add_argument("--transactions", type=int, default=384)
    mw.add_argument("--warehouses", type=int, default=16)
    mw.add_argument("--districts", type=int, default=4)
    mw.add_argument("--customers", type=int, default=32)
    mw.add_argument("--items", type=int, default=128)
    mw.add_argument("--order-lines", type=int, default=5)
    mw.add_argument("--hot-warehouse-bps", type=int, default=0)
    mw.add_argument("--remote-stock-bps", type=int, default=1000)
    mw.add_argument("--seed", type=int, default=20260905)
    mix = sub.add_parser("native-mix")
    mix.add_argument("--output-dir", type=Path, required=True)
    mix.add_argument("--blocks", type=int, default=20)
    mix.add_argument("--transactions", type=int, default=384)
    mix.add_argument("--users", type=int, default=256)
    mix.add_argument("--pools", type=int, default=16)
    mix.add_argument("--nft-tokens", type=int, default=4096)
    args = ap.parse_args()
    if args.kind == "miniwarehouse":
        generate_miniwarehouse(args)
    else:
        generate_native_mix(args)


if __name__ == "__main__":
    main()
