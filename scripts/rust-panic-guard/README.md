# Incremental Rust panic-site guard

This CARD-012 tool checks source, not an application's runtime semantics.
Its isolated Cargo manifest locks `syn`, `proc-macro2`, `quote`, and
`unicode-ident` to versions and registry checksums already in the Core lock.
No product manifest or build feature changes.

Build and run from the monorepo root, using the required guardrails checkout
with both exact commits and their ancestry available. In the existing
guardrails job, `BASE_SHA` is the externally supplied trusted PR target or
push predecessor, never the checked source SHA. Pin the Rust toolchain used
by this existing job; local validation used Rust 1.95.0.

~~~sh
cargo test --locked --manifest-path scripts/rust-panic-guard/Cargo.toml
cargo build --locked --manifest-path scripts/rust-panic-guard/Cargo.toml
export RESPIRE_RUST_PANIC_PARSER="$PWD/scripts/rust-panic-guard/target/debug/respire-rust-panic-guard"
node --test scripts/check-rust-panic.test.mjs
node scripts/check-rust-panic.mjs --base "$BASE_SHA" --head "$CHECKED_SHA" --parser "$RESPIRE_RUST_PANIC_PARSER"
~~~

Use the `.exe` suffix on Windows. Both git objects, the native analyzer, and
parseable sources are required; a missing prerequisite is not a passing skip.
The checker reads committed Git blobs, not a possibly different worktree.
It reports source paths and method identifier locations, not source text.
The tests isolate temp Git metadata and never run product Rust code.

The parser distinguishes methods and qualified function references
(`Alias::unwrap`, `<Alias>::expect`) from comments, strings, other names,
and plain fields. It also handles literal Rust method/path tokens within
macro bodies. Old AST sites may match one-to-one by unchanged Git line
projection, or within the same changed hunk if the normalized syntax shape
matches. Dropping a test guard, adding call parentheses to an existing
field or adding a second copy of a call remains a new site. Changing an
existing diagnostic string alone does not add a site.

Only enclosing conditions proved false for a production compile with
`test=false` are exempt. This includes actual `cfg(test)`, test-required
`all(...)`, and unconditional nested `cfg_attr`. A directory/file name,
a bare attribute name, `cfg(any(test, windows))`, and feature guesses
never exempt code. For separately declared test modules that lack a
file-local test-only annotation, keep additions production-safe or add an
explicit `#![cfg(test)]` to the genuinely test-only file. The guard does
not resolve cross-file includes, type aliases to functions through
arbitrary runtime indirection, procedural-macro expansion, or Rust type
inference. It is not a claim that the code cannot panic.
