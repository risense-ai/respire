# Repository responsibilities

The canonical product repository is [risense-ai/respire](https://github.com/risense-ai/respire).
See [SOURCE-PROVENANCE.md](../SOURCE-PROVENANCE.md) for the pinned historical source.

- `apps/runtime` owns the `rsrs-runtime` service, encrypted local data,
  inference orchestration, and Runtime RPC and MCP endpoints.
- `apps/core` owns the pure inference and memory engine. Runtime builds it via
  a relative Cargo dependency and consumes its existing C ABI.
- CLI, Client2, and plugins interact with Runtime through protocols and
  capabilities. Hosts supervise Runtime and keep persistent credential custody.
- The versioned API snapshot is under the monorepo's root `contracts/`,
  maintained from `respire-resources`. Runtime's typed internal protocol
  remains in its own crate.

See [SOURCE-BUILD.md](../SOURCE-BUILD.md) for direct builds.
