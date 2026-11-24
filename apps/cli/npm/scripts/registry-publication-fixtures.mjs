import { createHash } from "node:crypto";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { tmpdir } from "node:os";
import assert from "node:assert/strict";

const bytes = Buffer.from('a deterministic packed package for offline tests');
const digest = body => ({integrity: `sha512-${createHash('sha512').update(body).digest('base64')}`,
  shasum: createHash('sha1').update(body).digest('hex')});
const expected = {name: '@rsrsai/cli', version: '1.0.12-dev.9', filename: 'rsrsai-cli-1.0.12-dev.9.tgz', ...digest(bytes)};
const metadata = (overrides = {}) => ({name: expected.name, version: expected.version,
  dist: {...digest(bytes), tarball: 'https://registry.npmjs.org/@rsrsai/cli/-/cli-1.0.12-dev.9.tgz'}, ...overrides});
const json = data => new Response(JSON.stringify(data), {status: 200, headers: {'Content-Type': 'application/json'}});
const missing = () => new Response('', {status: 404});

function fixture(t) {
  const root = mkdtempSync(path.join(tmpdir(), 'rsrs-registry-test-'));
  t.after(() => rmSync(root, {recursive: true, force: true}));
  const dir = path.join(root, 'package');
  mkdirSync(dir);
  writeFileSync(path.join(dir, 'package.json'), JSON.stringify({name: expected.name, version: expected.version}));
  return {root, dir};
}

function mockNpm({publishStatus = 0, packBytes = bytes, packReport = expected, packOutput = report => [report], onPublish} = {}) {
  const calls = [];
  return {calls, spawnImpl(command, args, options) {
    calls.push({command, args, options});
    if (args[0] === 'pack') {
      assert(args.includes('--ignore-scripts'));
      const dest = args[args.indexOf('--pack-destination') + 1];
      writeFileSync(path.join(dest, packReport.filename), packBytes);
      return {status: 0, stdout: JSON.stringify(packOutput(packReport))};
    }
    assert.equal(args[0], 'publish');
    assert.equal(args.at(-1), 'https://registry.npmjs.org');
    assert.deepEqual(readFileSync(args[1]), bytes);
    onPublish?.();
    return {status: publishStatus};
  }};
}

function registry({absent = 0, tags = {dev: '1.0.12-dev.8'}, body = bytes, meta = metadata()} = {}) {
  const calls = [];
  let checked = 0;
  return {calls, async fetchImpl(url, options) {
    calls.push({url, options});
    assert.equal(options.credentials, 'omit');
    assert.equal(options.redirect, 'error');
    assert.equal(options.headers.Authorization, undefined);
    assert.equal(options.headers.authorization, undefined);
    if (url.includes('/dist-tags')) return tags === null ? missing() : json(tags);
    if (url.endsWith('.tgz')) return new Response(body);
    return checked++ < absent ? missing() : json(meta);
  }};
}

function fastClock() {
  let elapsed = 0;
  return {timeoutMs: 30, pollMs: 10, now: () => elapsed, sleep: async ms => {elapsed += ms;}};
}

export { bytes, digest, expected, fastClock, fixture, json, metadata, missing, mockNpm, registry };
