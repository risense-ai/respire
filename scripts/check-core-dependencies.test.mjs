import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { checkActiveEdges, checkCoreDependencies, parseCargoTree } from "./check-core-dependencies.mjs";

const WINDOW = "x86_64-pc-windows-msvc";
const registry = "registry+https://github.com/rust-lang/crates.io-index";
const reviewed = new Map([
  ["ort-sys@2.0.0-rc.13@" + registry, "cf211e3776eea6aec988552fa118dd746d70e1b1e5e244058d1c98015f3e5872"],
  ["ureq@3.4.2@" + registry, "9a7ac20be9b7726e0bbdbf974c059676d9acb1cd414961f570a4e8231cacd7fc"],
  ["ureq-proto@0.6.4@" + registry, "f86fd172ccca569e458f61b6bdd6220965a9ef36e672a6852953b51a0e1583be"],
]);
const last = String.fromCharCode(96) + "-- ";
const active = [
  "PKG=respire_core v1.0.0 (/fixture/core)|FEAT=",
  last + "PKG=ort-sys v2.0.0-rc.13|FEAT=download-binaries,__tls",
  "    [build-dependencies]",
  "    " + last + "PKG=ureq v3.4.2|FEAT=_tls,native-tls,native-tls-no-default,native-tls-webpki-roots,socks-proxy",
  "        " + last + "PKG=ureq-proto v0.6.4|FEAT=client",
];

test("only the exact reviewed ORT build archive chain is accepted", () => {
  const result = checkActiveEdges(parseCargoTree(active.join("\n")), reviewed, WINDOW);
  assert.deepEqual(result, {diagnostics:[],allowed:2});
  for (const lines of [
    active.filter(line=>!line.includes("[build-dependencies]")),
    active.map(line=>line.replace("build-dependencies","dev-dependencies")),
    active.map(line=>line.replace("3.4.2|","3.4.3|")),
    active.map(line=>line.replace("socks-proxy", "socks-proxy,new-network-feature")),
    active.map(line=>line.replace("2.0.0-rc.13|", "2.0.0-rc.13 (/fixture/unreviewed)|")),
    active.map(line=>line.replace("download-binaries,__tls", "new-fetch,__tls")),
    active.map(line=>line.replace("respire_core v1", "unreviewed_root v1")),
  ]) assert.match(checkActiveEdges(parseCargoTree(lines.join("\n")),reviewed,WINDOW).diagnostics[0],/forbidden/);
  assert.throws(()=>parseCargoTree(active.join("\n")+" (*)"),/deduplicated/);
  const bad=new Map(reviewed);
  bad.set("ureq@3.4.2@"+registry,"0".repeat(64));
  assert.match(checkActiveEdges(parseCargoTree(active.join("\n")),bad,WINDOW).diagnostics[0],/forbidden build/);
});

test("even a resolver that omits the HTTP crate cannot allow ort/fetch-models", () => {
  const rows = parseCargoTree("PKG=respire_core v1.0.0|FEAT=\n" + last +
    "PKG=ort v2.0.0-rc.13|FEAT=std,fetch-models\n");
  assert.match(checkActiveEdges(rows,reviewed,WINDOW).diagnostics[0],/prohibited runtime fetch-models/);
});

function localFixture(body) {
  const temp=fs.realpathSync.native(os.tmpdir());
  const root=fs.realpathSync.native(fs.mkdtempSync(path.join(temp,"core-io-fixture-")));
  const write=(rel,text)=>{
    const full=path.join(root,rel);
    fs.mkdirSync(path.dirname(full),{recursive:true});
    fs.writeFileSync(full,text);
  };
  const pkg=(member,name,extra)=>{
    write(member+"/Cargo.toml","[package]\nname = "+JSON.stringify(name)+
      "\nversion = \"0.0.1\"\nedition = \"2021\"\n"+extra);
    write(member+"/src/lib.rs","pub fn synthetic() {}\n");
  };
  pkg("deps/http","ureq","");
  const manifest=path.join(root,"Cargo.toml");
  const report=(extra,helper="")=>{
    write("Cargo.toml","[package]\nname = \"review-fixture\"\nversion = \"0.0.1\"\nedition = \"2021\"\n"+extra);
    write("src/lib.rs","pub fn synthetic() {}\n");
    if (helper) pkg("deps/bridge","bridge",helper);
    execFileSync("cargo",["generate-lockfile","--offline","--manifest-path",manifest],
      {cwd:root,stdio:["ignore","pipe","pipe"]});
    return checkCoreDependencies({root,manifest,targets:[WINDOW],offline:true});
  };
  try { body(report); }
  finally {
    assert.ok(path.basename(root).startsWith("core-io-fixture-")&&path.relative(temp,root)===path.basename(root));
    fs.rmSync(root,{recursive:true});
  }
}

test("real Cargo workspace rejects renamed normal, build and dev dependencies", ()=>localFixture(report=>{
  for (const [section,kind] of [["dependencies","normal"],["build-dependencies","build"],["dev-dependencies","dev"]]) {
    const actual=report("["+section+"]\nrenamed = {package=\"ureq\",path=\"deps/http\"}\n");
    assert.match(actual.reports[0],new RegExp("declared "+kind+" dependency ureq \\(alias renamed\\)"));
  }
}));

test("all-feature Cargo graph detects optional transitive runtime networking", ()=>localFixture(report=>{
  const actual=report("[dependencies]\nbridge={path=\"deps/bridge\"}\n[features]\nnetwork=[\"bridge/fetch\"]\n",
    "[dependencies]\ntransport={package=\"ureq\",path=\"../http\",optional=true}\n[features]\nfetch=[\"dep:transport\"]\n");
  assert.match(actual.reports[0],/forbidden normal dependency review-fixture@.*bridge@.*ureq@/);
}));

test("an unrelated build tool or workspace dev path cannot reuse the exception", ()=>localFixture(report=>{
  const actual=report("[dependencies]\nbridge={path=\"deps/bridge\"}\n",
    "[build-dependencies]\nexternal={package=\"ureq\",path=\"../http\"}\n");
  assert.match(actual.reports[0], /forbidden build dependency .*bridge@.*ureq@/);
  const dev=report("[dev-dependencies]\nbridge={path=\"deps/bridge\"}\n",
    "[dependencies]\nexternal={package=\"ureq\",path=\"../http\"}\n");
  assert.match(dev.reports[0], /forbidden normal dependency .*bridge@.*ureq@.*\(via dev\)/);
}));
