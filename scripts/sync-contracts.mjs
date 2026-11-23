import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const sourceRepo = "risense-ai/respire-resources";
const openapi = "contracts/runtime-api/openapi.yaml";
const fixtures = "contracts/runtime-api/fixtures";
const lockPath = resolve(root, "contracts/sync.lock.json");
const [mode, source, revision] = process.argv.slice(2);
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex");

function main() {
  if (!source || !["--check", "--update"].includes(mode) ||
      (mode === "--update") !== Boolean(revision) || process.argv.length > 5) {
    throw new Error("Usage: node scripts/sync-contracts.mjs --check <resources checkout> or --update <resources checkout> <40-character commit>");
  }
  const git = (...args) => execFileSync("git", ["--no-replace-objects", "-C", resolve(source), ...args]);
  const origin = git("remote", "get-url", "origin").toString().trim().replace(/\.git$/, "");
  if (![ "https://github.com/" + sourceRepo, "git@github.com:" + sourceRepo ].includes(origin)) {
    throw new Error("Source checkout origin is not " + sourceRepo);
  }
  const commit = revision ?? JSON.parse(readFileSync(lockPath, "utf8")).commit;
  if (!/^[a-f0-9]{40}$/.test(commit)) throw new Error("Expected a complete source commit SHA");
  if (git("cat-file", "-t", commit).toString().trim() !== "commit") throw new Error("Source object is not a commit");
  git("merge-base", "--is-ancestor", commit, "origin/main");
  const entries = git("ls-tree", "-rz", commit, "--", openapi, fixtures)
    .toString().split("\0").filter(Boolean).map((entry) => {
      const match = entry.match(/^100644 blob ([a-f0-9]{40})\t(.*)$/);
      if (!match) throw new Error("Contract source includes a non-regular blob");
      const [, object, path] = match;
      if (path !== openapi && !/^contracts\/runtime-api\/fixtures\/[a-z][a-z0-9-]*\.json$/.test(path)) {
        throw new Error("Unexpected contract path: " + path);
      }
      const bytes = git("cat-file", "blob", object);
      if (path !== openapi) JSON.parse(bytes.toString("utf8"));
      return { path, bytes };
    });
  const spec = entries.find(({ path }) => path === openapi);
  if (!spec || entries.length < 2) throw new Error("Missing OpenAPI or golden fixture source blobs");
  const checksums = Object.fromEntries(entries.filter(({ path }) => path !== openapi)
    .map(({ path, bytes }) => [path, sha(bytes)]));
  const lock = Buffer.from(JSON.stringify({
    schema_version: 1, source_repo: sourceRepo, ref: "main", commit,
    openapi_path: openapi, openapi_sha256: sha(spec.bytes), fixtures_sha256: checksums,
  }, null, 2) + "\n");
  entries.push({ path: "contracts/sync.lock.json", bytes: lock });
  const fixtureDir = resolve(root, fixtures);
  if (mode === "--update") mkdirSync(fixtureDir, { recursive: true });
  const unexpected = readdirSync(fixtureDir).filter((file) => !Object.hasOwn(checksums, fixtures + "/" + file));
  if (unexpected.length) throw new Error("Unexpected golden files; review removal explicitly: " + unexpected.join(", "));
  for (const { path, bytes } of entries) {
    const destination = resolve(root, path);
    if (mode === "--check") {
      if (!readFileSync(destination).equals(bytes)) throw new Error("Contract drift: " + path);
    } else {
      mkdirSync(dirname(destination), { recursive: true });
      writeFileSync(destination, bytes);
    }
  }
  console.log((mode === "--check" ? "Verified" : "Synced") + " " + entries.length + " contract files @ " + commit);
}

try {
  main();
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
