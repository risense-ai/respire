# Core dependency I/O guard

Run from the monorepo root with the existing pinned Cargo toolchain:

~~~sh
node --test scripts/check-core-dependencies.test.mjs
node scripts/check-core-dependencies.mjs
~~~

Use `--offline` only with an already populated Cargo cache. The guard fails
on missing prerequisites and does not run build scripts, compile a product,
fetch model weights, or launch Core. Cargo's normal locked registry
acquisition is sufficient in CI. Run this in the existing guardrails job;
no additional workflow is needed.

The checker resolves the actual locked workspace with all features for the
six supported Windows, Linux and macOS target graphs. Cargo metadata pins
the real package names behind workspace dependency aliases, including
optional, dev, build and target-specific declarations. The active Cargo
tree distinguishes normal, build and dev edges; neither a textual
Cargo.toml grep nor membership in Cargo.lock establishes runtime use.
Unrelated external crates' inactive dev tests are not shipped edges.

The sole existing acquisition exception is the locked crates.io
`ort-sys 2.0.0-rc.13` build dependency on `ureq 3.4.2`, whose normal
protocol helper is `ureq-proto 0.6.4`. The guard checks the source hashes,
Core ancestry, direct edge kinds, package provenance and reviewed HTTP
features, with no exception for a new package path or alias, git/local
replacement, dynamic runtime HTTP, `ort/fetch-models`, or another native
tool. The existing ORT provisioning feature remains unmodified. This
checks dependency edges, not every possible standard-library I/O call.

Review dependency/feature changes before changing any approved build pin.
The release-provisioning exception does not authorize new I/O by Core.
