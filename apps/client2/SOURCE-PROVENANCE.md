# Client2 source provenance and migration status

## Source

- Repository: https://github.com/risense-ai/respire-client2
- Commit: `87ced0aa1f43cd3d8fe9c973758edfbac3fbec96`
- Import date: 2026-11-24
- Destination: `apps/client2/`
- Specification: OpenSpec `003-monorepo-merge-and-release-bridge`, CARD-008

The input was a read-only Git archive of the exact commit. Uncommitted source
edits and legacy Git history were not imported. Monorepo GitHub workflows,
documentation and the Respire Noncommercial License 1.0 replace their legacy
first-party equivalents. Third-party font assets, licenses and
`internal/desktop/frontend/notices.txt` are preserved. No dependency caches,
duplicated contracts, historical outputs or release binaries are included.

## Runtime boundary

Client2 is a desktop client and Runtime launcher; it does not open the
Runtime SQLite database. Startup and unlock use Runtime processes with verified
installation, profile, account and instance identity. Business operations
continue using their supported Runtime RPCs. Compatibility is negotiated by
feature or protocol, not by locking both apps to the same release.

An explicitly downloaded quantized BGE-M3 pair can be kept in the native
Runtime-managed namespace when the verified Runtime advertises that capability.
The normal download flow verifies both pinned files before the Runtime publishes
them. Online removal delegates the inactive managed pair to the running
Runtime, without stopping the process or deleting any legacy model files. A
missing or uncertain response is not treated as permission to use a destructive
fallback. Legacy downloads and packaged CPU or other variant paths remain
separate. Unsupported account-maintenance writes do not inherit startup or
model authority.

## Verification and limits

Isolated native Windows tests cover cold start, takeover and existing-account
startup, wrong-account or wrong-password rejection, scoped Vault unlock,
normal quantized BGE-M3 download, same-Runtime online uninstall and cancellation
while a legacy memory read is still available. Test fixtures do not use real
accounts or user memory.

Bilingual browser checks use the compiled Client2 server mode; they are not
a claim that every packaged WebView, GPU variant or historical maintenance
path has passed. See the pull request's exact-head CI and independent verdict
for merge acceptance.
