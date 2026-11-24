import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { collectNotices } from './collect-sdk-notices.mjs';

// Internal business SDK packaging; no network publication is performed.
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const environment = (name) => process.env[name]
  ?? process.env[name.replace(/^RSRS_/, 'ONEMEMORY_')]
  ?? process.env[name.replace(/^RSRS_/, 'RESPIRE_')];
const revision = spawnSync('git', ['-c', `safe.directory=${root}`, 'rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' });
if (revision.status !== 0 || !/^[a-f0-9]{40}$/.test(revision.stdout.trim())) {
  throw new Error(`Cannot identify SDK source revision: ${revision.stderr || revision.error || 'invalid revision'}`);
}
const [target, destination, header] = process.argv.slice(2);
if (!target || !destination || !header) {
  throw new Error('Usage: node scripts/build-core-sdk.mjs <target> <output-dir> <canonical-header>');
}
// ort-sys skips downloaded native libraries in Cargo offline mode, even when
// the archive is cached. A staticlib can still compile and fail at final linkage.
if (process.env.CARGO_NET_OFFLINE === 'true' && !process.env.ORT_LIB_PATH && !process.env.ORT_LIB_LOCATION) {
  throw new Error('Offline SDK builds require an explicit ORT_LIB_PATH; otherwise unset CARGO_NET_OFFLINE');
}
const output = resolve(destination);
if (existsSync(join(output, 'manifest.json'))) {
  throw new Error('SDK output already exists; choose a fresh output directory');
}
const targetRoot = resolve(process.env.CARGO_TARGET_DIR || join(root, 'target'));
const cargoFlags = process.env.CARGO_ENCODED_RUSTFLAGS !== undefined
  ? process.env.CARGO_ENCODED_RUSTFLAGS.split('\x1f')
  : (process.env.RUSTFLAGS || '').trim().split(/\s+/).filter(Boolean);
// Encoded arguments preserve paths containing spaces. These mappings remove
// private checkout/cache prefixes from compiled panic and diagnostic strings.
cargoFlags.push(`--remap-path-prefix=${dirname(root)}=/respire/sdk-build`);
if (process.env.CARGO_HOME) cargoFlags.push(`--remap-path-prefix=${resolve(process.env.CARGO_HOME)}=/cargo`);
if (target.includes('musl')) {
  // GCC keeps its static C++ runtime outside rustc's default native paths.
  const cxx = process.env[`CXX_${target}`] || process.env[`CXX_${target.replaceAll('-', '_')}`] || process.env.CXX || 'c++';
  const machine = spawnSync(cxx, ['-dumpmachine'], { encoding: 'utf8' });
  if (machine.error) throw machine.error;
  if (machine.status !== 0 || machine.stdout.trim().split('-')[0] !== target.split('-')[0] || !machine.stdout.includes('musl')) {
    throw new Error('The C++ compiler must target the requested musl architecture');
  }
  const runtime = spawnSync(cxx, ['-print-file-name=libstdc++.a'], { encoding: 'utf8' });
  if (runtime.error) throw runtime.error;
  const archive = runtime.stdout.trim();
  if (runtime.status !== 0 || archive === 'libstdc++.a' || !existsSync(archive)) {
    throw new Error('Native musl SDK builds require the target static C++ runtime');
  }
  cargoFlags.push('-L', `native=${dirname(resolve(archive))}`);
}
const driver = environment('RSRS_CARGO_DRIVER') || '';
const nativeFaultTests = environment('RSRS_SDK_NATIVE_FAULT_TESTS') === '1';
const mlx = environment('RSRS_SDK_MLX') === '1';
if (driver && driver !== 'xwin') throw new Error('Unsupported SDK Cargo driver');
if (mlx && (target.includes('windows') || target.includes('linux') || target === 'x86_64-apple-darwin')) {
  throw new Error('The optional mlx feature requires an Apple Silicon macOS target');
}
// mlx-sys compiles Metal kernels into a metallib outside the Cargo target
// directory. Keep it inside the build tree so the SDK can ship it next to the
// executable, and so no builder home path is baked into the archive.
const metallibDir = mlx ? join(targetRoot, target, 'sdk-mlx-metallib') : null;
if (metallibDir) mkdirSync(metallibDir, { recursive: true });
const features = [nativeFaultTests && 'native-fault-tests', mlx && 'mlx'].filter(Boolean);
const build = spawnSync('cargo', [...(driver ? [driver] : []), 'rustc', '--manifest-path', join(root, 'Cargo.toml'),
  '--locked', '--profile', 'sdk', '--target', target, '-p', 'respire_core_ffi',
  ...(features.length ? ['--features', features.join(',')] : []),
  '--', '--print', 'native-static-libs'], { encoding: 'utf8', maxBuffer: 20 * 1024 * 1024,
    env: { ...process.env, ...(metallibDir ? { MLX_RS_METAL_PATH: metallibDir } : {}),
      CARGO_ENCODED_RUSTFLAGS: cargoFlags.join('\x1f') } });
