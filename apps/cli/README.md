# apps/cli

Hosts the CLI, TUI, `rsrs mcp` stdio bridge, IDE injection, and independent Runtime launcher imported for OpenSpec `003` (`CARD-007`). See [SOURCE-PROVENANCE.md](SOURCE-PROVENANCE.md) for the exact revision and import exclusions, and [MIGRATION-STATUS.md](MIGRATION-STATUS.md) for verification and outstanding migration gates.

- Owns an independent `Cargo.toml` and `Cargo.lock`;
- Does not link `apps/runtime`, `apps/core`, `storage`, `core-sdk`, `app`, or `axum`;
- Still retains the legacy host HTTP boundary and host safety mechanisms described in the migration status. The import is not ready for final acceptance;
- Uses the frozen legacy protocol and HTTP transport. Migration to `packages/runtime-sdk-rs` remains outstanding; adding an unused SDK dependency would not satisfy that boundary.
