#!/bin/sh
# Link the in-repository Core with a verified static ONNX Runtime for musl.
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
case "${TARGET:?pass the Rust musl target}" in
  x86_64-unknown-linux-musl|aarch64-unknown-linux-musl) ;;
  *) echo 'Only the supported musl targets can use this build' >&2; exit 2 ;;
esac
test "${ORT_LIB_PATH:?missing static ORT directory}" = "/opt/respire-ort/$TARGET/${ORT_STATIC_SHA256:?missing approved static ORT digest}"
archive="$ORT_LIB_PATH/libonnxruntime.a"
test -f "$archive" && test ! -L "$archive" || exit 1
test "$(sha256sum < "$archive")" = "${ORT_STATIC_SHA256:?missing approved static ORT digest}  -" || exit 1
export ORT_SKIP_DOWNLOAD=1 ORT_PREFER_DYNAMIC_LINK=0
case "${RELEASE_CACHE_ACTION:-build}:${RELEASE_DEPENDENCY_CACHE:-}" in
  key:*|pack:*|build:1)
    node --input-type=module -e "import {cpSync} from 'node:fs'" || {
      echo 'Reviewed musl cache image requires a working Node runtime' >&2; exit 1
    } ;;
esac
export OPENSSL_STATIC=1
export CARGO_HOME="${CARGO_HOME:-$root/target/musl-cache/$TARGET/cargo}"
export RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+crt-static"
if [ "$TARGET" = 'aarch64-unknown-linux-musl' ]; then
  export CFLAGS_aarch64_unknown_linux_musl="${CFLAGS_aarch64_unknown_linux_musl:-} -mno-outline-atomics"
fi
cd "$root"
case "${RELEASE_CACHE_ACTION:-build}" in
  key|pack)
    node scripts/rust-ci-cache.mjs "$RELEASE_CACHE_ACTION" release "$TARGET"
    ;;
  candidate)
    cargo build --locked -p respire_runtime --bin rsrs-runtime --target "$TARGET" --target-dir target/ci
    ;;
  build)
    if [ "${RELEASE_DEPENDENCY_CACHE:-}" = 1 ]; then node scripts/rust-ci-cache.mjs restore release "$TARGET"; fi
    cargo build --release --locked -p respire_runtime --bin rsrs-runtime --target "$TARGET" --target-dir target/ci
    ;;
  *) echo 'Invalid release cache action' >&2; exit 1 ;;
esac
