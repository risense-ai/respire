# apps/core

Hosts Respire's public pure computation and inference kernel (`memory-core`, `protocol`, `core-ffi`), published under **Respire Noncommercial License 1.0** (preserving third-party notices) from audited `risense-ai/respire-core` source (`CARD-002` / `CARD-013`). Legacy `risense-ai/respire-core` remains a private historical archive.

- MUST own an independent `Cargo.toml` and `Cargo.lock`;
- Strictly pure in-memory: NO filesystem, SQLite, keyring, HTTP, or network I/O crates;
- Preserves version-independent FFI compatibility with `apps/runtime`.
