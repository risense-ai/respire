# Core source provenance

The Core snapshot is from audited source revision
`339e8793c12d6cbb47aa165ddcc7d82dbc1607ef`. This is a source-only
export, not a history import. The legacy repository remains private.

The export contains Cargo manifests and lockfile, the `core-ffi`,
`memory-core`, and `protocol` crates, the frozen C ABI header and
compatibility fixtures under `contracts/core-abi/`, the patched Tokenizers
source under `vendor/tokenizers/`, native dependency notices under
`third-party/`, and the source SDK builder and notice collector.
No release binaries, deployment data, SDK cache, or history are included.
The only binary fixture is a documented 12-byte synthetic float vector.

First-party source is under the Respire Noncommercial License 1.0;
see `LICENSE` and `COMMERCIAL-LICENSE.md`. Third-party licenses and
notices remain in their original locations. The former interface-license
path is retained for Cargo metadata and uses the same first-party license.
