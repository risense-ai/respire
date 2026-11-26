"""First-observation inspection only. All candidate output is private until success."""
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import urllib.request
import xml.etree.ElementTree as ET
import zipfile

URL = "https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime/1.20.1/microsoft.ml.onnxruntime.1.20.1.nupkg"
SIZE = 108611147
MD5 = "sb+H46XbXvA4ipLstIbCIA=="
MIB = 1024 * 1024
PKG = "Microsoft.ML.OnnxRuntime"
LIB = "runtimes/osx-x64/native/libonnxruntime.dylib"
LIMITS = {LIB:64*MIB, "LICENSE":128*1024, "ThirdPartyNotices.txt":4*MIB,
          PKG+".nuspec":128*1024, ".signature.p7s":MIB}

def require(value, code):
    if not value:
        raise ValueError(code)

class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args):
        raise ValueError("REDIRECT")

def download(dest):
    h = hashlib.sha256()
    md = hashlib.md5(usedforsecurity=False)
    n = 0
    with urllib.request.build_opener(NoRedirect).open(URL,timeout=40) as r, dest.open("xb") as f:
        require(r.status==200 and r.geturl()==URL and r.headers.get_all("Content-Length")==[str(SIZE)],"LENGTH")
        require(r.headers.get("Content-Encoding","identity")=="identity","ENCODING")
        while data := r.read(65536):
            n += len(data)
            require(n<=SIZE,"SIZE")
            h.update(data)
            md.update(data)
            f.write(data)
    require(n==SIZE and base64.b64encode(md.digest()).decode()==MD5,"TRANSFER_CHECK")
    return h.hexdigest()

def inspect_package(path, out):
    hashes, seen, expanded = {}, set(), 0
    with zipfile.ZipFile(path) as z:
        require(0 < len(z.infolist()) <= 2048,"MEMBER_COUNT")
        for item in z.infolist():
            name = item.orig_filename
            parts = name.removesuffix("/").split("/")
            require(len(name)<=512 and all(p not in ("",".","..") for p in parts)
                    and not any(ord(c)<32 or ord(c)==127 or c in "\\:" for c in name)
                    and name.casefold().rstrip("/") not in seen,"PATH")
            seen.add(name.casefold().rstrip("/"))
            kind = (item.external_attr>>16)&0o170000
            require(kind in (0,0o040000,0o100000) and not item.flag_bits&1
                    and item.compress_type in (0,8),"TYPE")
            expanded += item.file_size
            require(item.file_size<=256*MIB and expanded<=768*MIB,"EXPANSION")
            if name in LIMITS:
                require(not item.is_dir() and 0<item.file_size<=LIMITS[name],"SELECTED_SIZE")
                b=z.read(item)
                require(len(b)==item.file_size,"READ")
                hashes[name]=hashlib.sha256(b).hexdigest()
                if name.endswith(".nuspec"):
                    require(b"<!" not in b.upper(),"XML_DECL")
                    node=ET.fromstring(b)
                    require(node.tag.split("}")[-1]=="package","NUSPEC")
                    metas=node.findall("{*}metadata")
                    require(len(metas)==1,"NUSPEC")
                    for key,val in [("id",PKG),("version","1.20.1")]:
                        children=metas[0].findall("{*}"+key)
                        require(len(children)==1 and children[0].text==val,"PACKAGE_ID")
                elif name in (LIB,"LICENSE","ThirdPartyNotices.txt"):
                    with (out/name.split("/")[-1]).open("xb") as f:
                        f.write(b)
    require(set(hashes)==set(LIMITS),"MEMBER_SET")
    return hashes

def signature_evidence(log):
    require(len(log) <= 128*1024 and "Successfully verified package" in log
            and ("Signature type: Repository" in log or "RepositoryCountersignature" in log)
            and "https://api.nuget.org/v3/index.json" in log
            and not re.search(r"(?i)\b(?:warning|error)\s+NU\d+", log), "SIGNATURE_EVIDENCE")

def run():
    out=Path(sys.argv[1])
    path=out/(PKG+".1.20.1.nupkg")
    package_hash=download(path)
    require(package_hash=="9c32108e9a1c78b090a95c2a219f244dd5f067d4caf68fb0f967103f3f20839c","PACKAGE_PIN")
    # Require trusted signing verification with the hosted .NET SDK before using members.
    proof=subprocess.run(["dotnet","nuget","verify",str(path),"--all","--verbosity","detailed"],
                   check=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=180,text=True)
    signature_evidence(proof.stdout)
    hashes=inspect_package(path,out)
    require(hashes=={
        LIB:"2d2306de2ea691044b0afa552d1b8ffb0e51baee3172d0988be587f0b68f27a0",
        "LICENSE":"c250d6278f0b47a6439fb7592b08b58a55eb9f535aa49a1db63211c3f982b674",
        "ThirdPartyNotices.txt":"fac5bb85f568b38fd8f3a6aea32737985841f23a1a43e9ce952f79474f883aa9",
        PKG+".nuspec":"663875bd17b51264801ad4dc68fc98ba50658693da6bf4037c8b093156345a48",
        ".signature.p7s":"623fb4711f8b76b49de97d95e66ef3cf098d4821ff4b8ae209b9fbc04f6e6d96"
    },"MEMBER_PIN")
    lib=out/"libonnxruntime.dylib"
    def inspect(*args):
        return subprocess.check_output(args,timeout=25,stderr=subprocess.DEVNULL,text=True)
    require(inspect("/usr/bin/lipo","-archs",lib).strip()=="x86_64","MACH_ARCH")
    ident=inspect("/usr/bin/otool","-D",lib).splitlines()[1:]
    deps=inspect("/usr/bin/otool","-L",lib).splitlines()[1:]
    require(len(ident)==1 and 0<len(deps)<=128,"MACH_LAYOUT")
    # Fingerprint load commands for review; never echo untrusted Mach-O strings.
    rows=[row.strip().split(" (compatibility version",1)[0] for row in deps]
    require(all(re.fullmatch(r"[A-Za-z0-9@_./+-]{1,512}",s) for s in ident+rows),"MACH_NAME")
    require(len(rows)==8 and all(s==ident[0] or s.startswith(("/usr/lib/","/System/Library/")) for s in rows),"DYLIB_DEPS")
    print(json.dumps(dict(scope="signed-package-static-observation",package_id=PKG,version="1.20.1",
        official_url=URL,package_bytes=SIZE,package_sha256=package_hash,member_sha256=hashes,
        architecture="x86_64",install_name_sha256=hashlib.sha256(ident[0].encode()).hexdigest(),
        dependencies_sha256=hashlib.sha256("\n".join(rows).encode()).hexdigest(),dependency_count=len(rows),
        non_system_dependency_count=sum(s!=ident[0] and not s.startswith(("/usr/lib/","/System/Library/")) for s in rows),
        run_id=os.environ["GITHUB_RUN_ID"],run_attempt=os.environ["GITHUB_RUN_ATTEMPT"]),sort_keys=True))
if __name__=="__main__":
    try: run()
    except Exception as e:
        code=str(e) if isinstance(e,ValueError) and re.fullmatch("[A-Z_]+",str(e)) else "INSPECTION_FAILED"
        sys.exit(code)
