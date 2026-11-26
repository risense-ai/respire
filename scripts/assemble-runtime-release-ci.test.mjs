import assert from "node:assert/strict";
import {createHash} from "node:crypto";
import {access, mkdir, mkdtemp, readFile, rename, rm, writeFile} from "node:fs/promises";
import {tmpdir} from "node:os";
import {join} from "node:path";
import {test} from "node:test";
import {assembleRuntimeNative} from "./assemble-runtime-release-ci.mjs";

const tag = "runtime-v2.0.0-dev.31";
const provenance = {
  repository:"risense-ai/respire",event:"workflow_dispatch",source:"c".repeat(40),workflow_sha:"d".repeat(40),
  workflow_ref:"risense-ai/respire/.github/workflows/runtime-release-dry-run.yml@refs/heads/main",run_id:"12345",run_attempt:"2",
};
const profiles = [
  ["windows-x64","x86_64-pc-windows-msvc","Windows","x64"],
  ["windows-arm64","aarch64-pc-windows-msvc","Windows","arm64"],
  ["linux-gnu-x64","x86_64-unknown-linux-gnu","Linux","x64","adjacent-dynamic"],
  ["linux-gnu-arm64","aarch64-unknown-linux-gnu","Linux","arm64","adjacent-dynamic"],
  ["macos-intel","x86_64-apple-darwin","macOS","x64","adjacent-dynamic"],
  ["macos-arm64","aarch64-apple-darwin","macOS","arm64","static"],
];
const digest = value=>createHash("sha256").update(value).digest("hex");
const caseResults = [
  ["host-control-no-wall-clock-deadline","delayed-input-survives-and-closed-input-is-rejected"],
  ["client-json-bad-config","scoped-structured-invalid-config-envelope","invalid-client-config-bytes-preserved",
    "inference-settings-preserved-on-rejection","scoped-recovery-preserves-inference-settings"],
  ["remember-result","recover-original-receipt-and-read-back-without-replay"],
  ["stop-clean-drained","standalone-stop-accepted","standalone-normal-exit","standalone-identity-bound-final-drained-receipt",
    "standalone-listener-closed-after-exit"],
  ["mcp-initialize-tools-list","initialize","tools/list","tools/call:memory_status","same-process-after-call"],
].flatMap(([testID,...names])=>names.map(name=>({test_id:"runtime."+testID,name,status:"passed"})));

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(),"native-assembly-synthetic-"));
  t.after(()=>rm(root,{recursive:true,force:true}));
  const evidence = join(root,"evidence"), payload = join(root,"payload"), output = join(root,"assembled");
  await mkdir(evidence);
  await mkdir(payload);
  async function stage([lane,target,os,arch,loader]) {
    const e=join(evidence,`runtime-${lane}-evidence-12345-2`), p=join(payload,`runtime-${lane}-payload-12345-2`);
    await mkdir(e);
    await mkdir(p);
    const image=`rsrs-runtime-${target}${os==="Windows"?".exe":""}`, arc=`rsrs-runtime-${target}.tar.gz`;
    const binary="synthetic binary "+target, packageBytes="synthetic archive "+target;
    await writeFile(join(p,image),binary);
    await writeFile(join(p,arc),packageBytes);
    const meta=`runtime-build-${target}.json`;
    const data=JSON.stringify({schema_version:2,runtime_version:"2.0.0-dev.31",tag:"v2.0.0-dev.31",channel:"dev",
      target_triple:target,source_commit:provenance.source,archive_file:arc,archive_sha256:digest(packageBytes),
      binary_file:image,binary_sha256:digest(binary)});
    await writeFile(join(p,meta),data);
    await writeFile(join(e,`runtime-${lane}.json`),JSON.stringify({schema:1,mode:"dry-run",...provenance,
      target,tag,native_identity:{runner_os:os,runner_arch:arch.toUpperCase(),node_arch:arch,rust_host:target.replace(/-musl$/,"-gnu")},
      native_loader:loader,provider_archive_sha256:lane.startsWith("linux-gnu")
        ?"769d1d3ea8ab6cd69f737c9dd4d4462aa4ad0ccfa106eaf506efc40d7bead5db":lane==="macos-intel"?"9c32108e9a1c78b090a95c2a219f244dd5f067d4caf68fb0f967103f3f20839c":"e".repeat(64),
      packaged_inference:lane==="macos-intel"?{scope:"packaged-runtime-offline-intel-inference",source:provenance.source,
        run_id:provenance.run_id,attempt:provenance.run_attempt,selected:"CPU",dimensions:4,cosine:1,
        loaded_provider_sha256:"2d2306de2ea691044b0afa552d1b8ffb0e51baee3172d0988be587f0b68f27a0",
        fixture_sha256:"291f57d2f44100a02bf83ed0339318e399f6d4d9fb5f2bea536e481824dd8a2d",tokenizer_sha256:"849c6e6797d8fe1b0b78f1fef1357d33c0d76b58c258c52be9fd1815aa59ac5a",
        binary_sha256:digest(binary),archive_sha256:digest(packageBytes)}:undefined,
      packaged_native_smoke:{passed:true,case_results:caseResults},
      artifact_sha256:{[image]:digest(binary),[arc]:digest(packageBytes),[meta]:digest(data)},
    }));
  }
  for (const profile of profiles) await stage(profile);
  async function alter(lane, change) {
    const path=join(evidence,`runtime-${lane}-evidence-12345-2`,`runtime-${lane}.json`);
    const body=JSON.parse(await readFile(path,"utf8"));
    change(body);
    await writeFile(path,JSON.stringify(body));
  }
  return {evidence,payload,output,alter,run:(identity=provenance)=>assembleRuntimeNative({
    tag,provenance:identity,evidenceRoot:evidence,payloadRoot:payload,output,
  })};
}

