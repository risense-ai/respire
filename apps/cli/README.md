# apps/cli

Hosts the CLI, TUI, `rsrs mcp` stdio bridge, IDE injection, and independent Runtime launcher imported for OpenSpec `003` (`CARD-007`). See [SOURCE-PROVENANCE.md](SOURCE-PROVENANCE.md) for the exact revision and import exclusions, and [MIGRATION-STATUS.md](MIGRATION-STATUS.md) for verification and outstanding migration gates.

- Owns an independent `Cargo.toml` and `Cargo.lock`;
- Does not link `apps/runtime`, `apps/core`, `storage`, `core-sdk`, `app`, or `axum`;
- Still retains the legacy host HTTP boundary and host safety mechanisms described in the migration status. The import is not ready for final acceptance;
- Uses the frozen legacy protocol and HTTP transport. Frozen operation inspection uses the generated Rust SDK. Other commands retain their legacy transports.

### Inspect a Runtime operation

Use the operation ID returned by a frozen Runtime HTTP endpoint with
`rsrs --json runtime operation <operation-id>`. Add `--follow` to wait for
completion over SSE. Observation ends after 30 seconds. Run the same command
again to inspect a longer operation. After a disconnect the CLI rechecks the
Runtime instance, reads a new snapshot and resubscribes once. It requires
an existing local bearer and the `sse.operations.v1` capability.
Legacy scoped RPC receipts use `rsrs runtime result --context <original-context>`.
