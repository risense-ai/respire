import assert from 'node:assert/strict';
import {closeSync, openSync, readSync} from 'node:fs';

export const nativeLanes = Object.freeze({
  'windows-x64': Object.freeze({target: 'x86_64-pc-windows-msvc', os: 'Windows', platform: 'win32',
    arch: 'x64', binary: 'rsrs-runtime.exe'}),
  'windows-arm64': Object.freeze({target: 'aarch64-pc-windows-msvc', os: 'Windows', platform: 'win32',
    arch: 'arm64', binary: 'rsrs-runtime.exe'}),
  'linux-gnu-x64': Object.freeze({target: 'x86_64-unknown-linux-gnu', os: 'Linux', platform: 'linux',
    arch: 'x64', binary: 'rsrs-runtime'}),
  'linux-gnu-arm64': Object.freeze({target: 'aarch64-unknown-linux-gnu', os: 'Linux', platform: 'linux',
    arch: 'arm64', binary: 'rsrs-runtime'}),
  'macos-intel': Object.freeze({target: 'x86_64-apple-darwin', os: 'macOS', platform: 'darwin',
    arch: 'x64', binary: 'rsrs-runtime'}),
  'macos-arm64': Object.freeze({target: 'aarch64-apple-darwin', os: 'macOS', platform: 'darwin',
    arch: 'arm64', binary: 'rsrs-runtime'}),
});

export function assertNativeLane(lane, {runnerOS, runnerArch, platform, arch, rustHost}) {
  assert(Object.hasOwn(nativeLanes, lane), 'Unsupported native dry-run lane');
  const expected = nativeLanes[lane];
  assert.equal(runnerOS, expected.os, 'Unexpected runner OS');
  assert.equal(runnerArch, expected.arch.toUpperCase(), 'Unexpected runner hardware architecture');
  assert.equal(platform, expected.platform, 'Unexpected Node platform');
  assert.equal(arch, expected.arch, 'Unexpected Node architecture');
  assert.equal(rustHost, expected.target, 'Unexpected Rust host');
  return expected;
}

export function assertNativePE(file, arch) {
  const dos = Buffer.alloc(64), pe = Buffer.alloc(6);
  const fd = openSync(file, 'r');
  try {
    assert.equal(readSync(fd, dos, 0, dos.length, 0), dos.length, 'Truncated DOS header');
    assert.equal(dos.toString('ascii', 0, 2), 'MZ', 'Invalid DOS header');
    const offset = dos.readUInt32LE(0x3c);
    assert(offset >= dos.length && offset <= 2 ** 20, 'Unexpected PE header offset');
    assert.equal(readSync(fd, pe, 0, pe.length, offset), pe.length, 'Truncated PE header');
    assert.equal(pe.readUInt32LE(), 0x4550, 'Invalid PE signature');
    assert(Object.hasOwn({x64: true, arm64: true}, arch), 'Unsupported PE architecture');
    assert.equal(pe.readUInt16LE(4), arch === 'arm64' ? 0xaa64 : 0x8664, 'Unexpected PE machine');
  } finally {
    closeSync(fd);
  }
}

export function assertMachONative({archs, commands, dependencies, sidecarArchs}) {
  assert.equal(archs.trim(), 'arm64', 'Expected an actual ARM64 Mach-O executable');
  const rpaths = [...commands.matchAll(/^\s*cmd LC_RPATH\s*\n\s*cmdsize \d+\s*\n\s*path (\S+) \(offset \d+\)\s*$/gm)]
    .map(match => match[1]);
  assert.deepEqual(rpaths, ['@executable_path'], 'Expected exactly one local Mach-O loader search path');
  const ortLoads = dependencies.split('\n').slice(1).map(line => line.trim().split(/\s+/)[0])
    .filter(path => /(?:^|\/)libonnxruntime[^/]*\.dylib$/.test(path) && !path.includes('providers_'));
  if (ortLoads.length === 0) {
    assert.equal(sidecarArchs, undefined, 'Unreferenced ONNX library must not be packaged');
    return 'static';
  }
  assert.deepEqual(ortLoads, ['@rpath/libonnxruntime.dylib'], 'Expected the adjacent bundled ONNX library');
  assert((sidecarArchs ?? '').trim().split(/\s+/).includes('arm64'), 'Missing ARM64 architecture in the ONNX sidecar');
  return 'adjacent-dynamic';
}

export function withoutLoaderOverrides(env) {
  return Object.fromEntries(Object.entries(env).filter(([key]) =>
    !/^(?:LD_|DYLD_|GLIBC_TUNABLES$|ORT_DYLIB_PATH$)/i.test(key)));
}
