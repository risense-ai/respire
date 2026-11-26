import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {constants, createReadStream, createWriteStream} from 'node:fs';
import {copyFile, lstat, mkdir, mkdtemp, readFile, realpath, rm, writeFile} from 'node:fs/promises';
import {basename, dirname, isAbsolute, join, resolve} from 'node:path';
import {Readable} from 'node:stream';
import {pipeline} from 'node:stream/promises';
import {fileURLToPath, pathToFileURL} from 'node:url';
import {createGzip} from 'node:zlib';
import {expectedReleaseAssets, parseComponentTag} from '../../../scripts/component-release-plan.mjs';

const maxFile = 256 * 2 ** 20;
const sha = /^[a-f0-9]{64}$/;
const nativeFile = /^(?:DirectML\.dll|onnxruntime\.dll|onnxruntime_providers_[A-Za-z0-9_]+\.dll|libonnxruntime\.(?:dylib|so(?:\.[0-9]+)*)|libonnxruntime_providers_[A-Za-z0-9_]+\.(?:so|dylib)|mlx\.metallib)$/;
const noticeNames = [
  "DirectML-LICENSE-CODE.txt",
  "DirectML-LICENSE.txt",
  "DirectML-ThirdPartyNotices.txt",
  "libstdc++-COPYING.RUNTIME.txt",
  "libstdc++-COPYING3.txt",
  "mlx-LICENSE.txt",
  "mlx-c-LICENSE.txt",
  "mlx-rs-LICENSE-APACHE.txt",
  "mlx-rs-LICENSE-MIT.txt",
  "mlx-rs-NOTICE.txt",
  "onnxruntime-LICENSE.txt",
  "onnxruntime-ThirdPartyNotices.txt"
];
const json = value => JSON.stringify(value, null, 2) + '\n';
const git = (root, ...args) => execFileSync('git', ['-C', root, ...args], {encoding: 'utf8'}).trim();

async function directory(path) {
  assert(isAbsolute(path) && await realpath(path) === resolve(path) && (await lstat(path)).isDirectory(),
    'Expected unredirected absolute directory');
  return resolve(path);
}

async function regular(path, limit = maxFile) {
  const stat = await lstat(path);
  assert(stat.isFile() && stat.nlink === 1 && stat.size > 0 && stat.size <= limit, 'Invalid bounded regular build file');
  return stat.size;
}

async function hash(path) {
  const digest = createHash('sha256');
  for await (const chunk of createReadStream(path)) digest.update(chunk);
  return digest.digest('hex');
}

function header(name, size, executable) {
  const block = Buffer.alloc(512);
  const octal = (value, at, length) => block.write(value.toString(8).padStart(length - 1, '0'), at, length - 1, 'ascii');
  assert(Buffer.byteLength(name) <= 100, 'Release path exceeds the plain ustar filename field');
  block.write(name, 0, 100, 'ascii');
  octal(executable ? 0o755 : 0o644, 100, 8);
  octal(0, 108, 8); octal(0, 116, 8); octal(size, 124, 12); octal(0, 136, 12);
  block.fill(32, 148, 156);
  block.write('0', 156); block.write('ustar\0' + '00', 257);
  block.write(block.reduce((sum, value) => sum + value, 0).toString(8).padStart(6, '0'), 148, 6, 'ascii');
  block[155] = 32;
  return block;
}

async function* archive(files, binary) {
  for (const file of files) {
    yield header(file.path, file.size, file.path === binary);
    yield* createReadStream(file.from);
    yield Buffer.alloc((512 - (file.size % 512)) % 512);
  }
  yield Buffer.alloc(1024);
}

