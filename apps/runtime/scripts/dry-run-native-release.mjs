import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {constants, closeSync, copyFileSync, createReadStream, mkdirSync, mkdtempSync, openSync,
  readFileSync, readdirSync, writeFileSync} from 'node:fs';
import {dirname, isAbsolute, join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {parseComponentTag} from '../../../scripts/component-release-plan.mjs';
import {stageRuntimeCargo} from './stage-runtime-cargo.mjs';
import {nativeRealpath} from './runtime-release-smoke-helpers.mjs';

const lane = process.env.RUNTIME_DRY_RUN_LANE;
const lanes = new Map([
  ['windows-x64', {target: 'x86_64-pc-windows-msvc', os: 'Windows', platform: 'win32', binary: 'rsrs-runtime.exe'}],
  ['linux-gnu-x64', {target: 'x86_64-unknown-linux-gnu', os: 'Linux', platform: 'linux', binary: 'rsrs-runtime'}],
]);
assert(lanes.has(lane), 'Unsupported native dry-run lane');
const {target, os, platform, binary} = lanes.get(lane);
const image = `rsrs-runtime-${target}${platform === 'win32' ? '.exe' : ''}`;
const archive = `rsrs-runtime-${target}.tar.gz`;
const reportName = `runtime-build-${target}.json`;
const root = nativeRealpath(resolve(dirname(fileURLToPath(import.meta.url)), '../../..'));
const env = process.env;
assert.equal(env.GITHUB_ACTIONS, 'true', 'Requires GitHub Actions');
assert.match(env.GITHUB_WORKFLOW_SHA ?? '', /^[0-9a-f]{40}$/);
assert.match(env.GITHUB_WORKFLOW_REF ?? '', /^risense-ai\/respire\/.github\/workflows\/runtime-release-dry-run\.yml@refs\//);
assert.match(env.GITHUB_RUN_ID ?? '', /^[1-9][0-9]*$/);
assert.match(env.GITHUB_RUN_ATTEMPT ?? '', /^[1-9][0-9]*$/);
assert.equal(env.RUNNER_ENVIRONMENT, 'github-hosted', 'Requires an ephemeral hosted runner');
assert.equal(env.GITHUB_REPOSITORY, 'risense-ai/respire', 'Unexpected source repository');
assert.equal(env.RUNNER_OS, os, 'Unexpected runner OS');
assert.equal(env.RUNNER_ARCH, 'X64', 'Requires native x64 hardware');
assert.equal(process.platform, platform);
assert.equal(process.arch, 'x64');
assert(isAbsolute(env.RUNNER_TEMP ?? ''), 'Runner temp path must be absolute');

const run = (command, args, options = {}) => execFileSync(command, args,
  {encoding: 'utf8', cwd: root, ...options});
const source = run('git', ['rev-parse', 'HEAD']).trim();
assert.match(source, /^[0-9a-f]{40}$/);
assert.equal(source, env.GITHUB_SHA, 'Checkout must match the Actions event SHA');
assert.equal(run('git', ['status', '--porcelain', '--untracked-files=no']).trim(), '', 'Tracked checkout must be clean');
assert.equal(run('rustc', ['+1.95.0', '-vV']).match(/^host: ([^\r\n]+)$/m)?.[1], target, 'Unexpected Rust host');
const tag = env.RUNTIME_COMPONENT_TAG;
const {component, version} = parseComponentTag(tag);
assert.equal(component, 'runtime');
const manifest = readFileSync(join(root, 'apps/runtime/runtime/Cargo.toml'), 'utf8');
const lock = readFileSync(join(root, 'apps/runtime/Cargo.lock'), 'utf8');
assert.equal(/^version = "([^"]+)"/m.exec(manifest)?.[1], version, 'Dispatch tag differs from checked-in Runtime version');
assert.equal(/\[\[package\]\]\r?\nname = "respire_runtime"\r?\nversion = "([^"]+)"/.exec(lock)?.[1], version,
  'Dispatch tag differs from checked-in Runtime lockfile');

const work = mkdtempSync(join(nativeRealpath(env.RUNNER_TEMP), `runtime-release-${lane}-`));
const targetDir = join(work, 'cargo-target'), cargoLog = join(work, 'cargo.jsonl');
mkdirSync(targetDir);

async function hashFile(file) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(file)) hash.update(chunk);
  return hash.digest('hex');
}

const cleanLoaderEnv = Object.fromEntries(Object.entries(env).filter(([key]) =>
  !/^LD_|^GLIBC_TUNABLES$/i.test(key)));
