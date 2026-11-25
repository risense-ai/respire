import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, isAbsolute, join, relative } from "node:path";
import { test } from "node:test";
import { checkRustPanic, diffHunks, newSites } from "./check-rust-panic.mjs";

const site = (line, shape="abc", column=0) => ({line,column,method:"unwrap",shape});

test("an unchanged panic site survives line offsets, formatting and one-to-one replacement only", () => {
  const moved = diffHunks("@@ -2,0 +3,2 @@\n");
  assert.deepEqual(newSites([site(4)], [site(6)], moved), []);
  const hunk = diffHunks("@@ -3 +3,2 @@\n");
  assert.deepEqual(newSites([site(3)], [site(3),site(4)], hunk), [site(4)]);
  assert.deepEqual(newSites([site(3)], [site(3,"different")], hunk), [site(3,"different")]);
});

test("line mapping tracks deletions and catches new AST at an unchanged identifier", () => {
  const removed = diffHunks("@@ -3 +2,0 @@\n");
  assert.deepEqual(newSites([site(4)], [site(3)], removed), []);
  assert.deepEqual(newSites([], [site(3)], removed), [site(3)]);
});

test("real Git history and native parser reject new production sites, not syntax lookalikes", () => {
  const parser=process.env.RESPIRE_RUST_PANIC_PARSER;
  assert.ok(parser&&isAbsolute(parser), "set RESPIRE_RUST_PANIC_PARSER to the locked native analyzer");
  const tmp=realpathSync.native(tmpdir()), repo=realpathSync.native(mkdtempSync(join(tmp,"rust-panic-unit-")));
  const git=(...args)=>execFileSync("git",["-c","user.name=Fixture","-c","user.email=fixture@example.invalid",
    "-c","commit.gpgSign=false",...args],{cwd:repo,encoding:"utf8",stdio:["ignore","pipe","pipe"]}).trim();
  const file=(name,value)=>writeFileSync(join(repo,name),value);
  const commit=()=>{git("add","-A");git("commit","-qm","Synthetic Rust guard fixture","--allow-empty");return git("rev-parse","HEAD")};
  try {
    git("init","-q");
    file("lib.rs",[
      "#[cfg(test)]",
      "fn fixture() { payload.unwrap(); }",
      "fn already() { payload.expect(\"old\"); }",
      "fn ambiguous() { let _ = payload.unwrap; }", "",
    ].join("\n"));
    const base=commit();
    file("lib.rs",[
      "#[cfg(test)]",
      "fn fixture() { payload.unwrap(); }",
      "fn already() { payload.expect(\"replacement diagnostic\"); } // okay",
      "fn ambiguous() { let _ = payload.unwrap; }",
      "// not a call: y.expect(\"fixture\")",
      "fn note() { let _ = r#\"x.unwrap()\"#; }", "",
    ].join("\n"));
    assert.deepEqual(checkRustPanic(repo,base,commit(),parser).violations,[]);
    file("lib.rs",[
      "fn fixture() { payload.unwrap(); }",
      "fn already() { payload.expect(\"old\"); }",
      "fn ambiguous() { let _ = payload.unwrap",
      "(); }",
      "fn new_fn() {",
      "    let value = payload",
      "        . /* comment */ expect",
      "        (\"synthetic\");",
      "    let callback = <Alias>::unwrap;",
      "}", "",
    ].join("\n"));
    file("new_test_named.rs","fn production(){ data.unwrap(); }\n");
    const report=checkRustPanic(repo,base,commit(),parser);
    assert.equal(report.checked,2);
    assert.deepEqual(report.violations.map(x=>x.replace(/:\d+: new/,": new")),[
      "lib.rs:1: new production unwrap site",
      "lib.rs:3: new production unwrap site",
      "lib.rs:7: new production expect site",
      "lib.rs:9: new production unwrap site",
      "new_test_named.rs:1: new production unwrap site",
    ]);
  } finally {
    assert.ok(basename(repo).startsWith("rust-panic-unit-")&&relative(tmp,repo)===basename(repo));
    rmSync(repo,{recursive:true});
  }
});
