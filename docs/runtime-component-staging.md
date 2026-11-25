# Stage a local Runtime build

Run from a clean checkout. Build the requested Runtime target with the existing
locked native or musl builder first. Read the exact
`build/respire_core_sdk-*/out/source-build.json` from that invocation's Cargo
`build-script-executed` JSON message. Cargo can hardlink its profile executable
to the copy in `deps`; the staging tool deliberately refuses multiply linked files.

In a fresh, private scratch profile outside Cargo's target directory, copy
(don't link) only `rsrs-runtime` (or `rsrs-runtime.exe`), each basename listed in
`runtime_files` from the source receipt, and the receipt itself. Preserve its
`build/respire_core_sdk-<hash>/out/source-build.json` location inside the scratch
profile. After copying, check the SHA-256 of each copy against its original;
the stager also verifies the native resources against the receipt pins.
Do not copy metadata or files from another invocation. Pass the scratch profile
and the copied receipt as the build inputs below.

```
node apps/runtime/scripts/stage-runtime-release.mjs <runtime-component-tag> <checkout-sha> <target-triple> <absolute-build-dir> <absolute-source-build.json> <absolute-output-parent>
```

The command writes a local batch record compatible with the component assembler.
The batch contains only the raw Runtime, deterministic ustar/gzip payload and
schema-2 report. Native runtime files remain beside the executable because the
source-linked probe and loader use that layout. Supplier notices and the
current source license are copied; no previous binary SDK is needed. Use the
same pinned Node toolchain for compressed-byte reproducibility. No publication,
tag creation, or network calls are made. Batch preparation does not replace
same-run build provenance, target-specific dependency inspection, or the
existing native hosted release smoke. Supply the exact three batch files as the
smoke's `release-assets` and its usual `RUNTIME_TARGET`/`RUNTIME_VERSION`.

## Automating the Cargo staging handoff

In a native build job with its reviewed target-specific provider setup, use a
fresh absolute Cargo target directory and capture the structured stdout from
the **same** successful release invocation:

```sh
cargo +1.95.0 build --release --locked --manifest-path apps/runtime/Cargo.toml \
  -p respire_runtime --bin rsrs-runtime --target "$TARGET" \
  --target-dir "$CARGO_TARGET_DIR" --message-format=json-render-diagnostics > "$CARGO_LOG"
node apps/runtime/scripts/stage-runtime-cargo.mjs "$COMPONENT_TAG" "$SOURCE_SHA" \
  "$TARGET" "$CARGO_TARGET_DIR" "$CARGO_LOG" "$OUTPUT_PARENT"
```

The adapter resolves the unique selected-target image and source receipt from
that Cargo invocation, makes byte-verified copies, and runs the same strict
stager above. Keep the tracked Runtime manifest and lockfile at the requested
release version; do not rewrite them from dispatch input. The log is not
an attestation. The job still owns native runner/target selection, trusted
provider setup, a same-run provenance envelope, and packaged Runtime smoke.
