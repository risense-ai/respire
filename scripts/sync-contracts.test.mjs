
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";

function save(path, content) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, content);
}

test("sync pins exact upstream bytes and rejects local drift and unrelated source commits", (t) => {
  const tmp = mkdtempSync(join(tmpdir(), "respire-contract-sync-"));
  t.after(() => rmSync(tmp, { recursive: true, force: true }));
  const upstream = join(tmp, "source");
  const destination = join(tmp, "destination");
  mkdirSync(upstream);
  const git = (...args) => {
    const result = spawnSync("git", ["-C", upstream, ...args], { encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
    return result.stdout.trim();
  };
  const contract = "contracts/runtime-api/openapi.yaml";
  const fixture = "contracts/runtime-api/fixtures/demo.json";
  git("init", "--quiet", "--initial-branch=main");
  git("config", "user.name", "Contract test");
  git("config", "user.email", "contract@example.test");
  git("config", "core.autocrlf", "false");
  git("remote", "add", "origin", "https://github.com/risense-ai/respire-resources.git");
  save(join(upstream, contract), "openapi: 3.1.0\n");
  save(join(upstream, fixture), '{"text":"café"}\n');
  git("add", ".");
  git("commit", "--quiet", "-m", "Golden source");
  const commit = git("rev-parse", "HEAD");
  git("update-ref", "refs/remotes/origin/main", commit);
  const isolatedScript = join(destination, "scripts/sync-contracts.mjs");
  mkdirSync(dirname(isolatedScript), { recursive: true });
  copyFileSync(new URL("./sync-contracts.mjs", import.meta.url), isolatedScript);
  const sync = (mode, ...args) => spawnSync(process.execPath, [isolatedScript, mode, upstream, ...args],
    { encoding: "utf8" });

  const updated = sync("--update", commit);
  assert.equal(updated.status, 0, updated.stderr);
  const lockPath = join(destination, "contracts/sync.lock.json");
  const locked = JSON.parse(readFileSync(lockPath, "utf8"));
  assert.equal(locked.commit, commit);
  assert.match(locked.fixtures_sha256[fixture], /^[0-9a-f]{64}$/);
  assert.deepEqual(readFileSync(join(destination, fixture)), readFileSync(join(upstream, fixture)));
  assert.equal(sync("--check").status, 0);

  const original = readFileSync(join(destination, fixture));
  save(join(destination, fixture), "{}\n");
  const drift = sync("--check");
  assert.equal(drift.status, 1);
  assert.match(drift.stderr, /Contract drift/);
  assert.equal(readFileSync(join(destination, fixture), "utf8"), "{}\n");
  save(join(destination, fixture), original);

  const unattachedCommit = git("commit-tree", "HEAD^{tree}", "-m", "Unpublished commit");
  assert.equal(sync("--update", unattachedCommit).status, 1);
  assert.equal(sync("--check").status, 0);
});
