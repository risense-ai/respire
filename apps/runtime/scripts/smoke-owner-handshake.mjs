// Native integration smoke for an explicitly supplied, hash-pinned Runtime candidate.
// Node 20+. No keyring, installer, service manager, model, or actual user data.
import assert from 'node:assert/strict';
import {execFileSync, spawn} from 'node:child_process';
import {createHash, createHmac, randomBytes} from 'node:crypto';
import {constants, createReadStream, existsSync, fstatSync, lstatSync, mkdirSync,
  mkdtempSync, openSync, closeSync, readSync, readFileSync, realpathSync, rmSync,
  writeFileSync} from 'node:fs';
import {createServer} from 'node:net';
import {tmpdir} from 'node:os';
import {basename, delimiter, dirname, isAbsolute, join, relative, resolve, sep, win32} from 'node:path';
import {setTimeout as delay} from 'node:timers/promises';
import {fileURLToPath} from 'node:url';
import {assertDrained, assertProtocol, assertSamePath, digest, normalizedPath, syntheticSession}
  from './runtime-release-smoke-helpers.mjs';
import {runRuntimeMcpProbe} from './runtime-mcp-probe.mjs';

const capability = 'auth.handshake.v1';
const proofHeader = 'X-Respire-Handshake-Proof';
const handshakePath = '/v1/auth/handshake';
const actions = [
  ['grants:read', 'GET', '/v1/grants'],
  ['grants:revoke', 'DELETE', '/v1/grants/00000000-0000-4000-8000-000000000001'],
  ['stats:read', 'GET', '/v1/stats'],
  ['query-log:read', 'GET', '/v1/query-log?limit=1'],
  ['reviews:read', 'GET', '/v1/memories/synthetic-absent/reviews?limit=1'],
  ['sync-conflicts:read', 'GET', '/v1/sync/conflicts?include_processed=false'],
];
const all = actions.map(([scope]) => scope);
const sameInstance = ['pid', 'instance_id', 'process_start_id', 'image_sha256'];
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

function mask(scopes) {
  assert.ok(scopes.length && scopes.length === new Set(scopes).size, 'invalid fixture scopes');
  return scopes.reduce((bits, scope) => {
    const index = all.indexOf(scope);
    assert.ok(index >= 0, 'unknown fixture scope');
    return bits | (1 << index);
  }, 0);
}

function proof(record, scopes, now = Date.now(), nonce = randomBytes(16)) {
  assert.ok(record.length === 48 && nonce.length === 16, 'invalid proof fixture input');
  const claims = Buffer.alloc(41);
  record.copy(claims, 0, 32);
  claims.writeBigUInt64BE(BigInt(now), 16);
  nonce.copy(claims, 24);
  claims[40] = mask(scopes);
  const tag = createHmac('sha256', record.subarray(0, 32))
    .update('respire-local-owner-proof-v1\0').update(claims).digest();
  return 'rhp1_' + Buffer.concat([claims, tag]).toString('hex');
}

function selfTest() {
  const record = Buffer.from(Array.from({length: 48}, (_, i) => i));
  const expected = 'rhp1_202122232425262728292a2b2c2d2e2f00000000499602d2'
    + 'a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a521'
    + '75e3e3b613134f646cbee0cbaa25e4a676977f045a9f24697c24e052c5849497';
  assert.ok(proof(record, [all[0], all[5]], 1234567890, Buffer.alloc(16, 0xa5)) === expected,
    'Node HMAC fixture differs from canonical Rust vector');
  if (process.platform === 'win32') {
    const script = fileURLToPath(import.meta.url);
    const verbatim = win32.toNamespacedPath(script);
    assert.ok(verbatim !== script, 'Windows path fixture must cover a verbatim drive path');
    assertSamePath(canonicalPath(verbatim, 'verbatim self-test'), canonicalPath(script, 'self-test'));
  }
  assert.equal(mask(all), 63);
  assert.throws(() => mask([all[0], all[0]]));
  record.fill(0);
}

