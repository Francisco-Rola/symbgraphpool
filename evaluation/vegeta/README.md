# Vegeta-derived workloads

This directory retains the reviewed family maps/manifests used to translate the Vegeta Ethereum datasets into the Wasmd evaluation substrate. It is data provenance, not an experiment entry point.

- **S1:** 5,000-block headline prior-work workload; exact per-transaction tracing is not required for the final full-domain campaign.
- **S3:** 101-block exact/mechanism workload used for phase breakdown, exact-oracle analysis and ACG implementation ablation.
- **S4:** 5,000-block later-period workload. Source collection is supported; native translation remains pending until the current S4 trace is characterized.

Use `evaluation/workloads/` to prepare datasets and `evaluation/experiments/` to run paper experiments. Metric definitions and the claim/figure map are in `evaluation/PAPER_PLAN.md`.
