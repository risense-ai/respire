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

### Send plugin data without command-line arguments

For a new plugin, discover the local Runtime endpoint and call
`GET /v1/health`. Choose transport by capability, not by Runtime version.
When `openapi.v1` is advertised, use the [generated SDKs](../../packages)
and send memory content as UTF-8 JSON in the request body. For example,
`POST /v1/memories` with `{"title":"Migration note","content":"The note body"}`
returns the ID used by `GET /v1/memories/{id}`. Use the existing local bearer
as an authorization header. Do not put it in a URL, log, or command line.
If discovery returns 404 or does not advertise the capability, keep the
supported legacy transport; do not retry a write over a different transport
after an ambiguous response.

For an MCP client, start `rsrs mcp` with fixed startup arguments and send
JSON-RPC frames on stdin, including all tool arguments in `params.arguments`.
Read results from stdout. Clients already configured for Runtime MCP over
loopback can use `POST /mcp` with existing authentication instead. Neither
method requires passing note or query content as process arguments, avoiding
Windows command-line size and quoting limits. For the complete compatibility
rules, see the [Runtime migration playbook](https://github.com/risense-ai/respire-resources/blob/main/contracts/OPENAPI_MIGRATION_PLAYBOOK.md).
