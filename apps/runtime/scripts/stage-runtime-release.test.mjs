import assert from 'node:assert/strict';
import {test} from 'node:test';
import {createHash} from 'node:crypto';
import {execFileSync} from 'node:child_process';
import {copyFileSync, linkSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, unlinkSync, utimesSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {dirname, join} from 'node:path';
import {gunzipSync} from 'node:zlib';
import {stageRuntime} from './stage-runtime-release.mjs';
import {archiveTool} from './runtime-release-smoke-helpers.mjs';

const hash = value => createHash('sha256').update(value).digest('hex');
const v = '2.0.0-dev.31';
const target = 'x86_64-pc-windows-msvc';
function fixture(t) {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'runtime-stage-test-')));
  t.after(() => rmSync(root, {recursive: true, force: true}));
  function file(name, content) {
    const path = join(root, name);
    mkdirSync(dirname(path), {recursive: true});
    writeFileSync(path, content);
    return path;
  }
  const lock = `[[package]]\nname = "respire_runtime"\nversion = "${v}"\n`;
  file('apps/runtime/Cargo.lock', lock);
  file('apps/runtime/runtime/Cargo.toml', `[package]\nversion = "${v}"\n`);
  file('apps/core/LICENSE', 'test license bytes');
  for (const name of ["DirectML-LICENSE-CODE.txt","DirectML-LICENSE.txt","DirectML-ThirdPartyNotices.txt","libstdc++-COPYING.RUNTIME.txt","libstdc++-COPYING3.txt","mlx-LICENSE.txt","mlx-c-LICENSE.txt","mlx-rs-LICENSE-APACHE.txt","mlx-rs-LICENSE-MIT.txt","mlx-rs-NOTICE.txt","onnxruntime-LICENSE.txt","onnxruntime-ThirdPartyNotices.txt"]) file('apps/core/third-party/' + name, name);
  file('apps/core/third-party/UNTRACKED.txt', 'not part of payload');
  const binary = file('release/rsrs-runtime.exe', 'fixture binary');
  const resource = file('release/DirectML.dll', 'fixture directml');
  const receipt = {mode: 'in_repository_source', target, core_source_sha256: '1'.repeat(64),
    runtime_cargo_lock_sha256: hash(lock), ort_public_archive_sha256: null,
    runtime_files: {'DirectML.dll': hash('fixture directml')}};
  const buildReceipt = file('release/build/respire_core_sdk-a/out/source-build.json', JSON.stringify(receipt));
  return {args: {root, tag: 'runtime-v' + v, source: '2'.repeat(40), target, build: dirname(binary),
    buildReceipt, outputParent: root}, binary, resource, receipt};
}

test('ships only the exact verified payload, in reproducible plain ustar', async t => {
  const {args,binary} = fixture(t);
  const first = (await stageRuntime(args)).directory;
  utimesSync(binary, 1234567, 1234567);
  const second = (await stageRuntime(args)).directory;
  const asset = 'rsrs-runtime-' + target;
  const names = [asset + '.exe', asset + '.tar.gz', 'runtime-build-' + target + '.json'].sort();
  assert.deepEqual(readdirSync(first).sort(), names);
  for (const name of names) assert.deepEqual(readFileSync(join(first,name)), readFileSync(join(second,name)));
  const archive = join(first, asset + '.tar.gz');
  const report = JSON.parse(readFileSync(join(first, 'runtime-build-' + target + '.json')));
  assert.equal(report.archive_sha256, hash(readFileSync(archive)));
  assert.equal(report.tag, 'v' + v);
  const tar = archiveTool(process.platform, process.env.SystemRoot);
  const files = execFileSync(tar, ['-tzf', asset + '.tar.gz'], {cwd: first, encoding: 'utf8'}).trim().split(/\r?\n/);
  assert.deepEqual(files, report.files.map(file => file.path));
  assert(!files.some(file => file.includes('UNTRACKED')));
  const unpack = gunzipSync(readFileSync(archive));
  assert.equal(unpack.subarray(257, 262).toString(), 'ustar');
  assert(unpack.subarray(-1024).every(value => value === 0));
  const root = join(args.root,'unpack');
  mkdirSync(root);
  execFileSync(tar,['-xzf', asset + '.tar.gz','-C',root],{cwd:first});
  for (const file of report.files) {
    const bytes = readFileSync(join(root,file.path)); assert.equal(bytes.length,file.size); assert.equal(hash(bytes),file.sha256);
  }
  assert.deepEqual(readFileSync(join(root,asset + '.exe')), readFileSync(binary));
  const core = JSON.parse(readFileSync(join(root,'core-runtime.json')));
  assert.deepEqual(core.files, report.files.filter(f => !['core-runtime.json',asset+'.exe'].includes(f.path))
    .map(({path,sha256})=>({path,sha256})));
});

test('wrong target, version, receipt or pinned bytes cannot produce a batch', async t => {
  const {args,resource,receipt} = fixture(t);
  await assert.rejects(stageRuntime({...args,target:'unsupported'}), /Unknown Runtime target/);
  await assert.rejects(stageRuntime({...args,tag:'runtime-v1.0.0'}), /manifest version differs/);
  writeFileSync(args.buildReceipt,JSON.stringify({...receipt,runtime_files:{'../foreign.dll':'b'.repeat(64)}}));
  await assert.rejects(stageRuntime(args), /Unknown native resource/);
  writeFileSync(args.buildReceipt,JSON.stringify(receipt));
  writeFileSync(resource,'mutated');
  await assert.rejects(stageRuntime(args), /differs from source-build pin/);
  assert.deepEqual(readdirSync(args.outputParent).sort(), ['apps','release']);
});

test('a byte-copied Cargo image passes without accepting a hardlinked input', async t => {
  const {args, binary} = fixture(t);
  const cargoDeps = join(args.root, 'cargo-deps-image.exe');
  linkSync(binary, cargoDeps);
  assert.equal(lstatSync(binary).nlink, 2);
  await assert.rejects(stageRuntime(args), /Invalid bounded regular build file/);
  assert.deepEqual(readdirSync(args.outputParent).sort(), ['apps', 'cargo-deps-image.exe', 'release']);
  unlinkSync(binary);
  copyFileSync(cargoDeps, binary);
  assert.equal(lstatSync(binary).nlink, 1);
  const batch = await stageRuntime(args);
  const staged = join(batch.directory, 'rsrs-runtime-' + target + '.exe');
  assert.deepEqual(readFileSync(staged), readFileSync(cargoDeps));
  assert.equal(lstatSync(staged).nlink, 1);
});
