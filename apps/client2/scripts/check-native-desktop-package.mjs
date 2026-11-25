import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {lstatSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync} from 'node:fs';
import {isAbsolute, join, relative, sep} from 'node:path';
import {fileURLToPath} from 'node:url';

const [rootArg, target, version] = process.argv.slice(2);
const match = /^(windows|darwin|linux)-(amd64|arm64)$/.exec(target ?? '');
assert.equal(process.argv.length, 5, 'Expected candidate directory, native target and version');
assert(match && /^\d+\.\d+\.\d+(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?$/.test(version ?? ''),
  'Expected a supported native target and version');
const [, os, arch] = match;
const platform = {windows: 'win32', darwin: 'darwin', linux: 'linux'}[os];
const runnerOS = {windows: 'Windows', darwin: 'macOS', linux: 'Linux'}[os];
const env = process.env;
assert.equal(env.GITHUB_ACTIONS, 'true');
assert.equal(env.RUNNER_ENVIRONMENT, 'github-hosted', 'Disposable GitHub-hosted runner required');
assert.equal(env.GITHUB_REPOSITORY, 'risense-ai/respire');
assert.equal(env.RUNNER_OS, runnerOS);
assert.equal(env.RUNNER_ARCH, arch === 'amd64' ? 'X64' : 'ARM64');
assert.equal(process.platform, platform);
assert.equal(process.arch, arch === 'amd64' ? 'x64' : 'arm64');
assert.equal(env.CANDIDATE_VERSION, version);
assert.match(env.GITHUB_SHA ?? '', /^[0-9a-f]{40}$/);
assert.equal(env.CANDIDATE_SOURCE, env.GITHUB_SHA);
assert(isAbsolute(rootArg) && isAbsolute(env.RUNNER_TEMP ?? ''), 'Absolute candidate and runner temp paths required');
const root = realpathSync.native(rootArg);
const temp = realpathSync.native(env.RUNNER_TEMP);
const belowTemp = relative(temp, root);
assert(belowTemp && !isAbsolute(belowTemp) && belowTemp !== '..' && !belowTemp.startsWith('..' + sep),
  'Candidate directory must be inside runner temp');
const script = (name) => fileURLToPath(new URL(name, import.meta.url));
const command = (name, args, options = {}) => execFileSync(name, args, {
  encoding: 'utf8', maxBuffer: 2 * 1024 * 1024, timeout: 120000, windowsHide: true, ...options,
});
assert.equal(command('git', ['rev-parse', 'HEAD']).trim(), env.CANDIDATE_SOURCE);
command('python', [script('desktop-candidate.py'), 'verify', '--root', root], {stdio: 'inherit'});
if (os === 'darwin') {
  assert.equal(command('/usr/bin/uname', ['-m']).trim(), arch === 'amd64' ? 'x86_64' : 'arm64');
  // Intel Macs do not provide these sysctls; Rosetta returns 1.
  const value = (key) => command('/usr/sbin/sysctl', ['-in', key]).trim();
  assert(['', '0'].includes(value('sysctl.proc_translated')), 'Rosetta is not native Intel evidence');
  if (arch === 'amd64') assert(['', '0'].includes(value('hw.optional.arm64')));
}
const work = mkdtempSync(join(temp, 'respire-desktop-check-'));
const profile = join(work, 'profile');
const checkEnv = {...env, RSRS_DATA_DIR: profile};
const binary = (path) => {
  assert(lstatSync(path).isFile(), 'Expected a regular executable');
  if (os === 'windows') {
    const data = readFileSync(path);
    assert.equal(data.readUInt16LE(0), 0x5a4d);
    const pe = data.readUInt32LE(0x3c);
    assert.equal(data.readUInt32LE(pe), 0x4550);
    assert.equal(data.readUInt16LE(pe + 4), arch === 'amd64' ? 0x8664 : 0xaa64);
  } else if (os === 'darwin') {
    assert.equal(command('/usr/bin/lipo', ['-archs', path]).trim(), arch === 'amd64' ? 'x86_64' : 'arm64');
  } else {
    const data = readFileSync(path);
    assert.deepEqual([...data.subarray(0, 6)], [0x7f, 0x45, 0x4c, 0x46, 2, 1]);
    assert.equal(data.readUInt16LE(18), arch === 'amd64' ? 62 : 183);
  }
  const info = JSON.parse(command(path, ['--version'], {env: checkEnv, timeout: 15000}));
  assert.equal(info.version, version);
  assert.equal(info.platform, target);
  const fonts = JSON.parse(command(path, ['--headless', 'fonts'], {env: checkEnv, timeout: 15000}));
  assert.equal(fonts.ok, true);
  assert(Array.isArray(fonts.result) && fonts.result.length > 0, 'No system fonts returned');
};
let mounted = false;
try {
  const unpack = join(work, 'unpack');
  mkdirSync(unpack);
  if (os === 'windows') {
    binary(join(root, 'respire-' + target + '.exe'));
    command('pwsh', ['-NoProfile', '-NonInteractive', '-File',
      script('check-native-desktop-installer.ps1'), '-Candidate', root, '-Work', work, '-Arch', arch, '-Version', version],
      {stdio: 'inherit', timeout: 300000});
  } else if (os === 'linux') {
    const packagePath = join(root, 'respire-' + target + '.tar.gz');
    assert.equal(command('tar', ['-tzf', packagePath]).trim(), 'respire', 'Unexpected archive inventory');
    assert(command('tar', ['-tvzf', packagePath]).startsWith('-'), 'Archive product is not a regular file');
    command('tar', ['-xzf', packagePath, '-C', unpack]);
    binary(join(unpack, 'respire'));
  } else {
    const packagePath = join(root, 'respire-' + target + '.zip');
    const checkZip = [
      'import stat,sys,zipfile',
      'with zipfile.ZipFile(sys.argv[1]) as z:',
      '    entries=z.infolist()',
      '    assert len({x.filename for x in entries})==len(entries)',
      '    for x in entries:',
      "        p=x.filename.rstrip('/').split('/')",
      "        assert p[0] in {'Respire.app','__MACOSX'} and all(v not in {'', '.', '..'} and chr(92) not in v for v in p)",
      '        assert stat.S_IFMT(x.external_attr >> 16) in {0, stat.S_IFREG, stat.S_IFDIR}',
    ].join('\n');
    command('python', ['-c', checkZip, packagePath]);
    command('/usr/bin/ditto', ['-x', '-k', packagePath, unpack]);
    const app = join(unpack, 'Respire.app');
    command('/usr/bin/codesign', ['--verify', '--deep', '--strict', app]);
    binary(join(app, 'Contents', 'MacOS', 'respire'));
    if (arch === 'arm64') {
      const mount = join(work, 'mount');
      const copied = join(work, 'Respire.app');
      mkdirSync(mount);
      command('/usr/bin/hdiutil', ['attach', '-readonly', '-nobrowse', '-mountpoint', mount,
        join(root, 'respire-darwin-arm64.dmg')]);
      mounted = true;
      try {
        command('/usr/bin/ditto', [join(mount, 'Respire.app'), copied]);
        command('/usr/bin/codesign', ['--verify', '--deep', '--strict', copied]);
        binary(join(copied, 'Contents', 'MacOS', 'respire'));
      } finally {
        command('/usr/bin/hdiutil', ['detach', mount]);
        mounted = false;
      }
    }
  }
  console.log('Verified real native Client2 package:', target, version);
} finally {
  if (!mounted) {
    const below = relative(temp, realpathSync.native(work));
    assert(!isAbsolute(below) && !below.includes(sep) && below.startsWith('respire-desktop-check-'));
    rmSync(work, {recursive: true, force: true});
  }
}
