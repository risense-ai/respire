"""No-load archive inspection. Never extract paths from the archive."""
import gzip
import hashlib
import io
import pathlib
import re
import subprocess
import sys
import tarfile

ROOT = "onnxruntime-osx-x86_64-1.20.1"
LIB = "lib/libonnxruntime.1.20.1.dylib"
PIN = "0f73006813af2a1a5d1723ed7dfb694fc629d15037124081bb61b7bf7d99fc78"
MIB = 1024 * 1024

def require(value, code):
    if not value:
        raise ValueError(code)

def inspect_archive(path, notices, output):
    require(path.stat().st_size <= 64 * MIB, "compressed size")
    data = path.read_bytes()
    require(hashlib.sha256(data).hexdigest() == PIN, "archive digest")
    with gzip.GzipFile(fileobj=io.BytesIO(data)) as compressed:
        plain = compressed.read(128 * MIB + 1)
    require(len(plain) <= 128 * MIB, "expanded size")
    wanted = {LIB, "LICENSE", "ThirdPartyNotices.txt", "include/onnxruntime_c_api.h"}
    seen, selected = set(), {}
    with tarfile.open(fileobj=io.BytesIO(plain), mode="r:") as archive:
        for member in archive:
            parts = member.name.split("/")
            require(parts[0] == ROOT and all(p not in ("", ".", "..") for p in parts)
                    and member.name not in seen and len(seen) < 512, "member layout")
            seen.add(member.name)
            name = "/".join(parts[1:])
            if member.issym():
                require(name == "lib/libonnxruntime.dylib" and member.linkname == LIB.split("/")[-1], "link")
                continue
            require(member.isdir() or member.isfile(), "member type")
            if name in wanted:
                require(member.isfile() and 0 < member.size <= 64 * MIB, "selected member")
                selected[name] = archive.extractfile(member).read()
    require(set(selected) == wanted, "missing members")
    require(re.findall(rb"(?m)^#define ORT_API_VERSION (\d+)\s*$",
                       selected["include/onnxruntime_c_api.h"]) == [b"20"], "archive API")
    for name in ("LICENSE", "ThirdPartyNotices.txt"):
        approved = notices / ("onnxruntime-" + name.removesuffix(".txt") + ".txt")
        require(selected[name] == approved.read_bytes(), "notice mismatch")
    with output.open("xb") as file:
        file.write(selected[LIB])
    return hashlib.sha256(selected[LIB]).hexdigest()

def feature_check(tree):
    versions = {int(v) for v in re.findall(r'ort(?:-sys)? feature "api-(\d+)"', tree)}
    require(versions and max(versions) == 20, "resolved API")

if __name__ == "__main__":
    if sys.argv[1] == "tree":
        feature_check(pathlib.Path(sys.argv[2]).read_text())
    else:
        print("dylib_sha256", inspect_archive(*(pathlib.Path(p) for p in sys.argv[1:])))
