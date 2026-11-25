# Stage a local Runtime build

Run from a clean checkout. Build the requested Runtime target with the existing
locked native or musl builder first. Use its exact profile output directory
and the `build/respire_core_sdk-*/out/source-build.json` for that build invocation
(as printed by Cargo's build-script-executed JSON message). Do not pass an
unrelated receipt, cached binary, or arbitrary files from another build.

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
