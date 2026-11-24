#!/usr/bin/env bash
set -euo pipefail
target="${1:?pass the GNU Rust target}"
ort="${2:?pass an empty output directory}"
case "$target" in
  x86_64-unknown-linux-gnu) rid=linux-x64 ;;
  aarch64-unknown-linux-gnu) rid=linux-arm64 ;;
  *) printf 'Unsupported GNU target: %s\n' "$target" >&2; exit 2 ;;
esac
core="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../core" && pwd)"
mkdir -- "$ort"
curl --fail --silent --show-error --location \
  https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime/1.28.0/microsoft.ml.onnxruntime.1.28.0.nupkg \
  --output "$ort/onnxruntime.nupkg"
echo '769d1d3ea8ab6cd69f737c9dd4d4462aa4ad0ccfa106eaf506efc40d7bead5db  onnxruntime.nupkg' |
  (cd "$ort" && sha256sum --check --strict)
python3 - "$ort" "$rid" "$core/third-party" <<'PY'
from pathlib import Path
import sys
import xml.etree.ElementTree as ET
import zipfile

root, rid, notices = Path(sys.argv[1]), sys.argv[2], Path(sys.argv[3])
with zipfile.ZipFile(root / "onnxruntime.nupkg") as package:
    metadata = ET.fromstring(package.read("Microsoft.ML.OnnxRuntime.nuspec"))
    repository, = [node.attrib for node in metadata.iter() if node.tag.endswith("}repository")]
    if repository["commit"] != "da9b5e364c465de65c49d91e696cd6485270757f":
        raise RuntimeError("ONNX Runtime source revision changed")
    for member in ("LICENSE", "ThirdPartyNotices.txt"):
        notice = notices / f"onnxruntime-{member.removesuffix('.txt')}.txt"
        if package.read(member).replace(b"\r\n", b"\n") != notice.read_bytes().replace(b"\r\n", b"\n"):
            raise RuntimeError(f"ONNX Runtime notice changed: {member}")
    for name in ("libonnxruntime.so", "libonnxruntime_providers_shared.so"):
        (root / name).write_bytes(package.read(f"runtimes/{rid}/native/{name}"))
PY
readelf -d "$ort/libonnxruntime.so" | grep -F 'Library soname: [libonnxruntime.so.1]'
ln -- "$ort/libonnxruntime.so" "$ort/libonnxruntime.so.1"
