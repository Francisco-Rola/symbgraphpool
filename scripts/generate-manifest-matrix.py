#!/usr/bin/env python3
"""Expand a compact evaluation grid into one Brick-5F ExperimentManifest."""

from __future__ import annotations

import argparse
import copy
import itertools
import json
import random
from pathlib import Path


def load_json(path: Path):
    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def stringify(value):
    if isinstance(value, bool):
        return "true" if value else "false"
    if value is None:
        return "null"
    return str(value)


def expand_grid(spec: dict, source: Path) -> dict:
    if spec.get("schema_version") != 1:
        raise ValueError("matrix schema_version must be 1")
    experiment_id = spec.get("experiment_id")
    if not isinstance(experiment_id, str) or not experiment_id:
        raise ValueError("experiment_id must be a non-empty string")
    physical_core_limit = int(spec.get("physical_core_limit", 0))
    if physical_core_limit <= 0:
        raise ValueError("physical_core_limit must be > 0")

    if "policy_file" in spec:
        policy_path = Path(spec["policy_file"])
        if not policy_path.is_absolute():
            # First try relative to the matrix file, then repo/cwd style.
            candidate = source.parent / policy_path
            policy_path = candidate if candidate.exists() else policy_path
        policy = load_json(policy_path)
    else:
        policy = copy.deepcopy(spec.get("policy"))
    if not isinstance(policy, dict):
        raise ValueError("policy or policy_file is required")

    base = spec.get("base_run", {})
    workload = base.get("workload")
    if not isinstance(workload, str) or not workload:
        raise ValueError("base_run.workload is required")
    base_parameters = {str(k): stringify(v) for k, v in base.get("parameters", {}).items()}

    matrix = spec.get("matrix", {})
    modes = matrix.get("modes", [base.get("mode", "cost-aware")])
    workers = matrix.get("workers", [base.get("workers", physical_core_limit)])
    seeds = matrix.get("seeds", [base.get("seed", 1)])
    parameter_grid = matrix.get("parameters", {})
    cases = matrix.get("cases", [{"parameters": {}}])
    if not cases:
        cases = [{"parameters": {}}]

    grid_keys = sorted(parameter_grid)
    grid_values = [parameter_grid[key] for key in grid_keys]
    for key, values in zip(grid_keys, grid_values):
        if not isinstance(values, list) or not values:
            raise ValueError(f"matrix.parameters[{key!r}] must be a non-empty array")

    parameter_products = list(itertools.product(*grid_values)) if grid_keys else [()]
    runs = []
    for mode, worker, seed, product, case in itertools.product(
        modes, workers, seeds, parameter_products, cases
    ):
        worker = int(worker)
        if worker <= 0 or worker > physical_core_limit:
            raise ValueError(
                f"worker count {worker} exceeds physical_core_limit {physical_core_limit}"
            )
        params = dict(base_parameters)
        for key, value in zip(grid_keys, product):
            params[str(key)] = stringify(value)
        case_params = case.get("parameters", {}) if isinstance(case, dict) else {}
        for key, value in case_params.items():
            params[str(key)] = stringify(value)
        runs.append(
            {
                "workload": workload,
                "mode": str(mode),
                "run_index": 0,
                "seed": int(seed),
                "workers": worker,
                "parameters": dict(sorted(params.items())),
            }
        )

    if not runs:
        raise ValueError("matrix expanded to zero runs")
    order_seed = spec.get("order_seed")
    if order_seed is not None:
        random.Random(int(order_seed)).shuffle(runs)
    for index, run in enumerate(runs, start=1):
        run["run_index"] = index

    return {
        "schema_version": 1,
        "experiment_id": experiment_id,
        "record_schema_version": int(spec.get("record_schema_version", 2)),
        "physical_core_limit": physical_core_limit,
        "policy": policy,
        "runs": runs,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("matrix", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    spec = load_json(args.matrix)
    manifest = expand_grid(spec, args.matrix)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(manifest, indent=2, sort_keys=False) + "\n", encoding="utf-8")
    print(f"generated {len(manifest['runs'])} runs -> {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
