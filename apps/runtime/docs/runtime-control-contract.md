# Runtime protocol contracts

The canonical public API snapshot is the monorepo root
[`contracts/runtime-api/openapi.yaml`](../../../contracts/runtime-api/openapi.yaml).
The separate resources repository owns the authoritative API inputs.

Runtime's typed RPC and host-control compatibility code remains in
[`crates/protocol`](../crates/protocol). These transports and authorization
boundaries are distinct from the public API. Runtime carries no duplicate
canonical API tree.
