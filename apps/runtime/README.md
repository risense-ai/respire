# Respire Runtime

This component builds the Respire Runtime service (`rsrs-runtime`) against Core source in the same repository. CLI and Client2 discover, install and supervise Runtime independently through protocols.

Boundary in one sentence: **runtime = core capability + RPC; the CLI or Client2
owns downloads, installation, component management and persistent private keys.**

Runtime dev.24 exposes [process-local core configuration](docs/core-config.md)
for candidate validation, atomic application and readback. Hosts own persistence;
legacy configuration reads remain until a separate boundary audit.

Core capabilities include account authentication, synchronization, database
management, recall and remember. A serving Runtime never accesses the OS
credential store. After restart it reports `credential_state: "required"` and
`store_ready: false`, keeps the library closed, and pauses indexing and sync.
The host reads the existing system credentials and submits them to
`POST /api/runtime/unlock`. This loopback endpoint requires the host bearer
token and binds the request to the exact profile, session revision, Runtime
instance and generation. Keys remain in process memory and are cleared at exit.

The CLI supplies credentials during startup and through `rsrs runtime unlock`.
Client2 requests the same CLI operation when an existing Runtime needs its key.
Other hosts can implement this protocol without adopting either frontend.
Private account-control helpers use the inherited framed channel to request
credential custody from their host; they never load a native credential backend.
These exchanges keep the existing transaction checkpoints and parent lifetime
checks, including confirmation before encrypted account state is committed.

## Layout

| Path | Purpose |
| --- | --- |
| `runtime/` | `rsrs-runtime` binary crate: serve/dispatch/admission, loopback HTTP, MCP endpoints, health, sync worker, snapshot apply |
| `crates/protocol/` | `respire-protocol` RPC contract crate, published to crates.io; the shared contract of this repo and `respire-cli` |
| `crates/core-sdk/` | Safe Runtime adapter to the in-repository Core C ABI |
| `crates/app/` | Application services: account transactions, memory, sync, resident recall, snapshots, diary, review, and embedding |
| `crates/storage/` | Encrypted storage layer |
| `crates/crypto/` | Cryptography primitives |
| `scripts/` | Build, native dependency verification and compatibility tooling |
| `docs/` | Release process, version/compatibility contract, repository map |

## Release

Release processing belongs to the monorepo. This source import does not publish an artifact. Keep native libraries and their notices beside any distributed executable.

## Building from source

From the monorepo root, run `cargo build --manifest-path apps/runtime/Cargo.toml --locked`. See [SOURCE-BUILD.md](SOURCE-BUILD.md) for verification and native platform inputs.

The runtime binary never downloads models or assets on its own; missing assets
are reported through `health` and `assets.status`, and the CLI installs them.
Model verification, switching and background indexing only inspect installed assets.
The business command parser contains no installer, uninstaller, startup
registration, plugin-management, injection or CLI-update commands. Asset
requirements and verification remain read-only RPC capabilities; the host owns
the actual file changes. Host directory migration never runs before business RPC.

## Runtime surface (unchanged from the combined binary)

- Loopback HTTP on `127.0.0.1:15169`, token-free on loopback, `RSRS_RPC_TOKEN` otherwise
- `/mcp` and `/sse` endpoints, `cli.exec` dispatch, health endpoint
- `RSRS_RUNTIME_BOOT_TOKEN` handoff via the CLI-side spawn infrastructure

## License

First-party source uses the [Respire Noncommercial License 1.0](../../LICENSE). Preserve applicable third-party notices with distributed artifacts.
