#!/usr/bin/env bash
# Run on a Linux host of the target architecture with Docker installed.
# Builds the static rsrs-runtime musl binary (runtime repository variant of the
# CLI helper; package name and binary name are adapted).
set -euo pipefail

target="${1:?pass the Rust musl target}"
case "$target:$(uname -m)" in
  x86_64-unknown-linux-musl:x86_64)
    pin=${ORT_MUSL_X64_SHA256:?provide the approved x64 static archive digest}
    image_digest=${ORT_MUSL_X64_IMAGE_DIGEST:?provide reviewed x64 OCI digest}
    platform=linux/amd64 ;;
  aarch64-unknown-linux-musl:aarch64)
    pin=${ORT_MUSL_ARM64_SHA256:?provide the approved ARM64 static archive digest}
    image_digest=${ORT_MUSL_ARM64_IMAGE_DIGEST:?provide reviewed ARM64 OCI digest}
    platform=linux/arm64 ;;
  *) echo "unsupported target/host: $target/$(uname -m)" >&2; exit 1 ;;
esac

root="$(cd "$(dirname "$0")/.." && pwd)"
mkdir -p "$root/target/musl-cache/$target"
action="${2:-build}"
case "$action" in build|candidate|key|pack) ;; *) echo 'invalid release cache action' >&2; exit 1 ;; esac
archive=${ORT_STATIC_ARCHIVE:?provide the absolute approved static ORT archive path}
if [[ ! $pin =~ ^[0-9a-f]{64}$ || ! $image_digest =~ ^sha256:[0-9a-f]{64}$ ||
      $archive != /* || $(realpath -- "$archive") != "$archive" ]]; then
  echo 'Invalid archive path or reviewed digest' >&2; exit 1
fi
private=$(mktemp -d /tmp/respire-ort.XXXXXXXXXX)
trap 'rm -rf -- "$private"' EXIT
snapshot="$private/libonnxruntime.a"
timeout --kill-after=2s 60s python3 "$root/scripts/verify-musl-archive.py" "$archive" "$snapshot" "$pin" "$platform"
ort_dir="/opt/respire-ort/$target/$pin"
image="rust@$image_digest"
actual_platform=$(docker image inspect "$image" --format '{{.Os}}/{{.Architecture}}')
[[ $actual_platform == "$platform" ]] || { echo 'Reviewed image platform mismatch' >&2; exit 1; }
docker run --rm --pull=never --platform "$platform" \
  -v "$root:/workspace/runtime" -v "$root/../core:/workspace/core:ro" -w /workspace/runtime \
  --mount "type=bind,src=$snapshot,dst=$ort_dir/libonnxruntime.a,readonly" \
  -e ORT_LIB_PATH="$ort_dir" -e ORT_STATIC_SHA256="$pin" \
  -e TARGET="$target" -e RELEASE_CACHE_ACTION="$action" \
  -e RELEASE_DEPENDENCY_CACHE="${RELEASE_DEPENDENCY_CACHE:-}" \
  -e GITHUB_SHA -e GITHUB_RUN_ID -e GITHUB_RUN_ATTEMPT \
  -e RELEASE_CONTAINER_IMAGE="$image" -e GITHUB_OUTPUT=/workspace/runtime/.rust-release-output \
  "$image" sh scripts/build-linux-musl-container.sh
if [ "$action" = key ]; then
  cat "$root/.rust-release-output" >> "${GITHUB_OUTPUT:?release key requires GITHUB_OUTPUT}"
fi
case "$action" in
  build) profile=release ;;
  candidate) profile=debug ;;
  *) exit 0 ;;
esac

binary="$root/target/ci/$target/$profile/rsrs-runtime"
# A static ELF must have neither a dynamic loader nor shared-library imports.
if readelf -l "$binary" | grep -q INTERP; then
  echo 'musl binary unexpectedly requires an ELF interpreter' >&2
  exit 1
fi
if readelf -d "$binary" | grep -q NEEDED; then
  echo 'musl binary unexpectedly requires shared libraries' >&2
  exit 1
fi
"$binary" --version
# Exercise the same artifact on a musl userspace as well as the Ubuntu host.
docker run --rm -v "$binary:/rsrs-runtime:ro" alpine:3.23 /rsrs-runtime --version
