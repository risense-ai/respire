

import test from 'node:test';
import assert from 'node:assert/strict';
import {runInNewContext} from 'node:vm';
import {readFileSync,mkdtempSync,mkdirSync,writeFileSync,existsSync,rmSync,readdirSync} from 'node:fs';
import {tmpdir} from 'node:os';
import path from 'node:path';
import {spawnSync,execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {regressionContract,validateRegressionContract,validationProfile,regressionStatus,regressionCompletedAt} from './release-regression-contract.mjs';
import {requireRegressions} from './cleanup-release-artifacts.mjs';
import {frontendLayout,releaseAssetNames,verifyBinary} from './verify-binary.mjs';
import {publishedAssetNames,parsePublishedAssets,validateArtifactRequest} from './extract-artifact.mjs';
import {createHash} from 'node:crypto';
const manifest=JSON.parse(readFileSync(new URL('../artifact-manifest.json',import.meta.url)));
const contract=regressionContract(manifest),copy=structuredClone;
// Git's Windows bin wrapper prepends system tools ahead of fixture stubs.
const bash=process.platform==='win32'?path.resolve(execFileSync('git',['--exec-path'],{encoding:'utf8'}).trim(),'../../../usr/bin/bash.exe'):'bash';
test('API smoke shell parses even when published-assets mode is inactive',()=>{
  const workflow=readFileSync(new URL('../../.github/workflows/release.yml',import.meta.url),'utf8').replace(/\r\n/g,'\n');
  const job=workflow.split('\n  api-smoke:\n')[1].split('\n  publish:\n')[0];
  const script=job.split('        run: |\n')[1].split('\n      - uses:')[0].replace(/^          /gm,'');
  const result=spawnSync(bash,['-n'],{input:script,encoding:'utf8'});
  if(result.error)throw result.error;
  assert.equal(result.status,0,result.stderr);
  assert.match(script,/dev-api-smoke.py/);assert.match(script,/dev-github-api-smoke.py/);
});
const job=name=>({name,status:'completed',conclusion:'success'});
const inventory=()=>[...contract.native.map(t=>job(`native-regression / Native (${t})`)),...contract.cloud.map(t=>job(`cloud-sweep (runner, ${t})`)),...contract.migration.map(t=>job(`Native migration (${t})`)),...contract.migration_gate.map(t=>job(`Migration gate (${t})`)),job('api-smoke'),job('publish')];

export { bash, contract, copy, inventory, job, manifest };
