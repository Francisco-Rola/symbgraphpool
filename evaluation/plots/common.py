from __future__ import annotations
import csv, json
from pathlib import Path


def read_csv(path: Path):
    with path.open(newline='', encoding='utf-8') as f:
        return list(csv.DictReader(f))


def read_json(path: Path):
    return json.loads(path.read_text(encoding='utf-8'))


def ensure_out(path: Path):
    path.parent.mkdir(parents=True, exist_ok=True)
    return path


def f(row, key, default=0.0):
    try:
        return float(row.get(key, default) or default)
    except (TypeError, ValueError):
        return default


def strategy_label(row):
    return row.get('label') or row.get('strategy', '')


def grouped_xy(rows, xkey, ykey, groupkey='label'):
    groups = {}
    for r in rows:
        groups.setdefault(r.get(groupkey) or r.get('strategy', ''), []).append((f(r, xkey), f(r, ykey)))
    return {k: sorted(v) for k, v in groups.items()}


def errorbar_series(ax, rows, *, xkey, ykey, label, marker='o'):
    rs = sorted(rows, key=lambda r: f(r, xkey))
    if not rs:
        return
    xs = [f(r, xkey) for r in rs]
    ys = [f(r, ykey) for r in rs]
    yerr = [f(r, ykey + '_ci95') for r in rs]
    ax.errorbar(xs, ys, yerr=yerr if any(yerr) else None, marker=marker, capsize=3, label=label)


def save(fig, path):
    fig.tight_layout()
    fig.savefig(ensure_out(path), bbox_inches='tight')
