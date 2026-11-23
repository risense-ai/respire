# Runtime SDKs

The generated SDKs use the exact OpenAPI 3.1 and golden fixtures pinned in
[contracts/sync.lock.json](../contracts/sync.lock.json). Never edit generated code,
the copied contract, or copied fixtures manually.

## Generate from a clean checkout

Fetch the public [resources repository](https://github.com/risense-ai/respire-resources)
into an adjacent checkout, then verify the snapshot without updating it:

```sh
node scripts/sync-contracts.mjs --check ../respire-resources
```

To intentionally advance the snapshot, fetch the resources main ref and pass
the approved 40-character commit to `node scripts/sync-contracts.mjs --update
../respire-resources <commit>`. Then regenerate and test all three packages:

- **Rust (CLI):** `cargo +1.95.0 test --locked --manifest-path packages/runtime-sdk-rs/Cargo.toml`.
  Progenitor builds the typed client automatically from the pinned contract.
- **Go (Client2):** from `packages/runtime-sdk-go`, run `go generate ./...`, then
  `go test ./...`. Generate before importing this local package into Client2.
- **TypeScript (frontend and plugins):** from `packages/runtime-sdk-ts`, run
  `npm ci --ignore-scripts`, `npm run generate`, `npm run check`, and `npm test`.
  Generate before importing the local package. The typed HTTP client is
  created by `createRuntimeClient`.

The Rust generator uses an in-memory OpenAPI 3.0 nullable projection. Go and
TypeScript consume the unmodified OpenAPI 3.1 file. Generated outputs are
recreated during builds (not checked in); the tool dependencies are locked.
The golden tests cover explicit nulls, UTF-8 text, nested errors, and model
catalog responses. Runtime handlers and SSE transport are tested separately.
