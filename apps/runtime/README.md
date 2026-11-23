# apps/runtime

Hosts `respire-runtime` (authoritative control & data plane daemon), imported with preserved Git history from `risense-ai/respire-runtime` during OpenSpec `003` (`CARD-006`).

- Owns an independent `Cargo.toml` and `Cargo.lock`;
- Exposes REST + SSE conforming strictly to root `contracts/runtime-api/openapi.yaml`;
- Links `apps/core` purely via the `core-abi` boundary without leaking DB or network logic into Core.
