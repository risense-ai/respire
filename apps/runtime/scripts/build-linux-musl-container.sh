#!/bin/sh
# Link the in-repository Core with a verified static ONNX Runtime for musl.
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
case "${TARGET:?pass the Rust musl target}" in
  x86_64-unknown-linux-musl|aarch64-unknown-linux-musl) ;;
  *) echo 'Only the supported musl targets can use this build' >&2; exit 2 ;;
esac
test -d "${ORT_LIB_PATH:?provide the absolute verified static ORT directory}"
export OPENSSL_STATIC=1
export CARGO_HOME="${CARGO_HOME:-/workspace/target/musl-cache/$TARGET/cargo}"
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
