#!/usr/bin/env bash
set -euo pipefail
umask 077
test "${RUNNER_ENVIRONMENT:-}" = github-hosted
test "${RUNNER_OS:-}/${RUNNER_ARCH:-}" = macOS/X64
test "$(uname -m)" = x86_64
test "$(sysctl -in sysctl.proc_translated)" != 1
product="$(cd product && pwd)"
test "$(git -C "$product" rev-parse HEAD)" = ca7d3d309581017be32aa0a303740c2a011619c1
inspect="$(cd "$(dirname "$0")" && pwd)/intel.py"
stage=$(mktemp -d "$RUNNER_TEMP/card009.XXXXXXXX")
trap 'rm -rf -- "$stage"' EXIT
cd "$product/apps/runtime"
rustc -vV | tee "$stage/rust"
grep -Fx 'host: x86_64-apple-darwin' "$stage/rust"
grep -Fx 'release: 1.95.0' "$stage/rust"
xcodebuild -version
xcrun --show-sdk-version
for crate in ort ort-sys; do
  cargo tree --locked --target x86_64-apple-darwin -p respire_runtime -e features -i "$crate@2.0.0-rc.13" > "$stage/$crate.tree"
  python3 "$inspect" tree "$stage/$crate.tree"
  shasum -a 256 "$stage/$crate.tree"
done
url=https://github.com/microsoft/onnxruntime/releases/download/v1.20.1/onnxruntime-osx-x86_64-1.20.1.tgz
curl --proto '=https' --proto-redir '=https' --fail --silent --show-error --location --max-redirs 5 --max-time 120 --max-filesize 67108864 "$url" -o "$stage/intel.tgz"
lib="$stage/libonnxruntime.1.20.1.dylib"
python3 "$inspect" "$stage/intel.tgz" "$product/apps/core/third-party" "$lib"
test "$(lipo -archs "$lib")" = x86_64
otool -D "$lib"
otool -L "$lib"
otool -l "$lib" | awk '/cmd LC_RPATH/{p=3} p>0{print;p--}'
echo 'Static inspection completed. Dependency approval and packaged inference pending.'
