import hashlib
import json
import os
from pathlib import Path
import re
import signal
import struct
import subprocess
import sys
import tarfile

def must(value):
    if not value: raise ValueError("NATIVE_PROOF_FAILED")
def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()
def fixture(product,work):
    source=(product/"apps/runtime/crates/core-sdk/src/worker/candidate/tests.rs").read_text()
    model=bytes.fromhex("".join(re.findall('"([0-9a-f]+)"',source.split("let literal = concat!(",1)[1].split(");",1)[0])))
    tokens=source.split('let tokens = br#"',1)[1].split('"#;',1)[0].encode()
    def store(name,b):
        p=work/name
        p.write_bytes(b)
        return dict(path=str(p),sha256=sha(p),size=len(b))
    return dict(model="fixture-candidate",backend="onnx",engine="cpu",backend_profile=dict(id="onnx",backend="onnx"),
      profile=dict(id="fixture-candidate",embedding=dict(model_id="fixture-candidate",revision="r1",dimensions=4,
      max_tokens=32,pooling="sentence_embedding",normalize=True,query_prefix="",document_prefix="",pad_token_id=0,
      pad_to_max_tokens=False),onnx=dict(input_ids="input_ids",output="embedding"),weights=store("identity.onnx",model),
      tokenizer=store("tokenizer.json",tokens)))

def main(product,stage,archive):
    target="x86_64-apple-darwin"
    payload=stage/"payload"
    payload.mkdir()
    with tarfile.open(archive,"r:gz") as tar:
        for m in tar:
            parts=m.name.split("/")
            must(m.isfile() and len(parts)<=2 and (len(parts)==1 or parts[0]=="core-notices")
                 and all(re.fullmatch(r"[a-zA-Z0-9._+-]+",p) and p not in [".",".."] for p in parts))
            path=payload/m.name
            path.parent.mkdir(exist_ok=True)
            with path.open("xb") as f: f.write(tar.extractfile(m).read())
    binary=payload/("rsrs-runtime-"+target)
    binary.chmod(0o700)
    meta=json.loads((archive.parent/("runtime-build-"+target+".json")).read_bytes())
    must(meta["archive_sha256"]==sha(archive) and meta["binary_sha256"]==sha(binary))
    lib=payload/"libonnxruntime.1.20.1.dylib"
    must(sha(lib)=="2d2306de2ea691044b0afa552d1b8ffb0e51baee3172d0988be587f0b68f27a0")
    for name,d in [("LICENSE","c250d6278f0b47a6439fb7592b08b58a55eb9f535aa49a1db63211c3f982b674"),
             ("ThirdPartyNotices","fac5bb85f568b38fd8f3a6aea32737985841f23a1a43e9ce952f79474f883aa9")]:
        must(sha(payload/"core-notices"/("onnxruntime-"+name+".txt"))==d)
    must(subprocess.check_output(["/usr/bin/lipo","-archs",binary],text=True).strip()=="x86_64")
    deps=subprocess.check_output(["/usr/bin/otool","-L",binary],text=True).splitlines()[1:]
    must([s.strip().split(" (")[0] for s in deps if "libonnxruntime" in s]==["@rpath/libonnxruntime.1.20.1.dylib"])
    commands=subprocess.check_output(["/usr/bin/otool","-l",binary],text=True)
    must(re.findall(r'cmd LC_RPATH\s+cmdsize \d+\s+path (\S+) \(offset',commands)==["@executable_path"])
    data=stage/"candidate"
    data.mkdir()
    index=data/"index"
    index.mkdir()
    selection=fixture(product,data)
    must(selection["profile"]["weights"]["sha256"]=="291f57d2f44100a02bf83ed0339318e399f6d4d9fb5f2bea536e481824dd8a2d")
    must(selection["profile"]["tokenizer"]["sha256"]=="849c6e6797d8fe1b0b78f1fef1357d33c0d76b58c258c52be9fd1815aa59ac5a")
    frame=dict(id=1,operation="model_probe",payload=dict(model=selection["model"],text="Respire candidate load verification"),
               index_root=str(index),transport=False,selection=selection)
    env=dict(PATH="/usr/bin:/bin",HOME=str(data),TMPDIR=str(data),RSRS_DATA_DIR=str(data),RESPIRE_PRIVATE_CORE_CANDIDATE="1")
    proc=subprocess.Popen(["/usr/bin/sandbox-exec","-p","(version 1)(allow default)(deny network*)",binary,"--core-worker-internal"],
                          stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,cwd=data,env=env)
    def timeout(*_): raise TimeoutError("probe timeout")
    signal.signal(signal.SIGALRM,timeout)
    signal.alarm(120)
    try:
        b=json.dumps(frame).encode()
        proc.stdin.write(struct.pack("<Q",len(b))+b)
        proc.stdin.flush()
        for _ in range(600):
            n,=struct.unpack("<Q",proc.stdout.read(8))
            must(0<n<=1024*1024)
            msg=json.loads(proc.stdout.read(n))
            if msg.get("id")==1: break
            must("heartbeat" in msg)
        result=msg["result"]
        must(result["ready"] is True and result["selected"]=="CPU" and result["dimensions"]==4 and 0.99<=result["cosine"]<=1.001)
        files=subprocess.check_output(["/usr/sbin/lsof","-a","-p",str(proc.pid),"-d","txt","-Fn"],text=True).splitlines()
        mapped={Path(s[1:]).resolve() for s in files if s.startswith("n") and "libonnxruntime" in Path(s[1:]).name}
        must(mapped=={lib.resolve()} and "n"+str(binary) in files)
        print(json.dumps(dict(scope="packaged-runtime-offline-intel-inference",selected="CPU",dimensions=4,
          cosine=result["cosine"],binary_sha256=sha(binary),archive_sha256=sha(archive),loaded_provider_sha256=sha(lib),
          fixture_sha256=selection["profile"]["weights"]["sha256"],tokenizer_sha256=selection["profile"]["tokenizer"]["sha256"],
          source=os.environ["GITHUB_SHA"],run_id=os.environ["GITHUB_RUN_ID"],
          attempt=os.environ["GITHUB_RUN_ATTEMPT"])))
    finally:
        signal.alarm(0)
        proc.kill()
        proc.wait(timeout=10)
if __name__=="__main__": main(*(Path(s).resolve() for s in sys.argv[1:]))
