// Compare the exact pinned OpenAPI with descriptors used to bind Rust handlers.
import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {fileURLToPath} from 'node:url';

const root = new URL('../../../', import.meta.url);
const read = (path) => readFileSync(new URL(path, root));
const methods = new Set(['delete', 'get', 'head', 'options', 'patch', 'post', 'put', 'trace']);
const marker = 'RESPIRE_V1_ROUTES_JSON=';
const own = (object, key) => Object.prototype.hasOwnProperty.call(object, key);

// CARD-010 #7: reviewed missing registrations at d8e6612 (33 of 44 live).
// Delete a row in the same change that registers it. Never add new exceptions.
const reviewedMissing = Object.freeze([
  "DELETE /v1/spaces/{spaceId} deleteSpace",
  "GET /v1/backups/export exportBackup",
  "GET /v1/spaces listSpaces",
  "GET /v1/spaces/{spaceId} getSpace",
  "PATCH /v1/spaces/{spaceId} updateSpace",
  "POST /v1/backups/import importBackup",
  "POST /v1/host/transactions/login executeLoginTransaction",
  "POST /v1/host/transactions/logout executeLogoutTransaction",
  "POST /v1/organize/split splitOrganizeNode",
  "POST /v1/spaces createSpace",
  "POST /v1/spaces/{spaceId}/members inviteSpaceMember"
]);

function unique(items, description) {
  assert.equal(new Set(items).size, items.length, 'duplicate ' + description);
}

function tuple(method, path, operationId) {
  assert.ok(typeof method === 'string' && methods.has(method.toLowerCase()), 'invalid method');
  assert.match(path, /^\/v1\/(?:[A-Za-z0-9._~-]+|\{[A-Za-z_][A-Za-z0-9_]*\})(?:\/(?:[A-Za-z0-9._~-]+|\{[A-Za-z_][A-Za-z0-9_]*\}))*$/);
  assert.match(operationId, /^[A-Za-z][A-Za-z0-9_]*$/);
  return method.toUpperCase() + ' ' + path + ' ' + operationId;
}

function resolveParameter(document, parameter, seen = new Set()) {
  if (!parameter || !own(parameter, '$ref')) return parameter;
  const ref = parameter.$ref;
  assert.ok(typeof ref === 'string' && ref.startsWith('#/') && !seen.has(ref), 'bad parameter reference');
  seen.add(ref);
  const target = ref.slice(2).split('/').reduce((node, part) => {
    const key = decodeURIComponent(part).replaceAll('~1', '/').replaceAll('~0', '~');
    assert.ok(node && own(node, key), 'unresolved parameter reference');
    return node[key];
  }, document);
  return resolveParameter(document, target, seen);
}

export function canonicalTuples(document) {
  assert.match(document.openapi, /^3\.[01]\./);
  assert.ok(document.paths && typeof document.paths === 'object');
  const expected = [];
  for (const [path, entry] of Object.entries(document.paths)) {
    assert.ok(path.startsWith('/v1/'), 'unexpected non-v1 canonical path');
    const placeholders = [...path.matchAll(/\{([^{}]+)\}/g)].map((match) => match[1]);
    unique(placeholders, 'path template name');
    assert.ok(entry && typeof entry === 'object' && !own(entry, '$ref'), 'unresolved path item');
    for (const [method, operation] of Object.entries(entry)) {
      if (!methods.has(method)) {
        assert.ok(['summary', 'description', 'parameters', 'servers'].includes(method)
          || method.startsWith('x-'), 'unknown path item member: ' + method);
        continue;
      }
      const effective = new Map();
      for (const parameters of [entry.parameters ?? [], operation.parameters ?? []]) {
        const seen = new Set();
        assert.ok(Array.isArray(parameters));
        for (const input of parameters) {
          const parameter = resolveParameter(document, input);
          assert.ok(parameter && ['path', 'query', 'header', 'cookie'].includes(parameter.in));
          assert.ok(typeof parameter.name === 'string' && parameter.name.length > 0);
          // HTTP header names are case-insensitive; query and path names are not.
          const key = parameter.in + ':' + (parameter.in === 'header'
            ? parameter.name.toLowerCase() : parameter.name);
          assert.ok(!seen.has(key), 'duplicate parameter: ' + key);
          seen.add(key);
          effective.set(key, parameter);
        }
      }
      const declared = [...effective.values()].filter((param) => param.in === 'path');
      assert.ok(declared.every((param) => param.required === true), 'path parameter must be required');
      assert.deepEqual(declared.map((param) => param.name).sort(), [...placeholders].sort(),
        method.toUpperCase() + ' ' + path + ' path parameter names');
      expected.push(tuple(method, path, operation.operationId));
    }
  }
  return expected;
}

