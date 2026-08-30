# Maintained scripts

This directory intentionally contains only the supported repository entrypoints:

- `test-all.sh` — canonical repository-wide unit-test/lint gate.
- `eval-wasmd.sh` — controlled Wasmd scheduler campaign (`smoke`, `debug`, or `paper`).
- `eval-wasmd-debug.sh` — short local evaluation wrapper; on a 6-core host it uses 2/4/6 workers and one sample.
- `eval-wasmd-paper.sh` — publication-oriented wrapper; uses physical-core scaling and five samples by default.

Historical one-off campaign wrappers are retained under `tools/legacy-scripts/` only for reproducibility. They are not maintained entrypoints. Python data-preparation and analysis utilities live under `tools/` rather than in this directory.