process.stdout.write(build.stdout || '');
process.stderr.write(build.stderr || '');
if (build.error) throw build.error;
if (build.status !== 0) throw new Error(`SDK build exited ${build.status}`);
// Cargo may emit ANSI styling in CI; escape sequences are not library names.
const log = `${build.stdout}\n${build.stderr}`.replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, '');
const match = /native-static-libs:\s*([^\r\n]+)/.exec(log);
if (!match) throw new Error('Missing native-static-libs report');
const tokens = match[1].trim().split(/\s+/);
const nativeLibraries = [];
for (let i = 0; i < tokens.length; i++) {
  const token = tokens[i];
  if (token === '-framework') {
    if (!tokens[i + 1]) throw new Error('Missing framework name');
    nativeLibraries.push(`framework=${tokens[++i]}`);
  } else if (token.startsWith('-l')) {
    const name = token.slice(2);
    nativeLibraries.push(target.includes('musl') && ['stdc++', 'unwind'].includes(name) ? `static=${name}` : name);
  }
  else if (token.startsWith('/defaultlib:')) nativeLibraries.push(token.slice(12).replace(/\.lib$/i, ''));
  else if (/\.lib$/i.test(token)) nativeLibraries.push(token.replace(/\.lib$/i, ''));
  else throw new Error(`Unsupported native link input: ${token}`);
}
const buildDir = join(targetRoot, target, 'sdk');
const library = target.includes('windows-msvc') ? 'respire_core_ffi.lib' : 'librespire_core_ffi.a';
const toolchain = spawnSync('rustc', ['-Vv'], { encoding: 'utf8' });
if (toolchain.status !== 0) throw new Error('Cannot identify rustc');
const sysroot = spawnSync('rustc', ['--print', 'sysroot'], { encoding: 'utf8' });
if (sysroot.status !== 0) throw new Error('Cannot identify Rust sysroot');
const host = /^host: (\S+)$/m.exec(toolchain.stdout)?.[1];
if (!host) throw new Error('Cannot identify compiler host');
const objcopy = environment('RSRS_OBJCOPY') || join(sysroot.stdout.trim(), 'lib', 'rustlib', host, 'bin',
  `llvm-objcopy${host.includes('windows') ? '.exe' : ''}`);
if (!existsSync(objcopy)) throw new Error('SDK packaging requires the matching llvm-tools component');
mkdirSync(join(output, 'lib'), { recursive: true });
mkdirSync(join(output, 'include'), { recursive: true });
mkdirSync(join(output, 'runtime'), { recursive: true });
const sections = target.includes('apple')
  ? ['__LLVM,__bitcode', '__LLVM,__cmdline'] : ['.llvmbc', '.llvmcmd'];
