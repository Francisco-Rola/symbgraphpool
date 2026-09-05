# Vegeta S1 corpus

This directory stores the S1 source corpus and prepared native Wasmd execution bundle used by the paper artifact. The canonical preparation wrappers are:

```bash
ETH_RPC_URL=... bash evaluation/workloads/collect_s1.sh
bash evaluation/workloads/prepare_s1.sh
```

The full-domain paper experiment deliberately uses the scalable prestate/diff representation rather than requiring an exact per-transaction SLOAD/SSTORE trace. The native translation is an S1-derived Wasmd workload; source-vs-translated parallelism is reported explicitly by the S1 experiment.
