# apps/core

Hosts Respire's open-source pure computation and inference kernel (`memory-core`, `protocol`, `core-ffi`), imported from `risense-ai/respire-core` after the `CARD-002` pre-open-source secret/license audit (`CARD-013`).

- MUST own an independent `Cargo.toml` and `Cargo.lock`;
- Strictly pure in-memory: NO filesystem, SQLite, keyring, HTTP, or network I/O crates;
- Preserves version-independent FFI compatibility with `apps/runtime`.
