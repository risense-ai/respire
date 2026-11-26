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
export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1 DOTNET_CLI_UI_LANGUAGE=en-US
command -v dotnet >/dev/null
dotnet nuget verify --help >/dev/null
python3 -I "$inspect" "$stage"