// Only the current in-repository build's declared native files enter the
// payload. The surrounding build job still owns binary provenance and native smoke.
export async function stageRuntime({root, tag, source, target, build, buildReceipt, outputParent, intelProvider}) {
  const plan = parseComponentTag(tag);
  const binary = `rsrs-runtime-${target}${target.endsWith('-windows-msvc') ? '.exe' : ''}`;
  const archiveName = `rsrs-runtime-${target}.tar.gz`;
  assert(plan.component === 'runtime' && expectedReleaseAssets(tag).github.includes(archiveName), 'Unknown Runtime target');
  assert(/^[a-f0-9]{40}$/.test(source), 'Invalid exact source commit');
  [root, build, outputParent] = await Promise.all([root, build, outputParent].map(directory));
  assert(isAbsolute(buildReceipt) && await realpath(buildReceipt) === resolve(buildReceipt) &&
    basename(buildReceipt) === 'source-build.json' && basename(dirname(buildReceipt)) === 'out' && /^respire_core_sdk-[a-f0-9]+$/.test(basename(dirname(dirname(buildReceipt)))) &&
    dirname(dirname(dirname(buildReceipt))) === join(build, 'build'),
  'Expected direct Cargo source-build receipt');
  buildReceipt = resolve(buildReceipt);
  await regular(buildReceipt, 2 ** 20);
  const receiptBytes = await readFile(buildReceipt);
  const receipt = JSON.parse(receiptBytes);
  const runtimeRoot = join(root, 'apps', 'runtime'), coreRoot = join(root, 'apps', 'core');
  const cargoLock = join(runtimeRoot, 'Cargo.lock');
  const manifest = await readFile(join(runtimeRoot, 'runtime', 'Cargo.toml'), 'utf8');
  const lock = await readFile(cargoLock, 'utf8');
  assert.equal(/^version = "([^"]+)"/m.exec(manifest)?.[1], plan.version, 'Runtime manifest version differs');
  assert.equal(/\[\[package\]\]\r?\nname = "respire_runtime"\r?\nversion = "([^"]+)"/.exec(lock)?.[1], plan.version,
    'Runtime Cargo.lock version differs');
  assert(receipt.mode === 'in_repository_source' && receipt.target === target &&
    sha.test(receipt.core_source_sha256 ?? '') && receipt.runtime_cargo_lock_sha256 === await hash(cargoLock) &&
    (receipt.ort_public_archive_sha256 === null || sha.test(receipt.ort_public_archive_sha256 ?? '')) &&
    receipt.runtime_files && Object.getPrototypeOf(receipt.runtime_files) === Object.prototype, 'Invalid source build receipt');
  const intel = target === "x86_64-apple-darwin";
  assert.equal(Boolean(intelProvider), intel, "Exactly Intel requires reviewed provider files");
  if (intel) {
    await directory(intelProvider);
    assert.equal(receipt.runtime_files["libonnxruntime.dylib"],
      "2d2306de2ea691044b0afa552d1b8ffb0e51baee3172d0988be587f0b68f27a0", "Intel provider differs");
  }
  const runtimeFiles = Object.entries(receipt.runtime_files);
  assert(runtimeFiles.length <= 64 && runtimeFiles.every(([name, digest]) => nativeFile.test(name) && sha.test(digest)),
    'Unknown native resource name or checksum');
  assert.equal(new Set(runtimeFiles.map(([name]) => name.toLowerCase())).size, runtimeFiles.length, 'Duplicate native resource');
  if (target.endsWith('-windows-msvc')) assert(Object.hasOwn(receipt.runtime_files, 'DirectML.dll'), 'Required DirectML resource missing');
  const notices = await directory(join(coreRoot, 'third-party'));
  const work = await realpath(await mkdtemp(join(outputParent, 'runtime-build-')));
  try {
    const payload = join(work, 'payload'), batch = join(work, 'batch');
    await mkdir(payload); await mkdir(batch); await mkdir(join(payload, 'core-notices'));
    const files = [];
    async function add(path, input, pin) {
      const size = await regular(input);
      const from = join(payload, ...path.split('/'));
      await copyFile(input, from, constants.COPYFILE_EXCL);
      assert.equal(await regular(from), size, 'Build input changed while staging');
      const sha256 = await hash(from);
      if (pin) assert.equal(sha256, pin, 'Native resource differs from source-build pin');
      files.push({path, size, sha256, from});
    }
    await add(binary, join(build, target.endsWith('-windows-msvc') ? 'rsrs-runtime.exe' : 'rsrs-runtime'));
    const image = files[0];
    for (const [name, pin] of runtimeFiles) await add(intel && name === "libonnxruntime.dylib" ? "libonnxruntime.1.20.1.dylib" : name, join(build, name), pin);
    await add('core-notices/LICENSE.txt', join(coreRoot, 'LICENSE'));
    for (const name of noticeNames) {
      const override = intel && {
        "onnxruntime-LICENSE.txt": ["LICENSE", "c250d6278f0b47a6439fb7592b08b58a55eb9f535aa49a1db63211c3f982b674"],
        "onnxruntime-ThirdPartyNotices.txt": ["ThirdPartyNotices.txt", "fac5bb85f568b38fd8f3a6aea32737985841f23a1a43e9ce952f79474f883aa9"],
      }[name];
      if (override) await add("core-notices/" + name, join(intelProvider, override[0]), override[1]);
      else await add("core-notices/" + name, join(notices, name));
    }
    files.sort((a,b) => a.path < b.path ? -1 : Number(a.path > b.path));
    const coreManifest = join(payload, 'core-runtime.json');
    await writeFile(coreManifest, json({schema_version: 1, target, files: files.filter(file => file !== image)
      .map(({path, sha256}) => ({path, sha256}))}), {flag: 'wx'});
    files.unshift({path: 'core-runtime.json', from: coreManifest, size: await regular(coreManifest, 2 ** 20),
      sha256: await hash(coreManifest)});
    assert(files.reduce((n, file) => n + file.size, 0) <= 512 * 2 ** 20, 'Archive exceeds uncompressed consumer limit');
    const standalone = join(batch, binary);
    await copyFile(image.from, standalone, constants.COPYFILE_EXCL);
    const archivePath = join(batch, archiveName);
    await pipeline(Readable.from(archive(files, binary)), createGzip({level: 9}), createWriteStream(archivePath, {flags: 'wx'}));
    await writeFile(join(batch, `runtime-build-${target}.json`), json({
      schema_version: 2, runtime_version: plan.version, tag: plan.legacyTag, channel: plan.channel,
      target_triple: target, source_commit: source, archive_file: archiveName, archive_sha256: await hash(archivePath),
      binary_file: binary, binary_sha256: image.sha256, protocol_range: {minimum: 1, maximum: 1},
      capabilities: {business_executor: '1', mcp_server: '1', offline_keygen_handoff: '1'},
      files: files.map(({path, size, sha256}) => ({path, size, sha256})),
    }), {flag: 'wx'});
    await rm(payload, {recursive: true});
    return {directory: batch, kind: 'github', source, tag};
  } catch (error) {
    assert.equal(await realpath(work), work, 'Refusing cleanup of redirected build root');
    await rm(work, {recursive: true});
    throw error;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2);
  assert(args.length === 6, 'Usage: stage-runtime-release.mjs <tag> <source-sha> <target> <build-dir> <source-build.json> <output-parent>');
  const [tag, source, target, build, buildReceipt, outputParent] = args;
  assert.notEqual(target, "x86_64-apple-darwin", "Intel staging requires the verified provider from the native dry-run producer");
  const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
  assert.equal(git(root, 'rev-parse', 'HEAD'), source, 'Source SHA does not match checkout');
  assert.equal(git(root, 'status', '--porcelain', '--untracked-files=no'), '', 'Tracked checkout must be clean');
  console.log(JSON.stringify(await stageRuntime({root, tag, source, target, build, buildReceipt, outputParent})));
}