async function binaryDigest(file) {
  const hash = createHash('sha256');
  for await (const update of createReadStream(file)) hash.update(update);
  return hash.digest('hex');
}

function canonicalPath(path, stage = 'fixture path') {
  if (process.platform !== 'win32') return realpathSync(path);
  try {
    // Rust emits verbatim drive paths in Runtime receipts. In Node 22, the
    // JavaScript walker can lstat a drive-relative root; use the OS resolver.
    return normalizedPath(realpathSync.native(path));
  } catch (error) {
    throw new Error('Windows path canonicalization failed at ' + stage + ' (' + error.code + ')');
  }
}
function below(parent, child) {
  const part = relative(parent, child);
  return !!part && !isAbsolute(part) && !part.split(sep).includes('..');
}

function fileJson(path, maxBytes = 64 * 1024) {
  const info = lstatSync(path);
  assert.ok(info.isFile() && !info.isSymbolicLink() && info.nlink === 1 && info.size <= maxBytes,
    'unsafe synthetic receipt');
  try { return JSON.parse(readFileSync(path, 'utf8')); } catch { throw new Error('invalid synthetic receipt JSON'); }
}

function ownerRecord(base, instance) {
  assert.match(instance, uuid, 'invalid Runtime instance');
  const dir = join(base, '.owner-credentials-' + instance);
  const path = join(dir, 'secret');
  const entry = lstatSync(dir);
  assert.ok(entry.isDirectory() && !entry.isSymbolicLink(), 'unsafe synthetic credential directory');
  assertSamePath(canonicalPath(dir), dir);
  assertSamePath(canonicalPath(path), path);
  if (process.platform !== 'win32') {
    assert.equal(entry.mode & 0o777, 0o700, 'credential directory is not owner-only');
    assert.equal(entry.uid, process.getuid());
  }
  const fd = openSync(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  const scratch = Buffer.alloc(49);
  try {
    const file = fstatSync(fd);
    assert.ok(file.isFile() && file.size === 48 && file.nlink === 1, 'invalid native credential record');
    if (process.platform !== 'win32') {
      assert.equal(file.mode & 0o777, 0o600, 'credential file is not owner-only');
      assert.equal(file.uid, process.getuid());
    }
    let count = 0;
    while (count < scratch.length) {
      const n = readSync(fd, scratch, count, scratch.length - count, count);
      if (!n) break;
      count += n;
    }
    assert.equal(count, 48, 'invalid native credential size');
    return Buffer.from(scratch.subarray(0, 48));
  } finally {
    scratch.fill(0);
    closeSync(fd);
  }
}


function secureWindowsFixtureDirectory(root) {
  if (process.platform !== 'win32') return;
  const systemRoot = process.env.SystemRoot ?? process.env.WINDIR;
  assert.ok(systemRoot && isAbsolute(systemRoot), 'Windows system root unavailable');
  const powershell = join(systemRoot, 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe');
  const script = [
    "$ErrorActionPreference = 'Stop'",
    '$owner = [Security.Principal.WindowsIdentity]::GetCurrent().User',
    '$acl = New-Object Security.AccessControl.DirectorySecurity',
    '$acl.SetOwner($owner)',
    '$acl.SetAccessRuleProtection($true, $false)',
    '$rule = [Security.AccessControl.FileSystemAccessRule]::new($owner, "FullControl",'
      + ' "ContainerInherit,ObjectInherit", "None", "Allow")',
    '$acl.AddAccessRule($rule)',
    'Set-Acl -LiteralPath $env:RESPIRE_SYNTHETIC_ACL_DIR -AclObject $acl',
  ].join('; ');
  // Windows mkdir ignores POSIX mode. This directory was just created by the
  // harness; do not weaken Runtime's enforcement to accommodate the temp root.
  execFileSync(powershell, ['-NoProfile', '-NonInteractive', '-Command', script], {
    env: {SystemRoot: systemRoot, WINDIR: systemRoot, RESPIRE_SYNTHETIC_ACL_DIR: root},
    stdio: 'ignore', windowsHide: true, timeout: 60000,
  });
}

function isolatedEnvironment(root, data, models, binary) {
  const paths = {};
  for (const name of ['home', 'config', 'local', 'roam', 'cache', 'state', 'temp']) {
    paths[name] = join(root, name);
    mkdirSync(paths[name], {mode: 0o700});
  }
  const os = {};
  for (const name of ['SystemRoot', 'WINDIR', 'SYSTEMDRIVE', 'ComSpec']) {
    if (process.platform === 'win32' && process.env[name]) os[name] = process.env[name];
  }
  const systemPath = process.platform === 'win32'
    ? [dirname(binary), join(os.SystemRoot ?? os.WINDIR ?? 'C:\\Windows', 'System32')].join(delimiter)
    : '/usr/bin:/bin';
  // Do not inherit real user paths, credential settings, sync config, proxies,
  // keyring/session bus variables, developer/test mode flags or shell argv.
  return {...os, PATH: systemPath, HOME: paths.home, USERPROFILE: paths.home,
    APPDATA: paths.roam, LOCALAPPDATA: paths.local, TEMP: paths.temp, TMP: paths.temp,
    TMPDIR: paths.temp, XDG_CONFIG_HOME: paths.config, XDG_DATA_HOME: data,
    XDG_CACHE_HOME: paths.cache, XDG_STATE_HOME: paths.state,
    RSRS_DATA_DIR: data, RSRS_BIN_DIR: dirname(binary), RSRS_M3_DIR: models,
    RSRS_NO_AUTOSYNC: '1', RSRS_CLIENT_ONLY: '0', RSRS_NO_AUTOSTART: '0'};
}

async function freeLoopbackPort() {
  const server = createServer();
  await new Promise((ok, fail) => {
    server.once('error', fail);
    server.listen({host: '127.0.0.1', port: 0, exclusive: true}, ok);
  });
  const {port} = server.address();
  await new Promise((ok, fail) => server.close(error => error ? fail(error) : ok()));
  return port;
}

async function poll(label, check, childState, time = 90000) {
  const deadline = Date.now() + time;
  while (Date.now() < deadline) {
    assert.ok(!childState(), 'Runtime exited before ' + label);
    const result = await check();
    if (result) return result;
    await delay(100);
  }
  throw new Error('Timed out waiting for ' + label);
}

function responseAt(endpoint) {
  return (path, {method = 'GET', body, headers = {}, timeout = 6000} = {}) =>
    fetch(endpoint + path, {method, redirect: 'error', signal: AbortSignal.timeout(timeout),
      headers: body === undefined ? headers : {'Content-Type': 'application/json', ...headers},
      body: body === undefined ? undefined : JSON.stringify(body)});
}

async function requireStatus(response, status, label) {
  assert.equal(response.status, status, label);
  if (status === 204) {
    assert.equal(await response.text(), '', '204 unexpectedly carried a body');
    return null;
  }
  try { return await response.json(); } catch { throw new Error('invalid JSON response: ' + label); }
}

async function expectProblem(response, status, code, label) {
  const body = await requireStatus(response, status, label);
  assert.ok(body.code === code, label);
}

function receiptBound(info, child, endpoint, binary, sha, data) {
  const {pid, url, image_path: image, image_sha256: actual, installation_root: root} = info;
  assert.ok(pid === child.pid && url === endpoint && actual === sha, 'fixture Runtime identity mismatch');
  assertSamePath(canonicalPath(image, 'Runtime image receipt'), binary);
  assertSamePath(canonicalPath(root, 'installation root receipt'), data);
  assert.match(info.instance_id, uuid);
  assert.ok(typeof info.process_start_id === 'string' && info.process_start_id.length > 0);
}

async function ownerChecks(call, record) {
  const handshake = (signed, scopes, extra = {}) => call(handshakePath, {method: 'POST',
    body: {scopes}, headers: {...extra, [proofHeader]: signed}});
  const bearer = access => ({Authorization: 'Bearer ' + access});
  async function getToken(signed, scopes) {
    const response = await handshake(signed, scopes);
    assert.equal(response.headers.get('cache-control'), 'no-store');
    const token = await requireStatus(response, 200, 'owner handshake');
    assert.ok(token.token_type === 'Bearer', 'unexpected lease token type');
    assert.ok(token.expires_in === 300, 'unexpected owner lease TTL');
    assert.ok(Array.isArray(token.scopes) && token.scopes.length === scopes.length
      && token.scopes.every((scope, index) => scope === scopes[index]), 'owner lease scope differs');
    assert.ok(typeof token.access_token === 'string' && token.access_token.length > 24, 'missing scoped lease');
    assert.ok(token.access_token !== signed && !JSON.stringify(token).includes(record.subarray(0, 32).toString('hex')),
      'handshake echoed credential material');
    return token.access_token;
  }
  const signed = proof(record, all);
  const access = await getToken(signed, all);
  await expectProblem(await handshake(signed, all), 401, 'unauthorized', 'replayed proof');
  const stale = proof(record, all, Date.now() - 61000);
  await expectProblem(await handshake(stale, all), 401, 'unauthorized', 'expired proof');
  const onlyStats = [all[2]];
  const narrow = proof(record, onlyStats);
  await expectProblem(await handshake(narrow, [all[0], ...onlyStats]), 401, 'unauthorized',
    'unproven scope in redemption');
  await expectProblem(await handshake(proof(record, all), all, {Origin: 'https://foreign.invalid'}),
    403, 'forbidden_origin', 'foreign Origin');
  const statsAccess = await getToken(proof(record, onlyStats), onlyStats);
  await requireStatus(await call(actions[2][2], {headers: bearer(statsAccess)}), 200, 'minimal stats');
  await expectProblem(await call(actions[0][2], {headers: bearer(statsAccess)}),
    401, 'unauthorized', 'stats token at grants path');
  await expectProblem(await call(actions[0][2]), 401, 'unauthorized', 'missing bearer');
  await expectProblem(await call(actions[2][2], {method: 'POST', headers: bearer(access)}),
    405, 'method_not_allowed', 'wrong canonical method');

  for (let index = 0; index < actions.length; index++) {
    const [scope, method, path] = actions[index];
    const scoped = await getToken(proof(record, [scope]), [scope]);
    const [, otherMethod, otherPath] = actions[(index + 1) % actions.length];
    await expectProblem(await call(otherPath, {method: otherMethod, headers: bearer(scoped)}),
      401, 'unauthorized', 'scope isolation: ' + scope);
    const body = await requireStatus(await call(path, {method, headers: bearer(scoped)}),
      method === 'DELETE' ? 204 : 200, scope);
    if (method === 'DELETE') {
      await requireStatus(await call(path, {method, headers: bearer(access)}), 204, 'idempotent revoke');
    } else if (scope === 'stats:read') {
      assert.equal(body.local_total, 0);
      assert.equal(body.local_alive, 0);
      assert.equal(typeof body.remote_configured, 'boolean');
    } else {
      assert.deepEqual(body.items, [], 'unexpected data in empty synthetic library');
      if (scope === 'query-log:read') assert.equal(body.stats.total, 0);
    }
  }
}

async function smoke(binary, sha) {
  const tempBase = canonicalPath(resolve(process.env.RUNNER_TEMP || tmpdir()));
  const root = canonicalPath(mkdtempSync(join(tempBase, 'respire-owner-smoke-')));
  assert.ok(below(tempBase, root) && basename(root).startsWith('respire-owner-smoke-'), 'invalid temp root');
  let child, ended, childFailure, closed, record, sessionBytes;
  try {
    const data = join(root, 'data'), models = join(root, 'empty-models'), runtimeDir = join(data, 'runtime');
    secureWindowsFixtureDirectory(root);
    mkdirSync(runtimeDir, {recursive: true, mode: 0o700});
    mkdirSync(models, {mode: 0o700});
    const fixture = syntheticSession(), legacyHostToken = randomBytes(32).toString('hex');
    sessionBytes = Buffer.from(JSON.stringify(fixture.session));
    writeFileSync(join(data, 'session.json'), sessionBytes, {flag: 'wx', mode: 0o600});
    writeFileSync(join(runtimeDir, 'token'), legacyHostToken, {flag: 'wx', mode: 0o600});
    const port = await freeLoopbackPort(), endpoint = 'http://127.0.0.1:' + port;
    const call = responseAt(endpoint);
    child = spawn(binary, ['serve', '--host', '127.0.0.1', '--port', String(port), '--data-dir', data],
      {cwd: root, env: isolatedEnvironment(root, data, models, binary), stdio: 'ignore', windowsHide: true});
    closed = new Promise(ok => child.once('close', (code, signal) => {
      ended = {code, signal}; ok(ended);
    }));
    child.once('error', () => { childFailure = true; });
    const childState = () => ended || childFailure;
    const endpointFile = join(runtimeDir, 'endpoint.json');
    // Verify our fresh, child-written endpoint before sending any HTTP at all.
    const endpointInfo = await poll('isolated endpoint receipt', () =>
      existsSync(endpointFile) ? fileJson(endpointFile) : null, childState);
    receiptBound(endpointInfo, child, endpoint, binary, sha, data);
    const health = () => call('/api/health').then(response => requireStatus(response, 200, 'legacy health'));
    const locked = await poll('synthetic locked Runtime', async () => {
      let info;
      try { info = await health(); } catch (error) {
        if (error instanceof TypeError || error.name === 'TimeoutError') return null;
        throw error;
      }
      receiptBound(info, child, endpoint, binary, sha, data);
      assertProtocol(info);
      assert.equal(info.store_ready, false, 'Runtime opened store without synthetic unlock');
      return info.startup_complete && info.credential_state === 'required' ? info : null;
    }, childState);
    assert.equal(locked.state, 'locked');
    assertSamePath(canonicalPath(locked.data_dir, 'locked profile receipt'), data);
    const lockedV1 = await requireStatus(await call('/v1/health'), 200, 'locked v1 health');
    assert.ok(!lockedV1.capabilities?.includes(capability), 'locked Runtime advertised owner handshake');
    await expectProblem(await call(handshakePath, {method: 'POST', body: {scopes: all}}),
      404, 'route_not_found', 'locked handshake is unavailable');
    // This is the existing synthetic release-smoke unlock binding, NOT the
    // 32-byte owner proof key. That key is read later and never sent over HTTP.
    const unlocked = await call('/api/runtime/unlock', {method: 'POST', timeout: 15000,
      headers: {Authorization: 'Bearer ' + legacyHostToken},
      body: {schema_version: 1, profile: data, session_sha256: digest(sessionBytes),
        instance_id: locked.instance_id, generation: locked.generation,
        super_password: fixture.superPassword, login_password: null}});
    const unlockBody = await requireStatus(unlocked, 200, 'synthetic host unlock');
    assert.equal(unlockBody.store_ready, true);
    const v1 = await poll('native owner handshake capability', async () => {
      const info = await requireStatus(await call('/v1/health'), 200, 'ready v1 health');
      return info.capabilities?.includes(capability) ? info : null;
    }, childState);
    assert.equal(v1.instance_id, locked.instance_id);
    const ready = await health();
    assert.equal(ready.state, 'ready');
    for (const key of sameInstance) assert.equal(ready[key], locked[key], 'legacy identity changed');
    record = ownerRecord(runtimeDir, ready.instance_id);
    await expectProblem(await call('/v1/grants', {headers: {Authorization: 'Bearer ' + legacyHostToken}}),
      401, 'unauthorized', 'legacy host token is not an owner lease');
    await ownerChecks(call, record);
    // The preserved aliases have the same normal malformed-JSON response. No
    // legacy mutation is invoked to establish this compatibility property.
    const oldPost = path => fetch(endpoint + path, {method: 'POST', redirect: 'error',
      headers: {'Content-Type': 'application/json'}, body: '{', signal: AbortSignal.timeout(6000)});
    const legacy = await requireStatus(await oldPost('/api/rpc'), 400, 'legacy /api/rpc');
    assert.deepEqual(await requireStatus(await oldPost('/rpc'), 400, 'legacy /rpc'), legacy);
    for (const path of ['/rpc', '/api/rpc']) {
      const id = 'owner-smoke-legacy-status';
      const reply = await requireStatus(await call(path, {method: 'POST',
        body: {v: 1, id, method: 'runtime.status', args: []}}), 200, path + ' runtime.status');
      assert.ok(reply.ok === true && reply.id === id && reply.pid === child.pid,
        'legacy alias did not return status for the serving process');
    }
    try { await runRuntimeMcpProbe(endpoint, ready.runtime_version, ready, {}); }
    catch { throw new Error('legacy MCP contract failed'); }
    const identity = Object.fromEntries(['installation_id', 'instance_id', 'account_id', 'generation']
      .map(key => [key, ready[key]]));
    const response = await call('/api/runtime/stop', {method: 'POST', body: identity,
      headers: {'X-Respire-Lifecycle': 'coordinator-v1'}});
    const accepted = await requireStatus(response, 200, 'scoped standalone stop');
    assert.ok(accepted.ok === true && accepted.state === 'accepted', 'scoped stop not accepted');
    const result = await Promise.race([closed, delay(35000, null, {ref: false})]);
    assert.ok(result?.code === 0 && result.signal === null, 'Runtime did not cleanly exit');
    assertDrained(fileJson(join(runtimeDir, 'stop-' + ready.instance_id + '.json'), 1 << 20),
      ready, accepted.operation_id);
    assert.ok(!existsSync(join(runtimeDir, '.owner-credentials-' + ready.instance_id, 'secret')),
      'stop did not remove the retired owner credential');
    console.log('Owner handshake: native scoped proof, six routes, denials, legacy RPC/MCP, and scoped stop passed.');
  } finally {
    record?.fill(0);
    sessionBytes?.fill(0);
    if (child && !ended && !childFailure) {
      child.kill();
      await Promise.race([closed, delay(2500, null, {ref: false})]);
      if (!ended) {
        child.kill('SIGKILL');
        await Promise.race([closed, delay(2500, null, {ref: false})]);
      }
    }
    if (!child || ended || childFailure) {
      const onDisk = canonicalPath(root);
      assert.ok(below(tempBase, onDisk) && basename(onDisk).startsWith('respire-owner-smoke-'),
        'synthetic cleanup target changed');
      rmSync(onDisk, {recursive: true, force: true, maxRetries: 3, retryDelay: 100});
    }
  }
}

async function main(args) {
  selfTest();
  if (args.length === 1 && args[0] === '--self-test') {
    console.log('Owner handshake Node proof vector passed; no Runtime launched.');
    return;
  }
  if (args.length !== 4 || args[0] !== '--runtime' || args[2] !== '--sha256' || !/^[0-9a-f]{64}$/.test(args[3])) {
    throw new Error('Usage: node smoke-owner-handshake.mjs --runtime <native-build-binary> --sha256 <expected-sha256>');
  }
  const supplied = resolve(args[1]);
  assert.ok(lstatSync(supplied).isFile() && !lstatSync(supplied).isSymbolicLink(), 'candidate must be a regular file');
  const binary = canonicalPath(supplied);
  assert.equal(await binaryDigest(binary), args[3], 'candidate Runtime digest mismatch');
  await smoke(binary, args[3]);
}

main(process.argv.slice(2)).catch(error => {
  // No response, header, fixture environment, key or child output is printed.
  console.error('Owner handshake smoke failed: ' + error.message.split('\n', 1)[0]);
  process.exitCode = 1;
});
