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
target=x86_64-apple-darwin
export ORT_LIB_PATH="$stage" ORT_SKIP_DOWNLOAD=1 ORT_PREFER_DYNAMIC_LINK=1
for crate in ort ort-sys; do
  cargo tree --locked --target "$target" -p respire_runtime -e features -i "$crate@2.0.0-rc.13" > "$stage/$crate.tree"
  python3 -I - "$stage/$crate.tree" <<'PY'
import re,sys
versions={int(i) for i in re.findall(r'ort(?:-sys)? feature "api-(\d+)"',open(sys.argv[1]).read())}
assert versions and max(versions)==20
PY
done
cargo fetch --locked --target "$target"
export CARGO_NET_OFFLINE=true
cargo build --release --frozen --target "$target" --target-dir "$stage/target" -p respire_runtime --bin rsrs-runtime --message-format=json-render-diagnostics > "$stage/cargo.jsonl"
test -z "$(git -C "$product" status --porcelain --untracked-files=no)"
# A private staging view supplies the package-specific notices. Cargo built the original checkout.
mkdir "$stage/view"
git -C "$product" archive HEAD scripts apps/runtime/scripts apps/core/third-party apps/core/LICENSE apps/runtime/Cargo.lock apps/runtime/runtime/Cargo.toml | tar -x -C "$stage/view"
cp "$stage/LICENSE" "$stage/view/apps/core/third-party/onnxruntime-LICENSE.txt"
cp "$stage/ThirdPartyNotices.txt" "$stage/view/apps/core/third-party/onnxruntime-ThirdPartyNotices.txt"
# Preserve the signed dylib bytes and honor the supplier's versioned LC_ID_DYLIB.
python3 -I - "$stage/view/apps/runtime/scripts/stage-runtime-release.mjs" <<'PYCODE'
from pathlib import Path
import sys
p=Path(sys.argv[1])
old="for (const [name, pin] of runtimeFiles) await add(name, join(build, name), pin);"
new="""for (const [name, pin] of runtimeFiles) {
      const packaged = name === 'libonnxruntime.dylib' && target === 'x86_64-apple-darwin'
        ? 'libonnxruntime.1.20.1.dylib' : name;
      await add(packaged, join(build, name), pin);
    }"""
s=p.read_text()
if s.count(old)!=1: raise SystemExit("STAGING_SOURCE_DIFFERED")
p.write_text(s.replace(old,new))
PYCODE
node --input-type=module - "$product" "$stage" <<'JS'
import {pathToFileURL} from 'node:url';
import {realpathSync} from 'node:fs';
const [product, stage]=process.argv.slice(2).map(p=>realpathSync(p));
const {stageRuntimeCargo}=await import(pathToFileURL(stage+'/view/apps/runtime/scripts/stage-runtime-cargo.mjs'));
await stageRuntimeCargo({root:stage+'/view',tag:'runtime-v2.0.0-dev.31',source:'ca7d3d309581017be32aa0a303740c2a011619c1',
 target:'x86_64-apple-darwin',targetDir:stage+'/target',cargoLog:stage+'/cargo.jsonl',outputParent:stage});
JS
python3 -I "$(dirname "$inspect")/proof.py" "$product" "$stage"
