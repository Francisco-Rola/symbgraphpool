# Third-party notices

## CosmWasm

The runtime depends on the published `cosmwasm-std` and `cosmwasm-vm` crates, licensed under
Apache-2.0 by the CosmWasm contributors.

The file
`crates/acg-cosmwasm-engine/testdata/hackatom_1.2.wasm.b64` is an upstream CosmWasm test fixture copied
from the user-provided CosmWasm fork. It is retained only to verify execution of real Wasm bytecode
and is covered by the upstream CosmWasm Apache-2.0 licensing terms.

No source files from the experimental VM fork are vendored into this runtime. Its behavior was
reviewed and then reimplemented behind the published CosmWasm host interfaces.
