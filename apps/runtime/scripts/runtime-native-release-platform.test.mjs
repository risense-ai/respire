import assert from 'node:assert/strict';
import {mkdtempSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import test from 'node:test';
import {assertMachONative, assertNativeLane, assertNativePE, nativeLanes, withoutLoaderOverrides} from './runtime-native-release-platform.mjs';

test('the supported native lanes require exact runner, Node and Rust architecture', () => {
  assert.deepEqual(Object.keys(nativeLanes), ['windows-x64', 'windows-arm64',
    'linux-gnu-x64', 'linux-gnu-arm64', 'macos-intel', 'macos-arm64']);
  for (const [name, lane] of Object.entries(nativeLanes)) {
    const actual = {runnerOS: lane.os, runnerArch: lane.arch.toUpperCase(), platform: lane.platform,
      arch: lane.arch, rustHost: lane.target};
    assert.equal(assertNativeLane(name, actual), lane);
    for (const field of Object.keys(actual)) {
      assert.throws(() => assertNativeLane(name, {...actual, [field]: 'wrong'}));
    }
  }
  assert.throws(() => assertNativeLane('macos-x64', {}), /Unsupported/);
  assert.throws(() => assertNativeLane('linux-musl-x64', {}), /Unsupported/);
  assert.throws(() => assertNativeLane('constructor', {}), /Unsupported/);
});

test('Windows binaries require the exact native PE architecture', t => {
  const root = mkdtempSync(join(tmpdir(), 'runtime-native-pe-'));
  t.after(() => rmSync(root, {recursive: true, force: true}));
  const file = join(root, 'test.exe');
  const bytes = Buffer.alloc(70);
  bytes.write('MZ');
  bytes.writeUInt32LE(64, 0x3c);
  bytes.writeUInt32LE(0x4550, 64);
  bytes.writeUInt16LE(0xaa64, 68);
  writeFileSync(file, bytes);
  assert.doesNotThrow(() => assertNativePE(file, 'arm64'));
  assert.throws(() => assertNativePE(file, 'x64'), /Unexpected PE machine/);
  writeFileSync(file, bytes.subarray(0, 66));
  assert.throws(() => assertNativePE(file, 'arm64'), /Truncated PE header/);
});

test('native Apple Silicon requires a local loader and either static or adjacent ONNX', () => {
  const commands = 'Load command 1\n          cmd LC_RPATH\n      cmdsize 32\n         path @executable_path (offset 12)\nLoad command 2';
  const native = {archs: 'arm64\n', commands,
    dependencies: 'rsrs-runtime:\n\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0)\n'};
  assert.equal(assertMachONative(native), 'static');
  const dynamic = {...native, sidecarArchs: 'arm64\n',
    dependencies: native.dependencies + '\t@rpath/libonnxruntime.dylib (compatibility version 1.0.0)\n'};
  assert.equal(assertMachONative(dynamic), 'adjacent-dynamic');
  assert.throws(() => assertMachONative({...native, archs: 'x86_64'}), /ARM64/);
  assert.throws(() => assertMachONative({...dynamic, sidecarArchs: undefined}), /Missing ARM64/);
  assert.throws(() => assertMachONative({...native, sidecarArchs: 'arm64'}), /Unreferenced/);
  assert.throws(() => assertMachONative({...native, commands: commands.replace('@executable_path', '/tmp/build')}), /local/);
  assert.throws(() => assertMachONative({...dynamic, dependencies: dynamic.dependencies.replace('@rpath/', '/tmp/')}), /adjacent/);
});

test('external loader path overrides cannot supply a packaged test dependency', () => {
  assert.deepEqual(withoutLoaderOverrides({PATH: 'ok', LD_PRELOAD: 'bad', Dyld_LIBRARY_PATH: 'bad',
    GLIBC_TUNABLES: 'bad', ORT_DYLIB_PATH: 'bad'}), {PATH: 'ok'});
});
