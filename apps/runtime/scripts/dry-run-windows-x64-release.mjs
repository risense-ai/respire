import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {constants, closeSync, copyFileSync, createReadStream, mkdirSync, mkdtempSync, openSync,
  readFileSync, readdirSync, realpathSync, writeFileSync} from 'node:fs';
import {dirname, isAbsolute, join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {parseComponentTag} from '../../../scripts/component-release-plan.mjs';
import {stageRuntimeCargo} from './stage-runtime-cargo.mjs';

const target = 'x86_64-pc-windows-msvc';
const image = `rsrs-runtime-${target}.exe`;
const archive = `rsrs-runtime-${target}.tar.gz`;
const reportName = `runtime-build-${target}.json`;
const root = realpathSync.native(resolve(dirname(fileURLToPath(import.meta.url)), '../../..'));
const env = process.env;
assert.equal(env.GITHUB_ACTIONS, 'true', 'Requires GitHub Actions');
assert.match(env.GITHUB_WORKFLOW_SHA ?? '', /^[0-9a-f]{40}$/);
assert.match(env.GITHUB_WORKFLOW_REF ?? '', /^risense-ai\/respire\/.github\/workflows\/runtime-release-dry-run\.yml@refs\//);
assert.match(env.GITHUB_RUN_ID ?? '', /^[1-9][0-9]*$/);
assert.match(env.GITHUB_RUN_ATTEMPT ?? '', /^[1-9][0-9]*$/);
assert.equal(env.RUNNER_ENVIRONMENT, 'github-hosted', 'Requires an ephemeral hosted runner');
assert.equal(env.GITHUB_REPOSITORY, 'risense-ai/respire', 'Unexpected source repository');
assert.equal(env.RUNNER_OS, 'Windows', 'Requires a Windows runner');
assert.equal(env.RUNNER_ARCH, 'X64', 'Requires native x64 hardware');
assert.equal(process.platform, 'win32');
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

const work = mkdtempSync(join(realpathSync.native(env.RUNNER_TEMP), 'runtime-release-windows-x64-'));
const targetDir = join(work, 'cargo-target'), cargoLog = join(work, 'cargo.jsonl');
mkdirSync(targetDir);
const descriptor = openSync(cargoLog, 'wx');
try {
  run('cargo', ['+1.95.0', 'build', '--release', '--locked', '--manifest-path', 'apps/runtime/Cargo.toml', '-p',
    'respire_runtime', '--bin', 'rsrs-runtime', '--target', target, '--target-dir', targetDir,
    '--message-format=json-render-diagnostics'], {stdio: ['ignore', descriptor, 'inherit']});
} finally {
  closeSync(descriptor);
}
assert.equal(run('git', ['status', '--porcelain', '--untracked-files=no']).trim(), '', 'Build modified tracked source');
const batch = await stageRuntimeCargo({root, tag, source, target, targetDir, cargoLog, outputParent: work});
const names = [image, archive, reportName].sort();
assert.deepEqual(readdirSync(batch.directory).sort(), names, 'Unexpected files in the staged batch');
const assets = join(root, 'release-assets');
mkdirSync(assets);
for (const name of names) copyFileSync(join(batch.directory, name), join(assets, name), constants.COPYFILE_EXCL);

const hashes = {};
for (const name of names) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(join(assets, name))) hash.update(chunk);
  hashes[name] = hash.digest('hex');
}
const build = JSON.parse(readFileSync(join(assets, reportName), 'utf8'));
assert.equal(hashes[image], build.binary_sha256);
assert.equal(hashes[archive], build.archive_sha256);
run(process.execPath, ['apps/runtime/scripts/smoke-runtime-release.mjs'], {stdio: 'inherit',
  env: {...env, RUNTIME_TARGET: target, RUNTIME_VERSION: version}});
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
writeFileSync(join(directory, 'runtime-windows-x64.json'), JSON.stringify({
  schema: 1, mode: 'dry-run', repository: env.GITHUB_REPOSITORY, source, event: env.GITHUB_EVENT_NAME,
  workflow_ref: env.GITHUB_WORKFLOW_REF, workflow_sha: env.GITHUB_WORKFLOW_SHA,
  run_id: env.GITHUB_RUN_ID, run_attempt: env.GITHUB_RUN_ATTEMPT,
  target, tag, artifact_sha256: hashes,
  packaged_native_smoke: {passed: true, case_results: smoke.case_results},
}, null, 2) + '\n', {flag: 'wx'});
