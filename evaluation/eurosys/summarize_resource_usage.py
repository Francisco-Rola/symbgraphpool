#!/usr/bin/env python3
"""Parse GNU /usr/bin/time -v output emitted for isolated Wasmd strategies."""
from __future__ import annotations
import argparse, csv, json, re
from pathlib import Path

NAME_TO_STRATEGY = {
    "serial": "cosmos-wasmd-direct-serial",
    "blockstm": "cosmos-wasmd-block-stm",
    "ariafb": "cosmos-wasmd-aria-fb",
    "symbgraph-rust": "cosmos-wasmd-symbgraph-rust",
    "vegeta": "cosmos-wasmd-vegeta",
    "exact-oracle": "cosmos-wasmd-symbgraph-rust-exact-trace-oracle",
}


def seconds(value: str) -> float:
    parts = value.strip().split(":")
    try:
        if len(parts) == 1:
            return float(parts[0])
        if len(parts) == 2:
            return 60 * float(parts[0]) + float(parts[1])
        return 3600 * float(parts[-3]) + 60 * float(parts[-2]) + float(parts[-1])
    except ValueError:
        return 0.0


def parse(path: Path) -> dict[str, object]:
    vals: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        text = line.strip()
        if text.startswith("Elapsed (wall clock) time"):
            marker = "): "
            if marker in text:
                vals["Elapsed (wall clock) time (h:mm:ss or m:ss)"] = text.split(marker, 1)[1].strip()
            continue
        if ":" not in text:
            continue
        key, value = text.split(":", 1)
        vals[key.strip()] = value.strip()
    m = re.search(r"records-w(\d+)-([^.]+)\.jsonl\.resource\.txt$", path.name)
    if not m:
        raise ValueError(f"unexpected resource filename: {path.name}")
    workers, short = int(m.group(1)), m.group(2)
    return {
        "workers": workers,
        "strategy": NAME_TO_STRATEGY.get(short, short),
        "strategy_short": short,
        "max_rss_kib": float(vals.get("Maximum resident set size (kbytes)", "0") or 0),
        "user_seconds": float(vals.get("User time (seconds)", "0") or 0),
        "system_seconds": float(vals.get("System time (seconds)", "0") or 0),
        "elapsed_seconds": seconds(vals.get("Elapsed (wall clock) time (h:mm:ss or m:ss)", "0")),
        "major_page_faults": float(vals.get("Major (requiring I/O) page faults", "0") or 0),
        "minor_page_faults": float(vals.get("Minor (reclaiming a frame) page faults", "0") or 0),
        "fs_inputs": float(vals.get("File system inputs", "0") or 0),
        "fs_outputs": float(vals.get("File system outputs", "0") or 0),
    }


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--raw-dir", type=Path, required=True)
    ap.add_argument("--output-dir", type=Path, required=True)
    args = ap.parse_args()
    rows = [parse(p) for p in sorted(args.raw_dir.glob("records-w*-*.jsonl.resource.txt"))]
    args.output_dir.mkdir(parents=True, exist_ok=True)
    csv_path = args.output_dir / "resource-usage.csv"
    fields = list(rows[0]) if rows else ["workers","strategy","strategy_short","max_rss_kib","user_seconds","system_seconds","elapsed_seconds","major_page_faults","minor_page_faults","fs_inputs","fs_outputs"]
    with csv_path.open("w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=fields); w.writeheader(); w.writerows(rows)
    (args.output_dir / "resource-usage.json").write_text(json.dumps({"schema_version":1,"rows":rows}, indent=2)+"\n", encoding="utf-8")
    print(csv_path)


if __name__ == "__main__":
    main()
