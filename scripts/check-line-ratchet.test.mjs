import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, realpathSync, rmSync, writeFileSync, renameSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join, relative } from "node:path";
import { test } from "node:test";
import { checkLineRatchet, countSourceLines, parseBaseline } from "./check-line-ratchet.mjs";

function utf8(lines, delimiter = "\n", last = true) {
  return Buffer.from(Array(lines).fill("// fixture").join(delimiter) + (last && lines ? delimiter : ""));
}

test("counts exactly 599, 600, and 601 lines, with either newline encoding or final newline", () => {
  for (const n of [599, 600, 601]) {
    for (const nl of ["\n", "\r\n"]) {
      for (const last of [true, false]) assert.equal(countSourceLines(utf8(n, nl, last)), n);
    }
  }
  assert.equal(countSourceLines(Buffer.alloc(0)), 0);
  for (const b of [Buffer.from("x\0"), Buffer.from("x\ry"), Buffer.from([0xff])]) {
    assert.throws(() => countSourceLines(b));
  }
});

test("baseline rejects malformed, unsorted, duplicate, stale-small and unsafe path entries", () => {
  const b = text => Buffer.from(text);
  for (const text of [
    "{}",
    '{"schema":1,"ceilings":{"a.rs":601,"a.rs":601}}\n',
    '{"schema":1,"ceilings":{"a.rs":600}}\n',
    '{"schema":1,"ceilings":{"../a.rs":601}}\n',
    '{"schema":1,"ceilings":{"notes.txt":601}}\n',
    JSON.stringify({ schema: 1, ceilings: { "z.rs": 601, "a.rs": 700 } }, null, 2) + "\n",
  ]) assert.throws(() => parseBaseline(b(text)));
});

function fixture(callback) {
  const home = realpathSync.native(tmpdir());
  const root = realpathSync.native(mkdtempSync(join(home, "ratchet-unit-")));
  const git = (...args) => execFileSync("git", ["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
    "-c", "commit.gpgSign=false", "-c", "core.autocrlf=false", ...args], {
    cwd: root, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"],
  }).trim();
  const file = (path, bytes) => {
    const dest = join(root, path);
    mkdirSync(dirname(dest), { recursive: true });
    writeFileSync(dest, bytes);
  };
  const baseline = ceilings => file("scripts/line-ratchet-baseline.json", JSON.stringify({ schema: 1, ceilings }, null, 2) + "\n");
  const commit = () => {
    git("add", "-A");
    git("commit", "-qm", "Synthetic ratchet fixture", "--allow-empty");
    return git("rev-parse", "HEAD");
  };
  try {
    git("init", "-q");
    callback({ root, git, file, baseline, commit });
  } finally {
    assert.ok(basename(root).startsWith("ratchet-unit-") && relative(home, root) === basename(root));
    rmSync(root, { recursive: true });
  }
}

test("exact tracked Git blobs: permit shrink, require its new ceiling, allow at most 600 on a new path", () => fixture(
  ({ root, file, baseline, commit }) => {
    file("legacy.rs", utf8(610)); baseline({ "legacy.rs": 610 });
    const base = commit();
    file("legacy.rs", utf8(608)); file("at-cap.ts", utf8(600));
    assert.match(checkLineRatchet(root, base, commit()).violations.join("\n"), /exact 608-line/);
    baseline({ "legacy.rs": 608 });
    const shrunk = commit();
    assert.deepEqual(checkLineRatchet(root, base, shrunk).violations, []);
    file("legacy.rs", utf8(609)); baseline({ "legacy.rs": 609 });
    assert.match(checkLineRatchet(root, shrunk, commit()).violations.join("\n"), /exceeds previous ceiling 608/);
  }));

test("renames, materialized exceptions, above-cap excess and untrusted baselines fail closed", () => fixture(
  ({ root, file, baseline, commit }) => {
    file("legacy.go", utf8(610)); baseline({ "legacy.go": 610 });
    const base = commit();
    const before = join(root, "legacy.go");
    renameSync(before, join(root, "renamed.go"));
    file("new.js", utf8(601));
    baseline({ "new.js": 601, "renamed.go": 610 });
    const changed = commit();
    const messages = checkLineRatchet(root, base, changed).violations;
    assert.match(messages.join("\n"), /new\.js: new or renamed/);
    assert.match(messages.join("\n"), /renamed\.go: new or renamed/);
    assert.throws(() => checkLineRatchet(root, changed, base), /Git could not prove merge-base/);
    assert.throws(() => checkLineRatchet(root, "bad", changed), /trusted base/);
  }));

test("dropping to 600 or removal of a legacy file requires dropping its ceiling", () => fixture(
  ({ root, file, baseline, commit }) => {
    file("legacy.rs", utf8(610)); baseline({ "legacy.rs": 610 });
    const base = commit();
    file("legacy.rs", utf8(600));
    assert.match(checkLineRatchet(root, base, commit()).violations.join("\n"), /remove stale line exception/);
    baseline({});
    assert.deepEqual(checkLineRatchet(root, base, commit()).violations, []);
  }));