export function compare(expected, rows, {ratchet = false} = {}) {
  assert.ok(Array.isArray(rows), 'runtime must export an array');
  const actual = rows.map(({method, path, operation_id}) => tuple(method, path, operation_id));
  for (const [items, name] of [[expected, 'canonical'], [actual, 'Runtime']]) {
    unique(items.map((item) => item.slice(0, item.lastIndexOf(' '))), name + ' method/template');
    unique(items.map((item) => item.slice(item.lastIndexOf(' ') + 1)), name + ' operationId');
  }
  const expectedSet = new Set(expected);
  const actualSet = new Set(actual);
  const missing = expected.filter((item) => !actualSet.has(item)).sort();
  const unexpected = actual.filter((item) => !expectedSet.has(item)).sort();
  const acknowledged = ratchet ? reviewedMissing : [];
  unique(acknowledged, 'reviewed gap');
  const unreviewed = missing.filter((item) => !acknowledged.includes(item));
  const stale = acknowledged.filter((item) => !missing.includes(item));
  assert.ok(unreviewed.length === 0 && unexpected.length === 0 && stale.length === 0,
    ['Runtime/OpenAPI route registration mismatch',
      ...unreviewed.map((item) => 'missing: ' + item),
      ...unexpected.map((item) => 'unexpected: ' + item),
      ...stale.map((item) => 'remove stale reviewed gap: ' + item)].join('\n'));
  return {registered: actual.length, canonical: expected.length, missing: missing.length};
}

export function runtimeRows(stdout) {
  const payloads = [...stdout.matchAll(/^RESPIRE_V1_ROUTES_JSON=(.+)$/gm)];
  assert.equal(payloads.length, 1, 'expected exactly one successful Runtime descriptor export');
  assert.match(stdout, /test result: ok\. 1 passed; 0 failed; 0 ignored;/,
    'descriptor exporter must be an actual executed Rust test');
  return JSON.parse(payloads[0][0].slice(marker.length));
}

function runRustTest(name) {
  const result = spawnSync('cargo', ['test', '--locked', '--offline', '--manifest-path',
    'apps/runtime/Cargo.toml', '-p', 'respire_commands', '--lib',
    'runtime_http::v1::route_parity::' + name, '--', '--exact', '--nocapture'],
  {cwd: fileURLToPath(root), encoding: 'utf8', maxBuffer: 16 * 1024 * 1024});
  if (result.error) throw result.error;
  if (result.stderr) process.stderr.write(result.stderr);
  if (result.status !== 0 && result.stdout) process.stderr.write(result.stdout);
  assert.equal(result.status, 0, 'failed Runtime test: ' + name);
  assert.match(result.stdout, /test result: ok\. 1 passed; 0 failed; 0 ignored;/,
    'expected exactly one actual Rust test: ' + name);
  return result.stdout;
}

function main() {
  const args = process.argv.slice(2);
  const ratchet = args.length === 1 && args[0] === '--reviewed-gap-ratchet';
  assert.ok(args.length === 0 || ratchet, 'only --reviewed-gap-ratchet is supported');
  const lock = JSON.parse(read('contracts/sync.lock.json'));
  assert.match(lock.commit, /^[0-9a-f]{40}$/);
  assert.equal(lock.source_repo, 'risense-ai/respire-resources');
  assert.equal(lock.openapi_path, 'contracts/runtime-api/openapi.yaml');
  const raw = read(lock.openapi_path);
  assert.equal(createHash('sha256').update(raw).digest('hex'), lock.openapi_sha256, 'unpinned OpenAPI bytes');
  const require = createRequire(new URL('packages/runtime-sdk-ts/package.json', root));
  // js-yaml is already integrity-pinned by the SDK's existing package lock.
  const {load} = require('js-yaml');
  const expected = canonicalTuples(load(raw.toString('utf8')));
  assert.equal(expected.length, 44, 'unexpected change to this card\'s canonical operation count');
  const registered = runtimeRows(runRustTest('export_registered_routes'));
  const counts = compare(expected, registered, {ratchet});
  runRustTest('real_router_health_and_problem_wire_shapes');
  console.log(
    'Runtime/OpenAPI registrations: ' + counts.registered + '/' + counts.canonical
    + ' at ' + lock.commit
    + (counts.missing ? ' (' + counts.missing + ' reviewed gaps; NOT full parity)' : ' (full registration parity)')
  );
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) main();
