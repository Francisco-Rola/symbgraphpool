#!/usr/bin/env python3
"""Capture immutable machine/run provenance for EuroSys evaluation roots."""
from __future__ import annotations
import argparse, json, os, platform, shutil, subprocess, time
from pathlib import Path


def run(cmd: list[str]) -> str:
    try:
        return subprocess.check_output(cmd, text=True, stderr=subprocess.STDOUT).strip()
    except Exception:
        return ""


def read(path: str) -> str:
    try:
        return Path(path).read_text(encoding="utf-8", errors="replace").strip()
    except OSError:
        return ""


def physical_cores() -> int:
    text = run(["lscpu", "-p=CORE,SOCKET"])
    cores = {line for line in text.splitlines() if line and not line.startswith("#")}
    if cores:
        return len(cores)
    return os.cpu_count() or 1


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", type=Path, required=True)
    ap.add_argument("--tag", required=True)
    ap.add_argument("--profile", required=True)
    ap.add_argument("--workers", required=True)
    ap.add_argument("--samples", type=int, required=True)
    args = ap.parse_args()
    root = Path(__file__).resolve().parents[2]
    obj = {
        "schema_version": 1,
        "machine_tag": args.tag,
        "captured_unix": int(time.time()),
        "profile": args.profile,
        "workers": args.workers,
        "samples": args.samples,
        "git": {
            "commit": run(["git", "-C", str(root), "rev-parse", "HEAD"]),
            "describe": run(["git", "-C", str(root), "describe", "--always", "--dirty"]),
            "status_porcelain": run(["git", "-C", str(root), "status", "--porcelain"]),
        },
        "host": {
            "hostname": platform.node(),
            "platform": platform.platform(),
            "kernel": platform.release(),
            "machine": platform.machine(),
            "physical_cores": physical_cores(),
            "logical_cpus": os.cpu_count() or 1,
            "lscpu": run(["lscpu"]),
            "meminfo": read("/proc/meminfo"),
            "cpuinfo_model": next((line.split(":",1)[1].strip() for line in read("/proc/cpuinfo").splitlines() if line.lower().startswith("model name")), ""),
        },
        "toolchains": {
            "python": platform.python_version(),
            "rustc": run(["rustc", "--version"]) if shutil.which("rustc") else "",
            "cargo": run(["cargo", "--version"]) if shutil.which("cargo") else "",
            "go": run(["go", "version"]) if shutil.which("go") else "",
        },
        "environment": {
            k: os.environ[k]
            for k in sorted(os.environ)
            if k.startswith("PAPER_EVAL_") or k.startswith("EVAL_WASMD_IAVL_")
        },
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(obj, indent=2) + "\n", encoding="utf-8")
    print(args.output)


if __name__ == "__main__":
    main()
