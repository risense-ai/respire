# Component release dry run

The local assembler has no GitHub, registry, credential or tag operations.
It validates detached build batches and copies verified bytes into a fresh
monorepo/legacy mirror pair under an explicit local output directory. This is
not a release workflow, native test attestation, or authorization to publish.

## Build lane and assembly contract

1. Start from a clean, independently reviewed monorepo commit with its full
   Git SHA. Use the component release planner to validate an input tag string;
   a dry run never creates a Git tag. Build the selected component only. Use
   exact native target jobs and check out the same reviewed commit in each one.
   Keep trusted build identity (repository, resolved commit, workflow/run/attempt
   and native matrix target) in the CI artifact receipt, separate from payload.
2. Create detached directories that contain only the planned artifact filenames.
   For every directory collect one JSON object with exact keys
   `tag`, `source`, `kind` (`github` or `npm`), and `directory` (absolute
   canonical path). The assembler requires the exact aggregate file set and
   fails on overlap, wrong identity, unsafe paths, metadata/hash mismatches or
   incomplete packs.
3. On a fresh local consumer job, run:

   ```sh
   node scripts/component-release-plan.mjs tag "$COMPONENT_TAG"
   node scripts/assemble-component-release.mjs "$COMPONENT_TAG" "$SOURCE_SHA" "$BATCH_PLAN" "$OUTPUT_PARENT"
   node --test scripts/component-release-plan.test.mjs scripts/assemble-component-release.test.mjs
   ```

   The output receipt reports local bytes only. Preserve it and native checks
   as separate artifacts. The batch JSON is not an attestation; derive it from
   known reviewed artifacts from the exact same run/attempt, not from caller
   names or a cross-run search. Do not expose any protected environment on this
   lane. Do not add a publish or tag path to a dry-run dispatch.

## Existing native producers to reuse

- **Runtime:** use the Runtime workspace lockfile and existing native provider
  configuration in the component CI. Build `rsrs-runtime` with the exact
  component version using Cargo release mode; use
  `apps/runtime/scripts/build-linux-musl-container.sh` or the native musl
  script for the musl lanes, with genuine supported ORT/provider inputs.
  Use the currently reviewed Core in-tree link and actual native sidecars.
  Assemble a standalone binary, archive, and schema-2 build report for each of
  the eight triples. Reuse `smoke-runtime-release.mjs` against the packaged
  Runtime, not `--self-check` alone. Use the deterministic local
  [Runtime staging producer](runtime-component-staging.md) with each exact
  native build. A verified native artifact and smoke are still required for
  every target before the complete release dry run. Internal build metadata
  keeps `vX.Y.Z`; only the release lookup tag is component-prefixed.
- **CLI:** in a disposable checkout of `apps/cli`, use
  `node npm/scripts/set-build-version.mjs "$VERSION"` where needed, then the
  appropriate real native release build into `target/ci/<triple>/release`.
  `node npm/scripts/stage-artifacts.mjs <triple>` checks the actual binary
  identity and creates its paired metadata. Set hosted run ID and attempt
  from the real producer. Assembling eight platform binaries is separate from
  verifying nine local npm packs. Reuse the checked-in native package validator,
  `npm/scripts/assemble-artifacts.mjs` and `npm/scripts/verify-packages.mjs`
  with the actual pack generation/export mode, never publication or
  `--download`. Preserve its producer and checksum sidecars as evidence;
  stage only the nine verified tarballs in the adapter's npm directory.
  The current manifest emits the direct `rsrs-cli` launcher and eight platform
  packages, with exact matching CLI versions. Use the reviewed version in the
  CLI Cargo manifest, lockfile, and npm artifact manifest for this lane.
  Windows packages are
  `@rsrsai/win-x64` and `@rsrsai/win-arm64`; `win32` is the Node platform key,
  not the package prefix. The historical `@rsrsai/cli` launcher and dependent
  `rsrs-cli` alias remain recovery-only, not additional packs in this lane.
- **Client2:** generate the pinned Go SDK before building. Use
  `apps/client2/scripts/build.ps1 -Version <version> -Arch <arch>` and
  `build-installer.ps1` on both native Windows targets; run `build.sh` on
  native Linux/macOS architectures and `build-dmg.sh` only for macOS Arm64
  after reviewing local dependencies. Install prerequisites outside the
  assembly adapter; do not rely on opportunistic cross-architecture Rosetta
  installation. Verify the six app assets, both Windows installers and the DMG.
  Produce combined SHA256SUMS, notices, release notes and
  `update-manifest.py v<version> ... 6` from the fully assembled on-disk bytes.
  Validate the native package-check receipt via `desktop-candidate.py`;
  `candidate.json` and `package-checks.json` stay evidence, not mirrored release
  payload. The byte-identical update manifest uses legacy-neutral version/URLs;
  the new client checks those and binds its download to the chosen outer tag.

