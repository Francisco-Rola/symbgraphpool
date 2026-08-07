# VM test data

`hackatom_1.2.wasm.b64` is a text encoding of the Apache-2.0 licensed CosmWasm Hackatom test
fixture. The test decodes it in memory to verify that the engine executes real CosmWasm bytecode
and processes the contract's emitted bank transfer.

The text representation keeps the repository's incremental patch portable through the standard
`patch` command; no binary patch support is required.
