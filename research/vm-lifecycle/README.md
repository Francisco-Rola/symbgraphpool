# VM lifecycle research archive

This directory is research-only. Production uses normal fresh CosmWasm instance semantics.

The archive preserves experiments for fresh-instance pools, adaptive preparation, unsafe dirty VM
reuse, cache sharding and related diagnostics. Dirty reuse was faster but leaks VM-local state and
is not correct. Cache sharding preserved fresh semantics but recovered little of the retained-VM
upper bound.

Do not copy these implementations back into production without a new correctness design and the
current Brick-5F evaluation gates.
