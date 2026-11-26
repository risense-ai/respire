#!/usr/bin/env bash
# Exercise both host/target pairs with minimal ELF headers and capture Docker argv.
set -euo pipefail
scripts=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/apps/runtime/scripts" "$tmp/bin"
cp "$scripts/build-linux-musl.sh" "$scripts/verify-musl-archive.py" "$tmp/apps/runtime/scripts/"
cat > "$tmp/bin/docker" <<'STUB'
#!/bin/sh
case "$1" in
  image) echo "$TEST_PLATFORM" ;;
  run)
    for arg do
      case "$arg" in
        type=bind,src=*)
          snapshot=${arg#type=bind,src=}
          snapshot=${snapshot%%,dst=*}
          test "$snapshot" != "$ORT_STATIC_ARCHIVE" || exit 1
          test "$(stat -c %a "$snapshot")" = 400 || exit 1
          test "$(stat -c %a "$(dirname "$snapshot")")" = 700 || exit 1
          cmp "$snapshot" "$ORT_STATIC_ARCHIVE" || exit 1
          ;;
      esac
    done
    printf '%s\n' "$@" > "$DOCKER_ARGS" ;;
  *) exit 1 ;;
esac
STUB
printf '#!/bin/sh\necho "$TEST_ARCH"\n' > "$tmp/bin/uname"
chmod +x "$tmp/bin/"*
export PATH="$tmp/bin:$PATH" DOCKER_ARGS="$tmp/docker.args"
unset ORT_MUSL_X64_SHA256 ORT_MUSL_ARM64_SHA256
run() { bash "$tmp/apps/runtime/scripts/build-linux-musl.sh" "$target" pack; }
reject() {
  rm -f "$DOCKER_ARGS"
  if run > /dev/null 2>&1; then echo 'Unexpected archive acceptance' >&2; exit 1; fi
  test ! -e "$DOCKER_ARGS"
}
for TEST_ARCH in x86_64 aarch64; do
  export TEST_ARCH
  target="$TEST_ARCH-unknown-linux-musl"
  case "$TEST_ARCH" in
    x86_64) platform=linux/amd64; machine=62; pin_var=ORT_MUSL_X64_SHA256 ;;
    aarch64) platform=linux/arm64; machine=183; pin_var=ORT_MUSL_ARM64_SHA256 ;;
  esac
  python3 - "$tmp/member.o" "$machine" <<'PY'
import struct, sys
ident = b'\x7fELF\x02\x01\x01' + bytes(9)
with open(sys.argv[1], 'wb') as member:
    member.write(struct.pack('<16sHHIQQQIHHHHHH', ident, 1, int(sys.argv[2]),
                             1, 0, 0, 0, 0, 64, 0, 0, 0, 0, 0))
PY
  export ORT_STATIC_ARCHIVE="$tmp/ORT $TEST_ARCH.a"
  ar rc "$ORT_STATIC_ARCHIVE" "$tmp/member.o"
  reject
  digest=$(sha256sum < "$ORT_STATIC_ARCHIVE")
  digest=${digest%% *}
  export "$pin_var=$digest"
  image_var=ORT_MUSL_X64_IMAGE_DIGEST
  if [[ $TEST_ARCH == aarch64 ]]; then image_var=ORT_MUSL_ARM64_IMAGE_DIGEST; fi
  unset "$image_var"
  reject
  export "$image_var=sha256:$digest"
  export TEST_PLATFORM=wrong
  reject
  export TEST_PLATFORM="$platform"
  run
  grep -Fx -- '--pull=never' "$DOCKER_ARGS"
  grep -Fx -- "rust@sha256:$digest" "$DOCKER_ARGS"
  dir="/opt/respire-ort/$target/$digest"
  mount=$(grep '^type=bind,src=' "$DOCKER_ARGS")
  [[ $mount == type=bind,src=/tmp/respire-ort.*/libonnxruntime.a,dst="$dir/libonnxruntime.a",readonly ]]
  snapshot=${mount#type=bind,src=}
  test ! -e "${snapshot%%,dst=*}"
  grep -Fx -- "ORT_LIB_PATH=$dir" "$DOCKER_ARGS"
  grep -Fx -- "ORT_STATIC_SHA256=$digest" "$DOCKER_ARGS"
  grep -Fx -- "$platform" "$DOCKER_ARGS"
  grep -Fx -- "TARGET=$target" "$DOCKER_ARGS"
  ORT_STATIC_ARCHIVE="$tmp/missing" reject
  export "$pin_var=$(printf '%064d' 0)"
  reject
done
export ORT_STATIC_ARCHIVE="$tmp/ORT x86_64.a"
digest=$(sha256sum < "$ORT_STATIC_ARCHIVE")
export ORT_MUSL_ARM64_SHA256=${digest%% *}
reject
target=x86_64-unknown-linux-musl
reject
