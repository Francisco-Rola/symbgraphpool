#!/usr/bin/env python3
"""Helpers for parsing `perf stat -x ';'` output across Linux/perf versions.

perf 7.x may emit event names with modifiers (e.g. task-clock:u) or PMU wrappers.
Keep this parser deliberately tolerant while rejecting <not supported>/<not counted> rows.
"""
from __future__ import annotations

import math
import re
from pathlib import Path

_INVALID = {"<not supported>", "<not counted>", "<not available>"}


def _parse_number(raw: str):
    raw = raw.strip()
    if not raw or raw.lower() in _INVALID:
        return None
    # The runner forces LC_ALL=C, but tolerate common grouped-output remnants.
    raw = raw.replace(" ", "")
    if raw.count(",") and not raw.count("."):
        # A single comma followed by exactly three digits is more likely a grouping
        # separator than a decimal separator in perf --no-big-num output. Otherwise
        # accept it as a decimal separator for manually supplied files.
        head, tail = raw.rsplit(",", 1)
        raw = raw.replace(",", "") if len(tail) == 3 and head.replace("-", "").isdigit() else raw.replace(",", ".")
    else:
        raw = raw.replace(",", "")
    try:
        value = float(raw)
    except ValueError:
        return None
    return value if math.isfinite(value) else None


def normalize_event_name(event: str) -> str:
    event = event.strip()
    # cpu_core/task-clock/ and cpu_atom/task-clock/ style PMU wrappers.
    if "/" in event:
        bits = [b for b in event.split("/") if b]
        if len(bits) >= 2 and bits[0].startswith("cpu_"):
            event = bits[1]
    # perf modifiers such as :u, :k, :H, :G, :p.
    event = re.sub(r":[ukhgGHpPeI]+$", "", event)
    aliases = {
        "cs": "context-switches",
        "migrations": "cpu-migrations",
        "faults": "page-faults",
    }
    return aliases.get(event, event)


def _to_ms(value: float, unit: str) -> float:
    u = unit.strip().lower()
    if u in {"msec", "ms", "milliseconds"}:
        return value
    if u in {"usec", "us", "microseconds"}:
        return value / 1_000.0
    if u in {"nsec", "ns", "nanoseconds"}:
        return value / 1_000_000.0
    if u in {"sec", "s", "seconds"}:
        return value * 1_000.0
    return value


def parse_perf_stat(path):
    """Return normalized event values from a perf CSV file.

    task-clock and duration_time are normalized to milliseconds when their units
    identify a time scale. Other counters retain their native numeric value.
    """
    events = {}
    unsupported = []
    with open(path, encoding="utf-8", errors="replace") as f:
        for raw_line in f:
            line = raw_line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split(";")
            if len(parts) < 3:
                continue
            raw_value, unit, raw_event = parts[0].strip(), parts[1].strip(), parts[2].strip()
            event = normalize_event_name(raw_event)
            if raw_value.lower() in _INVALID:
                unsupported.append(event)
                continue
            value = _parse_number(raw_value)
            if value is None:
                continue
            # Many perf versions emit wall duration as a synthetic final CSV row
            # whose event label is literally "seconds time elapsed" rather than
            # duration_time. Normalize both forms.
            if "seconds time elapsed" in raw_event.lower():
                events["duration_time"] = value * 1_000.0
                continue
            if event in {"task-clock", "duration_time"}:
                value = _to_ms(value, unit)
            events[event] = value
    return events, unsupported


def perf_health(path, elapsed_ms=None):
    events, unsupported = parse_perf_stat(path)
    task_ms = events.get("task-clock")
    duration_ms = events.get("duration_time") or elapsed_ms
    avg_cpus = task_ms / duration_ms if task_ms is not None and duration_ms else None
    cycles = events.get("cycles")
    instructions = events.get("instructions")
    cache_refs = events.get("cache-references")
    cache_misses = events.get("cache-misses")
    return {
        "events": events,
        "unsupported": unsupported,
        "task_clock_ms": task_ms,
        "duration_ms": duration_ms,
        "avg_cpus": avg_cpus,
        "ipc": instructions / cycles if instructions is not None and cycles else None,
        "cache_miss_rate": cache_misses / cache_refs if cache_misses is not None and cache_refs else None,
        "working": task_ms is not None and task_ms > 0,
    }
