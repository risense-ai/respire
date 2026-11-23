# apps/cli

Hosts `respire-cli` (ultra-thin CLI, TUI, `rsrs mcp` stdio bridge, and Runtime lifecycle launcher), imported from `risense-ai/respire-cli` during OpenSpec `003` (`CARD-007`) after stripping ~54.5k legacy server lines and 23 obsolete workflows.

- Owns an independent `Cargo.toml` and `Cargo.lock`;
- MUST NEVER depend on `apps/runtime`, `apps/core`, `storage`, `core-sdk`, `app`, `axum`, or `tiny_http`;
- Consumes `packages/runtime-sdk-rs` exclusively.
