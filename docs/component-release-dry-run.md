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

## First hosted dry-run lane

`runtime-release-dry-run.yml` builds only the locked Windows x64 Runtime in a
fresh native runner directory. On an eligible same-repository PR to main, a
maintainer opts in with `native-release-dry-run`; this uses the ordinary PR
integration SHA with a read-only token, no repository checkout credentials,
and uploads only the run-bound smoke envelope. Fork PRs do not run this job.
Workflow dispatch on trusted main can also retain exactly three verified
assets as short-lived workflow artifacts; no release, tag, package registry,
or deployment is touched. PR evidence does not replace the exact reviewed main
commit's native evidence.

This is one of eight Runtime targets, not a release fan-out. Windows ARM64,
macOS Intel/ARM64, Linux GNU x64/ARM64, and Linux musl x64/ARM64 still need their
own package smoke and provenance. The two musl lanes additionally need verified
static ORT inputs for their architecture. The CLI and Client2 native matrices
and complete same-run artifact assembly are separate prerequisites.
