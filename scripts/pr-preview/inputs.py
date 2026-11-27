"""Copy only bounded regular frontend blobs from the selected PR commit."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys

head, output = sys.argv[1:]
prefix = "apps/client2/internal/desktop/frontend/"
root = Path(output)
assets = root / "assets"
assets.mkdir(parents=True)
files = {}
total = 0
for entry in subprocess.check_output(["git", "ls-tree", "-rlz", head, "--", prefix]).split(b"\0"):
    if not entry:
        continue
    info, path = entry.decode().split("\t", 1)
    mode, kind, oid, length = info.split()
    name = path.removeprefix(prefix)
    if not path.startswith(prefix) or any(part in ("", ".", "..") for part in name.split("/")):
        raise ValueError("invalid frontend path")
    size = int(length)
    total += size
    if mode != "100644" or kind != "blob" or not 0 < size <= 8*1024**2 or total > 32*1024**2 or len(files) >= 1024:
        raise ValueError("invalid frontend inventory")
    data = subprocess.check_output(["git", "cat-file", "blob", oid])
    dest = assets / name
    dest.parent.mkdir(parents=True, exist_ok=True)
    with dest.open("xb") as f:
        f.write(data)
    files[name] = {"size":size, "sha256":hashlib.sha256(data).hexdigest()}
manifest = json.dumps({"schema":1, "source_sha":head, "files":files}).encode()
(root/"webui.json").write_bytes(manifest)
print(hashlib.sha256(manifest).hexdigest())