test("synthetic complete native matrix binds each asset and writes exact mirror evidence",async t=>{
  const {run,output,evidence}=await fixture(t);
  const result=await run();
  assert.equal(Object.keys(result.receipt.assets).length,18);
  assert.deepEqual(result.provenance,provenance);
  const [official, mirror] = result.fanout.github;
  assert.deepEqual([official.repository, official.tag], ["risense-ai/respire", tag]);
  assert.deepEqual([mirror.repository, mirror.tag], ["risense-ai/respire-runtime", "v2.0.0-dev.31"]);
  assert.deepEqual(official.assets, mirror.assets);
  assert.equal(official.assets.length, 18);
  assert.deepEqual(result.fanout.npm, []);
  for (const [lane] of profiles) {
    const file = join(evidence, `runtime-${lane}-evidence-12345-2`, `runtime-${lane}.json`);
    assert.equal(result.native_receipt_sha256[lane], digest(await readFile(file)));
  }
  assert.equal(JSON.parse(await readFile(join(output,"component-release-receipt.json"),"utf8")).scope,
    "same-attempt-native-runtime-and-local-byte-parity");
});

test("missing target, foreign attempt, fork event, duplicate case and wrong native architecture fail closed",async t=>{
  const {run,output,evidence,alter}=await fixture(t);
  await assert.rejects(run({...provenance,event:"pull_request"}),/RUNTIME_NATIVE_RECEIPT_FAILED/);
  const lane=join(evidence,"runtime-macos-intel-evidence-12345-2");
  await rename(lane,lane+"-stale");
  await assert.rejects(run(),/RUNTIME_NATIVE_RECEIPT_FAILED/);
  await rename(lane+"-stale",lane);
  await alter("macos-intel",body=>{body.run_attempt="1";});
  await assert.rejects(run(),/RUNTIME_NATIVE_RECEIPT_FAILED/);
  await alter("macos-intel",body=>{body.run_attempt="2";body.native_identity.node_arch="arm64";});
  await assert.rejects(run(),/RUNTIME_NATIVE_RECEIPT_FAILED/);
  await alter("macos-intel",body=>{body.native_identity.node_arch="x64";body.packaged_native_smoke.case_results[0]=caseResults[1];});
  await assert.rejects(run(),/RUNTIME_NATIVE_RECEIPT_FAILED/);
  await assert.rejects(access(output),{code:"ENOENT"});
});

test("old provider and provenance mismatch cannot seal assembled bytes",async t=>{
  const {run,output,alter}=await fixture(t);
  await alter("linux-gnu-arm64",body=>{body.provider_archive_sha256="1".repeat(64);});
  await assert.rejects(run(),/RUNTIME_NATIVE_RECEIPT_FAILED/);
  await alter("linux-gnu-arm64",body=>{
    body.provider_archive_sha256="769d1d3ea8ab6cd69f737c9dd4d4462aa4ad0ccfa106eaf506efc40d7bead5db";
    body.artifact_sha256["runtime-build-aarch64-unknown-linux-gnu.json"]="0".repeat(64);
  });
  await assert.rejects(run(),/RUNTIME_NATIVE_RECEIPT_FAILED/);
  await assert.rejects(access(output),{code:"ENOENT"});
});

test("malformed, oversized and control-bearing receipts produce only a fixed failure", async t => {
  const {run, output, evidence} = await fixture(t);
  const file = join(evidence, "runtime-macos-intel-evidence-12345-2", "runtime-macos-intel.json");
  const valid = JSON.parse(await readFile(file, "utf8"));
  for (const bytes of ["UNTRUSTED_MARKER\x1b[31m", JSON.stringify({...valid, tag: "UNTRUSTED_MARKER\n"}),
    Buffer.alloc(65537, 65), Buffer.from([0xff])]) {
    await writeFile(file, bytes);
    await assert.rejects(run(), error => {
      assert.equal(error.message, "RUNTIME_NATIVE_RECEIPT_FAILED");
      assert.doesNotMatch(error.stack, /UNTRUSTED_MARKER|\x1b/);
      assert.equal(error.cause, undefined);
      return true;
    });
    await assert.rejects(access(output), {code: "ENOENT"});
  }
});

test("unsupported Runtime musl receipts and stale Intel inference fail closed", async t => {
  const {run, evidence, alter} = await fixture(t);
  const foreign = join(evidence, "runtime-linux-musl-x64-evidence-12345-2");
  await mkdir(foreign);
  await assert.rejects(run(), /RUNTIME_NATIVE_RECEIPT_FAILED/);
  await rm(foreign, {recursive:true});
  await alter("macos-intel", body => { body.packaged_inference.attempt = "1"; });
  await assert.rejects(run(), /RUNTIME_NATIVE_RECEIPT_FAILED/);
});

test("Intel proof must describe the assembled archive, binary, provider and fixture", async t => {
  for (const key of ["archive_sha256","binary_sha256","loaded_provider_sha256","fixture_sha256","tokenizer_sha256"]) {
    const {run, alter, output} = await fixture(t);
    await alter("macos-intel", body => { body.packaged_inference[key] = "0".repeat(64); });
    await assert.rejects(run(), /RUNTIME_NATIVE_RECEIPT_FAILED/);
    await assert.rejects(access(output), {code:"ENOENT"});
  }
});
