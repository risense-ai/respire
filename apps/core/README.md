# apps/core

Hosts Respire's public pure computation and inference kernel (`memory-core`, `protocol`, `core-ffi`), published under **Respire Noncommercial License 1.0** (preserving third-party notices) from audited `risense-ai/respire-core` source (`CARD-002` / `CARD-013`). Legacy `risense-ai/respire-core` remains a private historical archive.

- MUST own an independent `Cargo.toml` and `Cargo.lock`;
- Strictly pure in-memory: NO filesystem, SQLite, keyring, HTTP, or network I/O crates;
- Preserves version-independent FFI compatibility with `apps/runtime`.

## CARD-013 audit

The imported baseline already removed `memory-core/src/legacy_engine.rs`,
`transport/`, `crypto.rs`, `env.rs`, and `crates/winml-catalog/`.
Historical envelope, payload and embedding specimens are static Golden
Fixtures under `crates/memory-core/tests/fixtures/`; Core does not decrypt them.

`core-ffi/src/deepen_topology.rs` indexes parent-to-child adjacency in a
`HashMap` and counts reachable descendants in O(N). The planner retains the
15-child minimum, the two-thirds descendant threshold, and the original
cluster membership rules. Pairwise vector clustering remains O(K^2) for K
embedded children; the linear bound applies to topology and counting.

`protocol` contains wire DTOs, vocabulary and serialization compatibility.
Metadata validation belongs to `memory-core/src/metadata/validation.rs`.
Protocol compatibility tests preserve flattened entry extensions, ignore
unknown query hints, and reject unknown complete-render inputs. Preserving
an extension in a DTO does not authorize its semantics during validation.

Runtime links Core directly through Cargo. No private Core SDK tarball or
cache helper remains in Core or the shared scripts directory. The public
model-fixture preparation script is test infrastructure, not SDK acquisition.
The frozen ABI contract is unchanged; contract documentation updates must
originate in `respire-resources` and be synchronized separately.

Run verification from the monorepo root:

```sh
node scripts/check-guardrails.mjs
cargo test --manifest-path apps/core/Cargo.toml --all-targets --locked
```

The native suite requires `RSRS_CORE_TEST_MODEL_DIR` and
`RSRS_CORE_TEST_ORT_FILE`. Verify existing model inputs with
`node apps/core/scripts/prepare-test-fixtures.mjs <fixture-dir> --verify-only`.
Use `RUST_TEST_THREADS=1` when running tests that share native session and
inference-queue state. Independent review is required before acceptance.
