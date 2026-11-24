import { mkdirSync, readFileSync, writeFileSync, copyFileSync, lstatSync, readdirSync } from "node:fs";
import { join } from "node:path";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { api, pages } from "./release-publication.mjs";
import { preparationInputs, preparedAssets, preparedPacks, createPreparation } from "./release-preparation.mjs";
import { completeCandidate } from "./candidate-artifact.mjs";
import { PRODUCT, REPO, hash, muslPreparationWorkflow, positive, readArtifacts, readJobs, recoveryWorkflow, retryable, suffix, windowsTarget } from "./release-recovery-identity.mjs";
import { failedArtifacts, readCandidate, recoveryScripts, requestedProducer } from "./release-recovery3930.mjs";
import { downloadRecoveryArtifact, recoveryArtifact } from "./release-recovery-artifact-io.mjs";

export const muslOldHashes={
  'v1.0.7':'936be264b4fdeba3203bf33e53e23630616b196eff3a636d7be80d5b4c481a63',
  'v1.0.9':'30526bacf1d38903d796db59df959b7e03a0c53121d6b50b2181c34ff610bd84',
  'v1.0.11':'4d62273aabd2baffadd73c4bc92acfae8803ffa2e25ad20639063b557a5296cc',
  'v1.0.11-dev.1':'8d58a2156b441eb4e102d53e4d5e653bf1987666029b90e2356f27b64c69f2fb',
  'v1.0.12':'93a095d78e4478d44c9dcf80dd3d47090cab0c8e6f999c4e2075d6fc365df59b',
  'v1.0.15':'1c930b021785a124deceeec96b56573de77323d8254f073a933f6693a986b991',
  'v1.0.16':'abcdbc2e0cfaaac603bccce51c0748cd15b6c6a13832b5ab88127aaa324b0124',
  'v1.0.17-dev.6':'7fa1bedaba29562a77eaaecc3833b8fc71fa934117c7c8fd64f2da9b013afb19',
  'v1.0.18':'dda910c068edf8466c05ca775a8c4bb81453bcc66c0783a69bbb0cc67950f860',
  'v1.0.19':'328c9c3f1bb15f96d762131b2919987969664b6333495684d747e7172ef78fc2',
  'v1.0.20':'87622f6125c0f5cf55fa09382fbe4d2ac56e5515dd1847557e8af3013af6e660'};

function muslReportFile(directory,name){
  const matches=[];
  function walk(relative=''){
    for(const entry of readdirSync(join(directory,relative))){
      const path=relative?relative+'/'+entry:entry;
      assert.ok(!/[\\:]/.test(path),'Unsafe evidence path');
      const info=lstatSync(join(directory,path));
      assert.ok(!info.isSymbolicLink(),'Symlink in evidence');
      if(info.isDirectory())walk(path);
      else {
        assert.ok(info.isFile(),'Non-regular evidence');
        if(path===name||path.endsWith('/'+name))matches.push(path);
      }
    }
  }
  walk();assert.equal(matches.length,1,'Missing or duplicate original report');
  return {path:matches[0],sha256:hash(readFileSync(join(directory,matches[0])))};
}

