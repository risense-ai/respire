# Runtime SDKs

The generated SDKs use the canonical [OpenAPI 3.1 contract](../contracts/runtime-api/openapi.yaml)
and the golden fixtures in this repository. Never edit generated code by hand.

## Generate and Test

After OpenSpec approval, update `contracts/runtime-api/openapi.yaml` in the same
PR as its new endpoints and consuming app changes. Regenerate and test all
three packages from the repository worktree:

- **Rust (CLI):** `cargo +1.95.0 test --locked --manifest-path packages/runtime-sdk-rs/Cargo.toml`.
  Progenitor builds the typed client automatically from the local contract.
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
