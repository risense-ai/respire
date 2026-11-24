

import test from 'node:test';
import {approvedRecoveryProducer} from './release-recovery-sources.mjs';
import {validateArmReplay, validateArmReplayInputs} from './release-recovery3930.mjs';
import assert from 'node:assert/strict';
import {mkdtempSync,writeFileSync,rmSync,readFileSync,mkdirSync,existsSync,readdirSync} from 'node:fs';
import {join} from 'node:path';
import {tmpdir} from 'node:os';
import {createHash} from 'node:crypto';
import {spawnSync,execFileSync} from 'node:child_process';
import {fileURLToPath,pathToFileURL} from 'node:url';
import {publishedFullFixture} from './full-report-fixture.mjs';
import {publishedAssetNames,inspectPublishedRelease,validateRecoveredRuntime} from './extract-artifact.mjs';
import {assertReleaseAssets} from './release-publication.mjs';
import {createPreparation,validatePreparation,preparedAssets,preparedPacks,validatePreparationRun} from './release-preparation.mjs';
import {completeCandidate} from './candidate-artifact.mjs';
import {fenceBody} from './publication-fence.mjs';
import {bindMuslReportOrigins,muslOldHashes,validateMuslReportProof,inspectMuslComposition,muslPreparationWorkflow,muslDiagnostic} from './release-recovery3930.mjs';
import {recoveryWorkflow,windowsTarget,validateRecoveryRuns,recoveryJobs,recoveryArtifact,recoveryArtifacts,
  downloadRecoveryArtifact,validateRecoveryEvidence,recoveryScripts,cloudScope,recoveryPublicationBody,recoveryPublicationEvidence,recoveryProofName} from './release-recovery3930.mjs';

const sha='3930ba500b96a0c52eb7aa735b7ddb4117ad4e9f',hash='b'.repeat(64),version='2.0.0-dev.19';
const targets=[['x86_64-unknown-linux-gnu','linux-x64-gnu'],['x86_64-unknown-linux-musl','linux-x64'],['aarch64-apple-darwin','macos-arm64'],['x86_64-pc-windows-msvc','win-x64']];
const manifest={cliVersion:version,npmScope:'@rsrsai/cli',targets:targets.map(([triple,name])=>({triple,pkg:`@rsrsai/${name}`}))};
const source={repository:'risense-ai/respire-cli',workflow:'.github/workflows/release.yml',run_id:'44276673815',run_attempt:1,sha,version};
const identity={hashes:{'Cargo.lock':hash,'rust-toolchain.toml':hash},sdk:targets.map(([target])=>({target,version:'1.0.2-dev.12',manifest_sha256:hash,archive_sha256:hash}))};
const time=minute=>`2026-10-27T00:${String(minute).padStart(2,'0')}:00Z`;
const job=name=>({name,status:'completed',conclusion:'success',started_at:time(0),completed_at:time(3)});
function fixture(){
  const jobs=[...targets.map(([t])=>job(`validate / Release (${t})`)),job('validate / npm-dry-run'),
    ...targets.map(([t])=>job(`native-regression / Native (${t})`)),
    ...targets.slice(1).map(([t])=>job(`cloud-sweep (runner, ${t})`)),
    job('api-smoke')];
  const run={id:44276673815,run_attempt:1,path:source.workflow,head_repository:{full_name:source.repository},head_sha:sha,
    head_branch:'main',event:'workflow_dispatch',status:'in_progress',updated_at:time(5)};
  const ci={id:600,run_attempt:1,path:'.github/workflows/ci.yml',head_repository:{full_name:source.repository},head_sha:sha,
    head_branch:'main',event:'push',status:'completed',conclusion:'success',run_started_at:time(0),updated_at:time(2)};
  const artifacts=[...targets.map(([t])=>`cli-release-${t}`),'npm-release-validation','npm-release-packages'].map((name,i)=>({
    name,id:i+1,size_in_bytes:100,digest:`sha256:${hash}`,expired:false,workflow_run:{id:44276673815,head_sha:sha}}));
  const assets=targets.flatMap(([t])=>[`rsrs-${t}${t.includes('windows')?'.exe':''}`,`rsrs-${t}-runtime.tar.gz`,`cli-build-${t}.json`]).map(file=>({file,size:100,sha256:hash}));
  const packs=[...targets.map(([,n])=>`@rsrsai/${n}`),'@rsrsai/cli'].map((name,i)=>({name,version,file:`pack-${i}.tgz`,size:100,sha256:hash}));
  jobs.push({id:900,name:'preparation-receipt',status:'in_progress',started_at:time(3)});
  const full_result={schema:2,scope:'database-in-place-upgrade',profile_import:{required:false,status:'not-run',diagnostic_workflow:'.github/workflows/migration-keyring-check.yml'},kind:'candidate-full',full_passed:true,gui_passed:true,harness_sha:source.sha,product_sources:{cli:source.sha},manifest_sha256:hash,run_id:Number(source.run_id),run_attempt:Number(source.run_attempt),historical_upgrades:manifest.targets.map(t=>({target:t.triple,profile:'full',all_published_sources_covered:true})),reports:[{artifact:'client2-published-online-x86_64-pc-windows-msvc',path:'report.json',sha256:hash}]};
  const context={source,identity,manifest,run,jobs,ci,artifacts,assets,packs,full_result,now:time(4)};
  const receipt=createPreparation(context);
  Object.assign(jobs.at(-1),{status:'completed',conclusion:'success',completed_at:time(4)});
  context.run={...context.run,status:'completed',conclusion:'success'};
  return {receipt,context};
}

export { fixture, hash, identity, job, manifest, sha, source, targets, time, version };