export function bindMuslReportOrigins(result,{old,current,summary}){
  const target='x86_64-unknown-linux-musl',logical='published-upgrades-'+target;
  const sources=[...Object.keys(muslOldHashes),'v1.0.8'];
  assert.equal(old.anchor.id,'14424335334');
  assert.equal(old.anchor.digest,'sha256:23646cc75cf81d80852f752e7391994dd0751fe296a112707bfce55e458aef31');
  assert.equal(old.anchor.owner.run_id,44292748654);assert.equal(old.anchor.owner.run_attempt,1);
  assert.equal(old.anchor.conclusion,'failure');
  assert.equal(old.anchor.name,logical+'-recovery-44292748654-1');
  for(const input of [current,summary]){
    assert.equal(input.anchor.conclusion,'success');
    assert.match(input.anchor.digest,/^sha256:[a-f0-9]{64}$/);
    assert.match(String(input.anchor.id),/^[1-9]\d*$/);
    assert.notEqual(String(input.anchor.id),old.anchor.id);
  }
  assert.notEqual(String(current.anchor.id),String(summary.anchor.id));
  assert.equal(current.anchor.name,logical+`-musl-source-${current.anchor.owner.run_id}-${current.anchor.owner.run_attempt}`);
  assert.equal(summary.anchor.name,`musl3930-composition-${summary.anchor.owner.run_id}-${summary.anchor.owner.run_attempt}`);
  const coverage=result.historical_upgrades.filter(row=>row.target===target);
  assert.equal(coverage.length,1);assert.deepEqual(coverage[0].passed.slice().sort(),sources.slice().sort());
  assert.deepEqual(coverage[0].not_applicable,[]);
  const consumed=result.reports.filter(report=>report.artifact===logical);
  assert.equal(consumed.length,13,'Require twelve sources and one derived summary');
  const replacements=new Map();
  for(const name of ['published-upgrades.json',...sources.map(tag=>tag+'/results/real-account-return-verification.json')]){
    const matching=consumed.filter(report=>report.path===name||report.path.endsWith('/'+name));
    assert.equal(matching.length,1,'Missing or duplicate consumed musl report');
    const input=name==='published-upgrades.json'?summary:name.startsWith('v1.0.8/')?current:old;
    const original=muslReportFile(input.directory,name);
    assert.equal(original.sha256,matching[0].sha256,'Report is absent from its claimed ZIP or has changed');
    replacements.set(matching[0],{artifact:input.anchor.name,...original});
  }
  assert.equal(replacements.size,13);
  return {...result,reports:result.reports.map(report=>replacements.get(report)||report)};
}

const muslTarget='x86_64-unknown-linux-musl',firstRecovery='44292748654';
const muslFixture='710e6b13ff588c02157d8d677e59e8f21c16beae';
const muslReview='65b26b5',muslBase='8f1314f476806f7182869653091ea1eae179cc1d';
export const muslDiagnostic={run_id:'44305349743',run_attempt:1,sha:muslBase};
const git=(...args)=>execFileSync('git',args,{encoding:'utf8',maxBuffer:16*1024*1024}).trim();
const muslFiles=['scripts/dev-legacy-vault-smoke.py','scripts/dev-migration-keyring.py','scripts/test-published-upgrades.py','scripts/test-real-m3-account-switch.py',
  'scripts/test-dev-credential-alias-fixtures.py','scripts/test-full-release-diagnostics.py','scripts/test_published_upgrade_policy.py'];