if (target.includes('windows-msvc')) {
  // objcopy's COFF output failed real linkage on this toolchain. Preserve the
  // original archive indices/objects; erase only the non-executable IR payloads.
  // COFF layout: https://learn.microsoft.com/en-us/windows/win32/debug/pe-format
  const archive = readFileSync(join(buildDir, library));
  if (archive.subarray(0, 8).toString('ascii') !== '!<arch>\n') throw new Error('Unexpected static archive format');
  let erased = 0;
  let debugErased = 0;
  let pathsErased = 0;
  let offset = 8;
  while (offset < archive.length) {
    const header = archive.subarray(offset, offset + 60);
    const size = Number(header.subarray(48, 58).toString('ascii').trim());
    if (header.length !== 60 || header.subarray(58).toString('ascii') !== '`\n'
      || !Number.isSafeInteger(size) || size < 0 || offset + 60 + size > archive.length) {
      throw new Error('Malformed static archive member');
    }
    const name = header.subarray(0, 16).toString('ascii').trim();
    const bytes = archive.subarray(offset + 60, offset + 60 + size);
    if (name === '//') {
      // Member names are diagnostic metadata, not linker symbols. Replace only
      // directory characters, retaining each name's length and table offset.
      let start = 0;
      for (let end = 0; end <= bytes.length; end++) {
        if (end !== bytes.length && bytes[end] !== 0 && bytes[end] !== 10) continue;
        const segmentEnd = end > start && bytes[end - 1] === 47 && bytes[end] === 10 ? end - 1 : end;
        let slash = -1;
        for (let i = start; i < segmentEnd; i++) if (bytes[i] === 47 || bytes[i] === 92) slash = i;
        if (slash >= start) { bytes.fill(95, start, slash + 1); pathsErased++; }
        start = end + 1;
      }
    }
    const standard = bytes.length >= 20 && [0x8664, 0xaa64].includes(bytes.readUInt16LE(0));
    const bigobj = bytes.length >= 56 && bytes.readUInt16LE(0) === 0
      && bytes.readUInt16LE(2) === 0xffff && bytes.readUInt16LE(4) === 2
      && [0x8664, 0xaa64].includes(bytes.readUInt16LE(6))
      && bytes.subarray(12, 28).equals(Buffer.from('c7a1bad1eebaa94baf20faf66aa4dcb8', 'hex'));
    if (!['/', '//', '/SYM64/'].includes(name) && (standard || bigobj)) {
      const count = bigobj ? bytes.readUInt32LE(44) : bytes.readUInt16LE(2);
      const table = bigobj ? 56 : 20 + bytes.readUInt16LE(16);
      if (table + count * 40 > bytes.length) throw new Error('Malformed COFF section table');
      for (let index = 0; index < count; index++) {
        const position = table + index * 40;
        const section = bytes.subarray(position, position + 8).toString('ascii').replace(/\0+$/, '');
        const debug = section.startsWith('.debug');
        if (!sections.includes(section) && !debug) continue;
        const length = bytes.readUInt32LE(position + 16);
        const pointer = bytes.readUInt32LE(position + 20);
        const flags = bytes.readUInt32LE(position + 36);
        if ((flags & 0x20000020) !== 0 || (length && pointer < table + count * 40)
          || pointer + length > bytes.length || (!debug && bytes.readUInt16LE(position + 32) !== 0)) {
          throw new Error('Unexpected executable, relocated or malformed IR section');
        }
        bytes.fill(0, pointer, pointer + length);
        // IMAGE_SCN_LNK_REMOVE: this metadata must not enter the linked image.
        bytes.writeUInt32LE(((flags | 0x800) & ~0x1000000) >>> 0, position + 36);
        if (debug) {
          // Debug records may refer to code symbols. Discard their relocation
          // table as well; never rewrite the executable sections or symbols.
          bytes.writeUInt32LE(0, position + 24);
          bytes.writeUInt16LE(0, position + 32);
          debugErased++;
        } else erased++;
      }
    }
    offset += 60 + size + (size % 2);
  }
  writeFileSync(join(output, 'lib', library), archive);
  console.log(`Erased ${erased} COFF IR, ${debugErased} debug sections and ${pathsErased} archive member paths without changing archive indices`);
} else {
  const cleaned = spawnSync(objcopy, ['--strip-debug', ...sections.map(section => `--remove-section=${section}`),
    join(buildDir, library), join(output, 'lib', library)], { encoding: 'utf8' });
  if (cleaned.error) throw cleaned.error;
  if (cleaned.status !== 0) throw new Error(`SDK bitcode removal failed: ${cleaned.stderr}`);
}
copyFileSync(resolve(header), join(output, 'include', 'respire_core.h'));
const paths = [`lib/${library}`, 'include/respire_core.h'];
if (target.includes('musl') && nativeLibraries.includes('static=unwind')) {
  // C callers do not inherit rustc's target self-contained runtime paths.
  const archive = join(sysroot.stdout.trim(), 'lib', 'rustlib', target, 'lib', 'self-contained', 'libunwind.a');
  if (!existsSync(archive)) throw new Error('Missing matching Rust target static unwind runtime');
  copyFileSync(archive, join(output, 'lib', 'libunwind.a'));
  paths.push('lib/libunwind.a');
}
if (target.includes('windows-msvc')) {
  // An explicitly supplied dynamic ORT distribution needs its matching import
  // library as well as the DLL. Standalone SDK consumers cannot link from the
  // builder's ORT_LIB_PATH after delivery.
  if (nativeLibraries.includes('onnxruntime')) {
    const ortRoot = process.env.ORT_LIB_PATH || process.env.ORT_LIB_LOCATION;
    const candidates = ortRoot
      ? [join(ortRoot, 'onnxruntime.lib'), join(ortRoot, 'lib', 'onnxruntime.lib')].filter(existsSync)
      : [];
    if (candidates.length !== 1) throw new Error('Dynamic Windows ORT requires one matching onnxruntime.lib');
    copyFileSync(candidates[0], join(output, 'lib', 'onnxruntime.lib'));
    paths.push('lib/onnxruntime.lib');
  }
  // windows-targets supplies versioned import archives outside the Windows SDK.
  // Cargo reports their names but a standalone C/Rust caller needs the files too.
  const imports = [...new Set(nativeLibraries.filter(name => /^windows\.[\d.]+$/.test(name)))];
  if (imports.length) {
    const metadata = spawnSync('cargo', ['metadata', '--manifest-path', join(root, 'Cargo.toml'),
      '--locked', '--format-version', '1', '--filter-platform', target], { encoding: 'utf8', maxBuffer: 30 * 1024 * 1024 });
    if (metadata.error) throw metadata.error;
    if (metadata.status !== 0) throw new Error(`Cannot identify Windows import archives: ${metadata.stderr}`);
    const packageName = `windows_${target.split('-')[0]}_msvc`;
    const packages = JSON.parse(metadata.stdout).packages.filter(pkg => pkg.name === packageName);
    for (const name of imports) {
      const candidates = packages.map(pkg => join(dirname(pkg.manifest_path), 'lib', `${name}.lib`)).filter(existsSync);
      if (candidates.length !== 1) throw new Error(`Expected one locked ${target} import archive for ${name}`);
      copyFileSync(candidates[0], join(output, 'lib', `${name}.lib`));
      paths.push(`lib/${name}.lib`);
    }
  }
}
for (const file of readdirSync(buildDir)) {
  if (/\.(dll|dylib)$/.test(file) || /\.so(?:\.[\d.]+)?$/.test(file)) {
    copyFileSync(join(buildDir, file), join(output, 'runtime', file));
    paths.push(`runtime/${file}`);
  }
}
// MLX resolves a colocated mlx.metallib next to the executable. Shipping it
// avoids baking the builder's home directory into the static library.
if (mlx) {
  const metallib = join(metallibDir, 'mlx.metallib');
  if (!existsSync(metallib)) throw new Error(`MLX metallib missing: ${metallib}`);
  copyFileSync(metallib, join(output, 'runtime', 'mlx.metallib'));
  paths.push('runtime/mlx.metallib');
}
const manifest = {
  ...(nativeFaultTests ? { test_only: 'native-fault-tests' } : {}),
  ...(mlx ? { mlx_backend_contract: 1 } : {}),
  sdk_version: environment('RSRS_SDK_VERSION') || '1.0.2-dev.4', stage: 'internal-business-sdk',
  association_contract: 1,
  model_input_stream_contract: 1,
  experimental_only: (environment('RSRS_SDK_VERSION') || '').includes('experiment'),
  resident_retrieval_contract: 1,
  inference_thread_contract: 1,
  embedding_profile_contract: 1,
  embedding_transport_contract: 1,
  backend_load_probe_contract: 1,
  feedback_contract: 1,
  render_budget_contract: 1,
  brief_render_contract: 1,
  render_min_score_contract: 1,
  howto_recall_contract: 1,
  superseded_status_contract: 1,
  memory_metadata_contract: 1,
  read_heat_contract: 1,
  recall_calibration_contract: 1,
  recall_fusion_contract: 1,
  inference_sessions: { foreground: 1, background: 1, run_concurrency_per_session: 1 },
  product: 'Respire', organization: 'risense-ai', c_symbol_prefix: 'rs_core',
  abi_version: 0x00010002, schema_version: 1, target,
  source_revision: revision.stdout.trim(),
  artifact_version: 1, panic: 'unwind', lto: false,
  crt: target.includes('windows') ? 'dynamic' : target.includes('musl') ? 'static' : 'system',
  model_execution: 'private-core', models_bundled: false,
  metadata_cleanup: target.includes('windows-msvc')
    ? 'coff-ir-debug-and-archive-member-paths' : 'llvm-ir-and-debug-sections',
  private_build_paths: 'remapped',
  toolchain: toolchain.stdout.trim(), native_libraries: nativeLibraries,
  files: [...paths, ...collectNotices(root,target,buildDir,output,features)].map(path => ({ path, sha256: createHash('sha256').update(readFileSync(join(output, path))).digest('hex') })),
  redistribution: 'not-approved',
};
writeFileSync(join(output, 'manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`, 'utf8');
console.log(`Prepared internal SDK: ${output}`);