const buildEnv = {...cleanLoaderEnv};
let ortArchiveSha256;
if (platform === 'linux') {
  const ort = join(work, 'pinned-ort');
  run('bash', ['apps/runtime/scripts/prepare-gnu-ort.sh', target, ort], {stdio: 'inherit', env: cleanLoaderEnv});
  ortArchiveSha256 = await hashFile(join(ort, 'onnxruntime.nupkg'));
  // CI may use an absolute build directory to probe debug binaries. Release
  // images must resolve the shipped SONAME beside the executable instead.
  Object.assign(buildEnv, {ORT_LIB_PATH: ort, ORT_SKIP_DOWNLOAD: '1', ORT_PREFER_DYNAMIC_LINK: '1',
    RUSTFLAGS: '-C target-feature=-crt-static -C embed-bitcode=no -C link-arg=-Wl,-rpath,$ORIGIN'});
  delete buildEnv.CARGO_ENCODED_RUSTFLAGS;
}

const descriptor = openSync(cargoLog, 'wx');
try {
  run('cargo', ['+1.95.0', 'build', '--release', '--locked', '--manifest-path', 'apps/runtime/Cargo.toml', '-p',
    'respire_runtime', '--bin', 'rsrs-runtime', '--target', target, '--target-dir', targetDir,
    '--message-format=json-render-diagnostics'], {stdio: ['ignore', descriptor, 'inherit'], env: buildEnv});
} finally {
  closeSync(descriptor);
}
assert.equal(run('git', ['status', '--porcelain', '--untracked-files=no']).trim(), '', 'Build modified tracked source');
if (platform === 'linux') {
  const profile = join(targetDir, target, 'release');
  const executable = join(profile, binary);
  const dynamic = run('readelf', ['-d', executable], {env: cleanLoaderEnv});
  const searchPaths = dynamic.split('\n').filter(line => /\((?:RPATH|RUNPATH)\)/.test(line));
  assert.equal(searchPaths.length, 1, 'Expected a single relative ELF loader path');
  assert.match(searchPaths[0], /\[\$ORIGIN\]$/, 'ELF loader path must be exactly $ORIGIN');
  assert.match(dynamic, /\(NEEDED\).*\[libonnxruntime\.so\.1\]/, 'Expected the shipped ONNX SONAME');
  const links = run('ldd', [executable], {env: cleanLoaderEnv});
  assert(!links.includes('not found'), 'Runtime has an unresolved native dependency');
  const loaded = links.match(/^\s*libonnxruntime\.so\.1 => (.+) \(0x[0-9a-f]+\)$/m)?.[1];
  assert.equal(loaded, join(profile, 'libonnxruntime.so.1'), 'Loader did not resolve the staged ONNX sidecar');
}

const batch = await stageRuntimeCargo({root, tag, source, target, targetDir, cargoLog, outputParent: work});
const names = [image, archive, reportName].sort();
assert.deepEqual(readdirSync(batch.directory).sort(), names, 'Unexpected files in the staged batch');
const assets = join(root, 'release-assets');
mkdirSync(assets);
for (const name of names) copyFileSync(join(batch.directory, name), join(assets, name), constants.COPYFILE_EXCL);

const hashes = {};
for (const name of names) hashes[name] = await hashFile(join(assets, name));
const build = JSON.parse(readFileSync(join(assets, reportName), 'utf8'));
assert.equal(hashes[image], build.binary_sha256);
assert.equal(hashes[archive], build.archive_sha256);
run(process.execPath, ['apps/runtime/scripts/smoke-runtime-release.mjs'], {stdio: 'inherit',
  env: {...cleanLoaderEnv, RUNTIME_TARGET: target, RUNTIME_VERSION: version}});
const smoke = JSON.parse(readFileSync(join(root, 'smoke-report', `runtime-smoke-${target}.json`), 'utf8'));
assert.equal(smoke.passed, true, 'The packaged native smoke must pass');
assert.equal(smoke.repository, env.GITHUB_REPOSITORY);
assert.equal(smoke.source_commit, source);
assert.equal(smoke.target, target);
assert.equal(smoke.version, version);
assert.equal(smoke.run_id, env.GITHUB_RUN_ID);
assert.equal(smoke.run_attempt, env.GITHUB_RUN_ATTEMPT);
assert.equal(smoke.binary_sha256, hashes[image]);
assert.equal(smoke.archive_sha256, hashes[archive]);
assert(Array.isArray(smoke.case_results) && smoke.case_results.length > 0 &&
  smoke.case_results.every(result => result.status === 'passed'), 'Incomplete native smoke coverage');

const directory = join(root, 'release-evidence');
mkdirSync(directory);
writeFileSync(join(directory, `runtime-${lane}.json`), JSON.stringify({
  schema: 1, mode: 'dry-run', repository: env.GITHUB_REPOSITORY, source, event: env.GITHUB_EVENT_NAME,
  workflow_ref: env.GITHUB_WORKFLOW_REF, workflow_sha: env.GITHUB_WORKFLOW_SHA,
  run_id: env.GITHUB_RUN_ID, run_attempt: env.GITHUB_RUN_ATTEMPT,
  target, tag, artifact_sha256: hashes,
  ...(ortArchiveSha256 && {provider_archive_sha256: ortArchiveSha256}),
  packaged_native_smoke: {passed: true, case_results: smoke.case_results},
}, null, 2) + '\n', {flag: 'wx'});
