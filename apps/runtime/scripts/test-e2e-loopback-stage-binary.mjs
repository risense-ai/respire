import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { stageRuntimeCargo } from './stage-runtime-cargo.mjs';
import { nativeRealpath } from './runtime-release-smoke-helpers.mjs';

const [, , rootArg, target, normalTargetDir, normalLog, featureTargetDir, featureLog, featureBinary] =
  process.argv;
assert(
  rootArg && target && normalTargetDir && normalLog && featureTargetDir && featureLog && featureBinary,
  'Usage: test-e2e-loopback-stage-binary.mjs <root> <target> <normalTargetDir> <normalLog> <featureTargetDir> <featureLog> <featureBinary>',
);

const root = nativeRealpath(resolve(rootArg));
const source = execFileSync('git', ['-C', root, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
const manifest = await readFile(join(root, 'apps/runtime/runtime/Cargo.toml'), 'utf8');
const version = /^version = "([^"]+)"/m.exec(manifest)?.[1];
assert(version, 'Failed to read runtime Cargo.toml version');
const tag = `runtime-v${version}`;

const outputParent = nativeRealpath(await mkdtemp(join(tmpdir(), 'e2e-stage-binary-')));
try {
  assert(!(await readFile(featureBinary)).includes('+e2e-loopback-mirror-absent'));
  assert((await readFile(featureBinary)).includes('+e2e-loopback-mirror'));

  const staged = await stageRuntimeCargo({
    root,
    tag,
    source,
    target,
    targetDir: resolve(normalTargetDir),
    cargoLog: resolve(normalLog),
    outputParent,
  });
  assert(staged.directory, 'Expected normal runtime binary to stage cleanly');

  await assert.rejects(
    stageRuntimeCargo({
      root,
      tag,
      source,
      target,
      targetDir: resolve(featureTargetDir),
      cargoLog: resolve(featureLog),
      outputParent,
    }),
    /Refusing to stage e2e-loopback-mirror build/,
  );
  console.log(`Verified real-binary staging gate for ${target}: normal passed, +e2e-loopback-mirror rejected.`);
} finally {
  await rm(outputParent, { recursive: true, force: true });
}