function muslFixtureIdentity(){
  git('merge-base','--is-ancestor',muslFixture,'origin/main');
  return {commit:muslFixture,files:Object.fromEntries(muslFiles.map(f=>[f,git('rev-parse',`${muslFixture}:${f}`)]))};
}
function muslRun(identity,path,conclusion){
  const run=api(`repos/${REPO}/actions/runs/${identity.run_id}/attempts/${identity.run_attempt}`);
  assert.equal(run.id,Number(identity.run_id));assert.equal(run.run_attempt,Number(identity.run_attempt));
  assert.equal(run.path,path);assert.equal(run.head_sha,identity.sha);assert.equal(run.head_branch,'main');
  assert.equal(run.event,'workflow_dispatch');assert.equal(run.head_repository?.full_name,REPO);
  assert.equal(run.status,'completed');assert.equal(run.conclusion,conclusion);return run;
}
function muslHarness(sha){
  git('merge-base','--is-ancestor',muslBase,sha);git('merge-base','--is-ancestor',sha,'origin/main');
  const allowed=new Set([muslPreparationWorkflow,'npm/scripts/release-recovery3930.mjs','npm/scripts/release-preparation.mjs',
    'npm/scripts/test-release-preparation.mjs','scripts/merge-musl3930-upgrades.py',
    'npm/scripts/extract-artifact.mjs','npm/scripts/test-release-publication.mjs','scripts/probe-client2-gui.ps1']);
  assert.ok(git('diff','--name-only',muslBase,sha).split(/\r?\n/).filter(Boolean).every(p=>allowed.has(p)),'Unreviewed composition product, fixture or harness change');
}
export function muslPreparationContext(source){
  assert.deepEqual(source.producer,requestedProducer());assert.equal(source.workflow,muslPreparationWorkflow);
  assert.equal(source.fixture_commit,muslFixture);assert.equal(source.sha,PRODUCT);assert.equal(source.version,'2.0.0-dev.19');
  assert.equal(source.repository,REPO);muslHarness(source.workflow_head_sha);
  const run=api(`repos/${REPO}/actions/runs/${source.run_id}/attempts/${source.run_attempt}`);
  assert.equal(run.id,Number(source.run_id));assert.equal(run.run_attempt,Number(source.run_attempt));assert.equal(run.path,muslPreparationWorkflow);
  assert.equal(run.head_sha,source.workflow_head_sha);assert.equal(run.head_repository?.full_name,REPO);
  assert.equal(run.head_branch,'main');assert.equal(run.event,'workflow_dispatch');assert.ok(['in_progress','completed'].includes(run.status));
  if(run.status==='completed')assert.equal(run.conclusion,'success');
  muslRun(source.producer,'.github/workflows/release.yml','failure');
  const first={run_id:firstRecovery,run_attempt:1,sha:git('rev-parse','385c3c9')};
  muslRun(first,recoveryWorkflow,'failure');
  const diagnostic=source.diagnostic;assert.deepEqual(diagnostic,muslDiagnostic);
  assert.match(diagnostic.sha,/^[a-f0-9]{40}$/);muslRun(diagnostic,'.github/workflows/release-musl3930.yml','success');
  git('merge-base','--is-ancestor',diagnostic.sha,'origin/main');
  for(const file of ['.github/workflows/release-musl3930.yml','.github/workflows/release-native-validation.yml','npm/scripts/release-musl3930.mjs','scripts/test-native-runtime-install.py'])
    assert.equal(git('rev-parse',`${diagnostic.sha}:${file}`),git('rev-parse',`${muslReview}:${file}`),'Diagnostic execution harness differs from reviewed PR451');
  const inputs=preparationInputs(PRODUCT,source.version),original=readJobs(source.producer),middle=readJobs(first),current=readJobs(diagnostic),receiptJobs=readJobs(source);
  const selection={},evidenceOwners={},joined=[];
  const required=[...inputs.manifest.targets.map(t=>`validate / Release (${t.triple})`),'validate / npm-dry-run','api-smoke',
    ...inputs.manifest.targets.map(t=>`native-regression / Native (${t.triple})`),...inputs.manifest.targets.filter(t=>t.triple!=='x86_64-unknown-linux-gnu').map(t=>t.triple)];
  for(const scope of required){
    const match=j=>j.name===scope||j.name.startsWith('cloud-sweep (')&&j.name.endsWith(', '+scope+')');
    const before=original.filter(match);assert.equal(before.length,1);
    const tier=scope===`native-regression / Native (${muslTarget})`?'diagnostic':before[0].conclusion==='success'?'original':'first';
    const owner=tier==='original'?source.producer:tier==='first'?first:diagnostic;
    const found=(tier==='original'?original:tier==='first'?middle:current).filter(match);assert.equal(found.length,1);
    const job=found[0];assert.equal(job.status,'completed');assert.equal(job.conclusion,'success');
    if(tier==='diagnostic')assert.equal(job.id,134805531846);
    assert.equal(job.run_id,Number(owner.run_id));assert.equal(job.run_attempt,Number(owner.run_attempt));assert.equal(job.head_sha,owner.sha);
    const normalized={run_id:Number(owner.run_id),run_attempt:Number(owner.run_attempt),sha:owner.sha};
    selection[job.name]={job,owner:normalized,tier};evidenceOwners[job.name]=normalized;joined.push(job);
  }
  const oldNative=middle.filter(j=>j.name===`native-regression / Native (${muslTarget})`);assert.equal(oldNative.length,1);
  assert.equal(oldNative[0].conclusion,'failure');assert.equal(oldNative[0].status,'completed');
  for(const stepName of ['Verify OS autostart registration from verified release bytes','Install and probe BGE CPU inference']){
    const steps=oldNative[0].steps.filter(s=>s.name===stepName);assert.equal(steps.length,1);assert.equal(steps[0].conclusion,'success');
  }
  const runs=pages(`repos/${REPO}/actions/workflows/ci.yml/runs?head_sha=${PRODUCT}&per_page=100`).flatMap(p=>p.workflow_runs)
    .filter(r=>r.head_sha===PRODUCT&&r.event==='push'&&r.head_branch==='main').sort((a,b)=>b.id-a.id);
  assert.ok(runs.length);const ci=api(`repos/${REPO}/actions/runs/${runs[0].id}`);assert.equal(ci.status,'completed');assert.equal(ci.conclusion,'success');
  return {...inputs,source,run,ci,original,middle,current,receiptJobs,selection,evidenceOwners,first,diagnostic,oldNative:oldNative[0],fixture:muslFixtureIdentity(),
    jobs:[...joined,...receiptJobs.filter(j=>j.name==='preparation-receipt')],artifacts:readArtifacts(source.producer),firstArtifacts:readArtifacts(first),
    diagnosticArtifacts:readArtifacts(diagnostic),consumerArtifacts:readArtifacts(source),scripts:{...recoveryScripts(PRODUCT,first.sha),consumer_sha:source.workflow_head_sha}};
}
function muslAnchors(context){
  const anchors=[],add=(logical,name)=>{
    let s=context.selection[name];assert.ok(s);
    // The full autostart/asset proof ran before the failed historical source on 385.
    if(logical==='candidate-assets-'+muslTarget)s={job:context.oldNative,owner:{...context.first,run_id:Number(firstRecovery)},tier:'first'};
    const suffix=s.tier==='original'?'':s.tier==='first'?`-recovery-${firstRecovery}-1`:`-musl-source-${context.diagnostic.run_id}-${context.diagnostic.run_attempt}`;
    const inventory=s.tier==='original'?context.artifacts:s.tier==='first'?context.firstArtifacts:context.diagnosticArtifacts;
    anchors.push({...recoveryArtifact(inventory,logical+suffix,s.job,s.owner,{failed:logical==='candidate-assets-'+muslTarget}),logical_name:logical});
  };
  for(const {triple:t} of context.manifest.targets){
    add('cli-release-'+t,`validate / Release (${t})`);const native=`native-regression / Native (${t})`;
    for(const prefix of ['candidate-assets-','resident-concurrency-','published-upgrades-'])if(!(t===muslTarget&&prefix==='published-upgrades-'))add(prefix+t,native);
    if(t===windowsTarget)add('client2-published-online-'+t,native);
    if(t==='x86_64-unknown-linux-gnu'){add('gnu-login-interruption',native);continue;}
    const cloud=Object.keys(context.selection).filter(n=>n.startsWith('cloud-sweep (')&&n.endsWith(', '+t+')'));assert.equal(cloud.length,1);
    add('base-release-sweep-'+t,cloud[0]);if(t.includes('linux'))for(const prefix of ['release-sweep-','secret-service-profile-migration-'])add(prefix+t,cloud[0]);
  }
  for(const name of ['npm-release-validation','npm-release-packages'])add(name,'validate / npm-dry-run');add('dev-api-coverage','api-smoke');return anchors;
}
function muslSourceAnchors(context){
  const name='published-upgrades-'+muslTarget;
  const old={...recoveryArtifact(context.firstArtifacts,name+`-recovery-${firstRecovery}-1`,context.oldNative,{...context.first,run_id:Number(firstRecovery)},{failed:true})};
  assert.equal(old.id,'14424335334');assert.equal(old.digest,'sha256:23646cc75cf81d80852f752e7391994dd0751fe296a112707bfce55e458aef31');
  const s=context.selection[`native-regression / Native (${muslTarget})`];
  const current=recoveryArtifact(context.diagnosticArtifacts,name+`-musl-source-${s.owner.run_id}-${s.owner.run_attempt}`,s.job,s.owner);
  return {old,current};
}
function muslSummaryAnchor(context){
  const jobs=context.receiptJobs.filter(j=>j.name==='compose');assert.equal(jobs.length,1);
  return recoveryArtifact(context.consumerArtifacts,`musl3930-composition-${context.source.run_id}-${context.source.run_attempt}`,jobs[0],
    {run_id:Number(context.source.run_id),run_attempt:Number(context.source.run_attempt),sha:context.source.workflow_head_sha});
}
function muslSource(){
  assert.equal(process.env.GITHUB_REF,'refs/heads/main');assert.equal(process.env.GITHUB_EVENT_NAME,'workflow_dispatch');assert.equal(process.env.GITHUB_REPOSITORY,REPO);
  const diagnostic={...muslDiagnostic};
  return {repository:REPO,workflow:muslPreparationWorkflow,run_id:process.env.GITHUB_RUN_ID,run_attempt:Number(process.env.GITHUB_RUN_ATTEMPT),
    workflow_head_sha:process.env.GITHUB_SHA,sha:PRODUCT,version:'2.0.0-dev.19',producer:requestedProducer(),fixture_commit:muslFixture,diagnostic};
}
function composeMuslFiles(context,root){
  const anchors=muslSourceAnchors(context);
  for(const [role,anchor] of Object.entries(anchors))downloadRecoveryArtifact(anchor,join(root,role));
  const candidate=readCandidate(context,join(root,'candidate'));
  const runtime=candidate.runtime.assets.find(a=>a.name==='rsrs-runtime-'+muslTarget);assert.ok(runtime);
  const directory=join(root,'merged');
  execFileSync(process.platform==='win32'?'py':'python3',[...(process.platform==='win32'?['-3']:[]),'scripts/merge-musl3930-upgrades.py',
    join(root,'old'),join(root,'current'),directory,muslFixture,runtime.sha256,candidate.runtime.version],{stdio:'inherit'});
  const summary=JSON.parse(readFileSync(join(directory,'published-upgrades.json')));
  assert.equal(summary.source_evidence.length,12);
  for(const row of summary.source_evidence)if(row.source!=='v1.0.8')assert.equal(row.report_sha256,muslOldHashes[row.source]);
  const audit={schema:1,kind:'fixed-musl3930-composition',producer:context.source.producer,diagnostic:context.diagnostic,fixture:context.fixture,
    candidate_sha256:hash(JSON.stringify(candidate)),runtime_sha256:runtime.sha256,sources:summary.source_evidence,source_artifacts:anchors,
    harness_sha:context.source.workflow_head_sha,summary_sha256:hash(readFileSync(join(directory,'published-upgrades.json')))};
  return {candidate,anchors,directory,audit};
}
export function composeMusl(root,outputFolder){
  const context=muslPreparationContext(muslSource()),composed=composeMuslFiles(context,root);
  mkdirSync(outputFolder,{recursive:true});copyFileSync(join(composed.directory,'published-upgrades.json'),join(outputFolder,'published-upgrades.json'));
  writeFileSync(join(outputFolder,'composition-evidence.json'),JSON.stringify(composed.audit,null,2)+'\n');return composed.audit;
}
export function attestMuslPreparation(root,file){
  const source=muslSource(),context=muslPreparationContext(source),composed=composeMuslFiles(context,join(root,'source-check'));
  const summary=muslSummaryAnchor(context),summaryRoot=join(root,'composition');downloadRecoveryArtifact(summary,summaryRoot);
  assert.deepEqual(JSON.parse(readFileSync(join(summaryRoot,'composition-evidence.json'))),composed.audit,'Composition audit was substituted');
  assert.equal(hash(readFileSync(join(summaryRoot,'published-upgrades.json'))),composed.audit.summary_sha256,'Persisted summary differs from actual source revalidation');
  const anchors=muslAnchors(context);for(const anchor of anchors)downloadRecoveryArtifact(anchor,join(root,anchor.logical_name));
  // This is a local read view. Report attribution is replaced with physical ZIPs below.
  const view=join(root,'published-upgrades-'+muslTarget);mkdirSync(view);copyFileSync(join(summaryRoot,'published-upgrades.json'),join(view,'published-upgrades.json'));
  for(const tag of [...Object.keys(muslOldHashes),'v1.0.8']){
    const directory=join(view,tag,'results');mkdirSync(directory,{recursive:true});
    copyFileSync(join(composed.directory,tag,'results','real-account-return-verification.json'),join(directory,'real-account-return-verification.json'));
  }
  const assetsRoot=join(root,'release-assets');mkdirSync(assetsRoot);
  for(const {triple:t} of context.manifest.targets)for(const name of [`rsrs-${t}${t.includes('windows')?'.exe':''}`,`cli-build-${t}.json`])copyFileSync(join(root,'cli-release-'+t,name),join(assetsRoot,name));
  const assets=preparedAssets(assetsRoot,source,context.manifest);
  const result=completeCandidate(composed.candidate,assets,root,context.jobs,source,{evidenceOwners:context.evidenceOwners});
  const origins={old:{anchor:composed.anchors.old,directory:join(root,'source-check','old')},current:{anchor:composed.anchors.current,directory:join(root,'source-check','current')},summary:{anchor:summary,directory:summaryRoot}};
  const full_result=bindMuslReportOrigins(result,origins);
  full_result.continuation={producer:source.producer,consumer:{run_id:source.run_id,run_attempt:source.run_attempt,sha:source.workflow_head_sha},scripts:context.scripts,
    artifacts:[...anchors,...Object.values(composed.anchors),summary],failed_artifacts:[...failedArtifacts(context),{...composed.anchors.old,reused:false}],
    failed_jobs:[...context.original.filter(j=>retryable(j)&&j.conclusion==='failure'),context.oldNative].map(j=>({id:j.id,name:j.name,run_attempt:j.run_attempt})),
    musl:{...composed.audit,summary_artifact:summary}};
  const receipt=createPreparation({...context,assets,packs:preparedPacks(join(root,'npm-release-packages'),source,context.manifest),full_result});
  writeFileSync(file,JSON.stringify(receipt,null,2)+'\n');return receipt;
}
export function validateMuslPreparation(result,context){
  const c=result.continuation,audit=c.musl;
  assert.deepEqual(c.producer,context.source.producer);assert.deepEqual(c.consumer,{run_id:context.source.run_id,run_attempt:context.source.run_attempt,sha:context.source.workflow_head_sha});
  assert.deepEqual(c.scripts,context.scripts);assert.deepEqual(audit.fixture,context.fixture);assert.deepEqual(audit.diagnostic,context.diagnostic);
  const anchors=muslSourceAnchors(context),summary=muslSummaryAnchor(context);
  assert.deepEqual(c.artifacts,[...muslAnchors(context),...Object.values(anchors),summary]);
  assert.deepEqual(c.failed_artifacts,[...failedArtifacts(context),{...anchors.old,reused:false}]);
  assert.deepEqual(c.failed_jobs,[...context.original.filter(j=>retryable(j)&&j.conclusion==='failure'),context.oldNative].map(j=>({id:j.id,name:j.name,run_attempt:j.run_attempt})));
  assert.deepEqual(audit.source_artifacts,anchors);assert.deepEqual(audit.summary_artifact,summary);
  validateMuslReportProof(result,context.source,context.fixture,context.first.sha);
}
export function validateMuslReportProof(result,source,fixture,firstSHA){
  const c=result.continuation,audit=c.musl,anchors=audit.source_artifacts,summary=audit.summary_artifact;
  assert.equal(audit.schema,1);assert.equal(audit.kind,'fixed-musl3930-composition');assert.deepEqual(audit.producer,source.producer);
  assert.deepEqual(audit.diagnostic,source.diagnostic);assert.equal(audit.harness_sha,source.workflow_head_sha);
  assert.deepEqual(audit.fixture,fixture);assert.match(audit.runtime_sha256,/^[a-f0-9]{64}$/);
  assert.equal(audit.candidate_sha256,result.manifest_sha256);assert.equal(audit.sources.length,12);
  assert.deepEqual(audit.sources.map(s=>s.source).sort(),[...Object.keys(muslOldHashes),'v1.0.8'].sort());
  const coverage=result.historical_upgrades.filter(r=>r.target===muslTarget);assert.equal(coverage.length,1);
  assert.deepEqual(coverage[0].passed.slice().sort(),audit.sources.map(s=>s.source).sort());assert.deepEqual(coverage[0].not_applicable,[]);
  for(const row of audit.sources){
    assert.equal(row.contract,'historical-385');assert.equal(row.fixture_commit,row.source==='v1.0.8'?muslFixture:firstSHA);
    assert.equal(row.validator_blob,fixture.files['scripts/test-published-upgrades.py']);
    if(row.source!=='v1.0.8')assert.equal(row.report_sha256,muslOldHashes[row.source]);
    const physical=row.source==='v1.0.8'?anchors.current:anchors.old;
    const reports=result.reports.filter(r=>r.artifact===physical.name&&r.path.endsWith(row.source+'/results/real-account-return-verification.json'));
    assert.equal(reports.length,1);assert.equal(reports[0].sha256,row.report_sha256);
  }
  const summaries=result.reports.filter(r=>r.artifact===summary.name&&r.path==='published-upgrades.json');assert.equal(summaries.length,1);
  assert.equal(summaries[0].sha256,audit.summary_sha256);
  assert.ok(result.reports.every(r=>c.artifacts.some(a=>a.name===r.artifact||a.logical_name===r.artifact)),'Unbound composition report');
}
export async function inspectMuslComposition(source,result,original,receiptJobs,manifest,read,readJobs){
  const audit=result.continuation.musl,first={run_id:firstRecovery,run_attempt:1};
  const previous=await read(`actions/runs/${firstRecovery}/attempts/1`);
  assert.equal(previous.status,'completed');assert.equal(previous.conclusion,'failure');assert.equal(previous.path,recoveryWorkflow);
  assert.equal(previous.head_branch,'main');assert.equal(previous.event,'workflow_dispatch');assert.equal(previous.head_repository?.full_name,REPO);
  const firstCommit=await read('commits/385c3c9');assert.equal(previous.head_sha,firstCommit.sha);first.sha=previous.head_sha;
  assert.equal(source.fixture_commit,muslFixture);const diagnostic=source.diagnostic;
  assert.deepEqual(diagnostic,muslDiagnostic);
  const execution=await read(`actions/runs/${diagnostic.run_id}/attempts/${diagnostic.run_attempt}`);
  assert.equal(execution.id,Number(diagnostic.run_id));assert.equal(execution.run_attempt,Number(diagnostic.run_attempt));
  assert.equal(execution.head_sha,diagnostic.sha);assert.equal(execution.path,'.github/workflows/release-musl3930.yml');
  assert.equal(execution.head_branch,'main');assert.equal(execution.event,'workflow_dispatch');assert.equal(execution.head_repository?.full_name,REPO);
  assert.equal(execution.status,'completed');assert.equal(execution.conclusion,'success');
  for(const file of ['.github/workflows/release-musl3930.yml','.github/workflows/release-native-validation.yml','npm/scripts/release-musl3930.mjs','scripts/test-native-runtime-install.py']){
    const actual=await read(`contents/${file}?ref=${diagnostic.sha}`),expected=await read(`contents/${file}?ref=${muslReview}`);
    assert.equal(actual.type,'file');assert.equal(actual.sha,expected.sha);
  }
  const fixture={commit:muslFixture,files:{}};
  for(const file of muslFiles){const blob=await read(`contents/${file}?ref=${muslFixture}`);assert.equal(blob.type,'file');fixture.files[file]=blob.sha;}
  const contract=await read(`contents/scripts/test-published-upgrades.py?ref=${first.sha}`);
  assert.equal(contract.sha,fixture.files['scripts/test-published-upgrades.py']);
  const middle=await readJobs(first),current=await readJobs(diagnostic),selected=[];
  const required=[...manifest.targets.map(t=>`validate / Release (${t.triple})`),'validate / npm-dry-run','api-smoke',
    ...manifest.targets.map(t=>`native-regression / Native (${t.triple})`),...manifest.targets.filter(t=>t.triple!=='x86_64-unknown-linux-gnu').map(t=>t.triple)];
  for(const scope of required){
    const match=j=>j.name===scope||j.name.startsWith('cloud-sweep (')&&j.name.endsWith(', '+scope+')');
    const before=original.filter(match);assert.equal(before.length,1);
    const musl=scope===`native-regression / Native (${muslTarget})`,reuse=!musl&&before[0].conclusion==='success';
    const found=(musl?current:reuse?original:middle).filter(match);assert.equal(found.length,1);const job=found[0],owner=musl?diagnostic:reuse?source.producer:first;
    assert.equal(job.status,'completed');assert.equal(job.conclusion,'success');assert.equal(job.run_id,Number(owner.run_id));
    assert.equal(job.run_attempt,Number(owner.run_attempt));assert.equal(job.head_sha,owner.sha);selected.push(job);
  }
  const oldNative=middle.filter(j=>j.name===`native-regression / Native (${muslTarget})`);assert.equal(oldNative.length,1);assert.equal(oldNative[0].conclusion,'failure');
  for(const name of ['Verify OS autostart registration from verified release bytes','Install and probe BGE CPU inference']){
    const steps=oldNative[0].steps.filter(s=>s.name===name);assert.equal(steps.length,1);assert.equal(steps[0].conclusion,'success');
  }
  const compose=receiptJobs.filter(j=>j.name==='compose');assert.equal(compose.length,1);assert.equal(compose[0].conclusion,'success');assert.equal(compose[0].status,'completed');
  const native=current.filter(j=>j.name===`native-regression / Native (${muslTarget})`);assert.equal(native.length,1);
  assert.equal(native[0].id,134805531846);
  const anchors=audit.source_artifacts;
  assert.equal(anchors.old.id,'14424335334');assert.equal(anchors.old.digest,'sha256:23646cc75cf81d80852f752e7391994dd0751fe296a112707bfce55e458aef31');
  for(const [anchor,job,owner,name] of [[anchors.old,oldNative[0],first,`published-upgrades-${muslTarget}-recovery-${firstRecovery}-1`],
    [anchors.current,native[0],diagnostic,`published-upgrades-${muslTarget}-musl-source-${diagnostic.run_id}-${diagnostic.run_attempt}`],
    [audit.summary_artifact,compose[0],{run_id:source.run_id,run_attempt:source.run_attempt,sha:source.workflow_head_sha},`musl3930-composition-${source.run_id}-${source.run_attempt}`]]){
    assert.equal(anchor.name,name);assert.equal(anchor.job_id,job.id);assert.equal(anchor.conclusion,job.conclusion);
    assert.deepEqual(anchor.owner,{run_id:Number(owner.run_id),run_attempt:Number(owner.run_attempt),sha:owner.sha});
    assert.ok(positive(anchor.id)&&positive(anchor.size));assert.match(anchor.digest,/^sha256:[a-f0-9]{64}$/);
    assert.equal(result.continuation.artifacts.filter(a=>JSON.stringify(a)===JSON.stringify(anchor)).length,1);
  }
  for(const anchor of result.continuation.artifacts){
    const jobs=[...selected,oldNative[0],compose[0]].filter(j=>j.id===anchor.job_id);assert.equal(jobs.length,1);
    assert.equal(jobs[0].run_id,anchor.owner.run_id);assert.equal(jobs[0].run_attempt,anchor.owner.run_attempt);assert.equal(jobs[0].head_sha,anchor.owner.sha);
  }
  validateMuslReportProof(result,source,fixture,first.sha);
}

export { muslFiles };
