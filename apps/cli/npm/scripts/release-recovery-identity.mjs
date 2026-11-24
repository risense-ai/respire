import { createHash } from "node:crypto";
import { pages } from "./release-publication.mjs";
import assert from "node:assert/strict";
import { armRecoveryRun } from "./release-recovery-sources.mjs";

export const recoveryWorkflow='.github/workflows/release-recovery3930.yml';
export const muslPreparationWorkflow='.github/workflows/release-musl3930-preparation.yml';
export const windowsTarget='x86_64-pc-windows-msvc';
const REPO='risense-ai/respire-cli',positive=n=>Number.isSafeInteger(Number(n))&&/^[1-9]\d*$/.test(String(n));
const hash=b=>createHash('sha256').update(b).digest('hex');
const PRODUCT='3930ba500b96a0c52eb7aa735b7ddb4117ad4e9f',BUILD_RUN='44276673815';
const suffix=s=>`-recovery-${s.run_id}-${s.run_attempt}`;
const retryable=j=>j.name.startsWith('native-regression / Native (')||j.name.startsWith('cloud-sweep (');
const targetOf=j=>j.name.startsWith('native-regression')?j.name.match(/Native \(([^)]+)\)$/)[1]:j.name.match(/, ([^)]+)\)$/)[1];
const osFor=t=>t.includes('windows')?(t.startsWith('aarch64')?'windows-11-arm':'windows-latest'):t.includes('darwin')?(t.startsWith('aarch64')?'macos-15':'macos-15-intel'):(t.startsWith('aarch64')?'ubuntu-24.04-arm':'ubuntu-latest');
const readJobs=s=>pages(`repos/${REPO}/actions/runs/${s.run_id}/attempts/${s.run_attempt}/jobs?per_page=100`).flatMap(p=>p.jobs);
const readArtifacts=s=>pages(`repos/${REPO}/actions/runs/${s.run_id}/artifacts?per_page=100`).flatMap(p=>p.artifacts);
const originOwner=s=>({sha:s.sha,run_id:Number(s.run_id),run_attempt:Number(s.run_attempt)});
const armTarget='aarch64-unknown-linux-gnu';
const armNative=`native-regression / Native (${armTarget})`;
const armReplay={run_id:44726670231,run_attempt:1,sha:'fbbe85b0aabd448d30f750d91ae488bda9284cea'};
const armReplayJob=136255766094;
const armOriginalFailure=136233485215;
const armOriginalReceipt=136275017543;
const macAggregate='native-regression / Apple Silicon upgrade aggregate';
const macShardPrefix='native-regression / Apple Silicon upgrade (';
const replayInputProof='native-replay-inputs-arm64-gnu';
const armBuild='cli-release-'+armTarget;
const armCandidate='candidate-assets-'+armTarget;
const armOriginalDigests={
  [armBuild]:'edba437b55930e36c7241bd57a43f1f614d9400b1b95cb8d70bd7a7f299ecb01',
  [armCandidate]:'ef4af3e83cf138f2f9707da4b9fc883b72ff48cc71c683d0de6536d9841e4066',
};
function successfulStep(job,name){
  const found=job.steps?.filter(s=>s.name===name) || [];
  assert.equal(found.length,1,'Missing or duplicate required step: '+name);
  assert.equal(found[0].status,'completed');
  assert.equal(found[0].conclusion,'success');
  return found[0].number;
}
const replayPinnedArtifacts={
  [replayInputProof]:'459a6b57ff6ad34bd56b1ab3246617a82ca512af339bd9089a26fccf66272034',
  ['resident-concurrency-'+armTarget]:'251a6c7d306a0538117f93c108f287448492224a27994a0bbafa2a9ce68a43dc',
  ['published-upgrades-'+armTarget]:'b8aa266ebf99bce2914892ccf02c015319c2fcd67ee186783c59f564d46483cd',
};
const isArmRecovery=producer=>producer.run_id===armRecoveryRun;
const replayName=logical=>(logical===replayInputProof?'candidate-assets-'+armTarget:logical)+`-replay-${armReplay.run_id}-${armReplay.run_attempt}`;

export { BUILD_RUN, PRODUCT, REPO, armBuild, armCandidate, armNative, armOriginalDigests, armOriginalFailure, armOriginalReceipt, armReplay, armReplayJob, armTarget, hash, isArmRecovery, macAggregate, macShardPrefix, originOwner, osFor, positive, readArtifacts, readJobs, replayInputProof, replayName, replayPinnedArtifacts, retryable, successfulStep, suffix, targetOf };