No hosted build, package signing, publication, registry call, release upload
or protected environment action is performed by these local commands. Native
checks cannot be replaced by the synthetic assembly regression.

## Unified hosted dry run

The repository keeps three workflows. The existing
`release.yml` hosts the Runtime, CLI and Client2 matrices under
one read-only workflow. A maintainer can opt in a same-repository PR to
`main` with `native-release-dry-run`. The jobs check out the exact PR integration
SHA without persisting checkout credentials. Fork PRs do not run the native
jobs. Manual dry runs use the trusted `main` ref. A successful PR dry run does
not replace evidence from a later exact reviewed `main` commit.

Jobs upload short-lived Actions artifacts, not GitHub Release assets. Producer
payloads, native receipts and final byte-parity receipts have distinct names
bound to the run ID and attempt. Consumers accept only artifacts from that exact
run and attempt; a partial rerun must not pull from an earlier attempt to fill
gaps. There are no tag writes, registry calls, protected environments,
deployment or publication paths. Component-prefixed tag strings only select
the expected inventory and version; no such tag is created.

- **Runtime:** the candidate matrix contains Windows x64/ARM64, Linux GNU
  x64/ARM64 and macOS ARM64. On GNU Linux, the pinned provider must resolve from
  the package through an exact `$ORIGIN` loader path without a loader override.
  New lanes require their own real packaged smoke and source-bound receipts.
  macOS Intel and both Linux musl targets are excluded pending verified,
  target-specific ONNX Runtime inputs; the musl packages require static
  archives. No incomplete Runtime target set is assembled or reported as a
  complete Runtime dry run.
- **CLI:** eight native jobs produce the binary and provenance pair for each
  supported target. The no-publish packing job checks the same-attempt pairs
  and verifies the contents of the nine actual npm tarballs before local
  assembly. The planner inventory alone is not package acceptance.
- **Client2:** six native builds supply six app artifacts, two Windows
  installers and the macOS ARM64 DMG. The candidate binds those nine files to
  their checksums and update manifest. Six native acceptance jobs check the
  matching desktop artifacts; the applicable jobs also exercise the installed
  Windows app or an app copied out of the mounted macOS image. Final assembly
  requires the complete set of same-attempt native checks.

The hosted consumer uses `assemble-component-ci.mjs` with absolute payload
directories and a fresh output path under `RUNNER_TEMP`. It reuses the local
assembler and writes `component-release-receipt.json` for byte-identical
monorepo and legacy layouts. This receipt does not attest to native execution;
retain the separate native evidence. Missing files, failed verification or
missing receipts prevent a complete component result. The matrix and expected
inventory are not evidence that the newly added hosted jobs have passed.

### Runtime final same-attempt consumer

`assemble-runtime-release-ci.mjs` is a protected-by-inventory consumer, not
a new workflow job or publication path. Once all eight native lanes have
reviewed provider inputs and successful native smoke, run it only in the
trusted-main dispatch workspace. Download two separate artifact groups
from the current workflow run, without merging the artifact directories:

- `runtime-<lane>-evidence-<run>-<attempt>`: one existing native JSON receipt.
- `runtime-<lane>-payload-<run>-<attempt>`: exact Runtime executable, archive
  and schema-2 build metadata.

Pass the Runtime tag, absolute evidence root, absolute payload root and a
fresh output directory, all under `RUNNER_TEMP`. The consumer rejects a
missing or extra lane, wrong run or attempt, source/workflow mismatches,
wrong native host or smoke case coverage, and assets whose final byte
digests differ from the matching native smoke receipt. A digest mismatch
removes the owned local candidate. Upload at most the final receipt after
success; it is not a release or supplier attestation.

Do not enable this consumer for a partial matrix. The two musl producers
still need reviewed, native static ONNX Runtime inputs (not the GNU
provider), and native Intel macOS needs a reviewed 1.28-compatible provider,
not an older binary or a Rosetta result. Their builders must verify the
provider source/pin and redistribution notices before packaging; receipt
digest presence alone does not approve a provider. The unimplemented lanes'
receipt format does not assert that hosted smoke or packaging has passed.
