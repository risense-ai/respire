import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {constants, createReadStream} from 'node:fs';
import {copyFile, lstat, mkdir, mkdtemp, readFile, realpath, rm} from 'node:fs/promises';
import {basename, dirname, isAbsolute, join, resolve} from 'node:path';
import {fileURLToPath, pathToFileURL} from 'node:url';
import {expectedReleaseAssets, parseComponentTag} from '../../../scripts/component-release-plan.mjs';
import {stageRuntime} from './stage-runtime-release.mjs';

const digest = /^[a-f0-9]{64}$/;
const nativeFile = /^[A-Za-z0-9][A-Za-z0-9._+-]{0,127}$/;

export function cargoRuntimeOutputs(messages, targetDir, target) {
  const profile = join(targetDir, target, 'release');
  const executable = join(profile, target.endsWith('-windows-msvc') ? 'rsrs-runtime.exe' : 'rsrs-runtime');
  const images = [], receipts = [], finished = [];
  for (const line of messages.split(/\r?\n/).filter(Boolean)) {
    const message = JSON.parse(line);
    if (message.reason === 'build-finished') finished.push(message.success);
    if (message.reason === 'compiler-artifact' && message.target?.name === 'rsrs-runtime' &&
        message.target.kind?.includes('bin') && typeof message.executable === 'string') {
      images.push(resolve(message.executable));
    }
    if (message.reason !== 'build-script-executed' || typeof message.out_dir !== 'string') continue;
    const out = resolve(message.out_dir);
    if (basename(out) === 'out' && /^respire_core_sdk-[a-f0-9]+$/.test(basename(dirname(out))) &&
        dirname(dirname(out)) === join(profile, 'build')) receipts.push(join(out, 'source-build.json'));
  }
  assert.deepEqual(finished, [true], 'Expected exactly one successful Cargo build');
  assert.deepEqual(images, [executable], 'Expected the sole Runtime image at the selected Cargo release target');
  assert.equal(receipts.length, 1, 'Expected the sole Core receipt at the selected Cargo release target');
  return {profile, executable, receipt: receipts[0]};
}

async function canonical(path) {
  assert(isAbsolute(path) && await realpath(path) === resolve(path), 'Expected an absolute unredirected path');
  return resolve(path);
}

async function file(path, max = 512 * 2 ** 20) {
  await canonical(path);
  const stat = await lstat(path);
  assert(stat.isFile() && stat.size > 0 && stat.size <= max, 'Expected a bounded regular input file');
}

async function sha256(path) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest('hex');
}

export async function stageRuntimeCargo({root, tag, source, target, targetDir, cargoLog, outputParent}) {
  assert(parseComponentTag(tag).component === 'runtime' &&
    expectedReleaseAssets(tag).github.includes(`rsrs-runtime-${target}.tar.gz`), 'Unknown Runtime target');
  outputParent = await canonical(outputParent);
  targetDir = await canonical(targetDir);
  await file(cargoLog, 64 * 2 ** 20);
  const {profile, executable, receipt} = cargoRuntimeOutputs(await readFile(cargoLog, 'utf8'), targetDir, target);
  await file(receipt, 2 ** 20);
  const receiptBytes = await readFile(receipt);
  const pins = JSON.parse(receiptBytes).runtime_files;
  assert(pins && Object.getPrototypeOf(pins) === Object.prototype, 'Invalid native resource list');
  const entries = Object.entries(pins);
  assert(entries.length <= 64 && entries.every(([name, pin]) => nativeFile.test(name) &&
    !name.endsWith('.') && digest.test(pin)), 'Invalid native resource name or checksum');
  const scratch = await mkdtemp(join(outputParent, 'runtime-cargo-image-'));
  try {
    const build = join(scratch, 'release');
    const buildReceipt = join(build, 'build', basename(dirname(dirname(receipt))), 'out', 'source-build.json');
    await mkdir(dirname(buildReceipt), {recursive: true});
    for (const [input, copy, pin] of [
      [executable, join(build, basename(executable))],
      [receipt, buildReceipt, createHash('sha256').update(receiptBytes).digest('hex')],
      ...entries.map(([name, sha]) => [join(profile, name), join(build, name), sha]),
    ]) {
      await file(input);
      const expected = pin ?? await sha256(input);
      await copyFile(input, copy, constants.COPYFILE_EXCL);
      assert.equal(await sha256(copy), expected, 'Cargo build bytes differ from the original checksum');
    }
    return await stageRuntime({root, tag, source, target, build, buildReceipt, outputParent});
  } finally {
    assert.equal(dirname(await canonical(scratch)), outputParent, 'Refusing cleanup outside the owned output parent');
    await rm(scratch, {recursive: true});
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2);
  assert(args.length === 6,
    'Usage: stage-runtime-cargo.mjs <tag> <source-sha> <target> <target-dir> <cargo-json-log> <output-parent>');
  const [tag, source, target, targetDir, cargoLog, outputParent] = args;
  const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
  const git = (...argv) => execFileSync('git', argv, {cwd: root, encoding: 'utf8'}).trim();
  assert.equal(git('rev-parse', 'HEAD'), source, 'Source SHA does not match checkout');
  assert.equal(git('status', '--porcelain', '--untracked-files=no'), '', 'Tracked checkout must be clean');
  console.log(JSON.stringify(await stageRuntimeCargo({root, tag, source, target, targetDir, cargoLog, outputParent})));
}
