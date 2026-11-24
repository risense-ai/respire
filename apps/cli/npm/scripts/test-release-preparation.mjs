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
import { fixture, hash, identity, job, manifest, sha, source, targets, time, version } from "./release-preparation-fixtures.mjs";

test('exact successful main preparation proves all builds, packaging, CI and regression lanes',()=>{
  const {receipt,context}=fixture();
  assert.equal(validatePreparation(receipt,context),receipt);
  assert.equal(receipt.validation.completed_at,new Date(time(3)).toISOString());
  assert.equal(receipt.state,'validated-unpublished');
  assert.equal(receipt.accepted_at,undefined);assert.equal(receipt.install,undefined);
});
test('different source, attempt, repository, workflow, PR, fork and incomplete run cannot authorize reuse',()=>{
  const mutations=[r=>r.head_sha='f'.repeat(40),r=>r.run_attempt=2,r=>r.id=701,r=>r.head_branch='feature',
    r=>r.event='pull_request',r=>r.head_repository.full_name='fork/respire-cli',r=>r.path='.github/workflows/ci.yml',
    r=>r.status='in_progress',r=>r.conclusion='failure'];
  for(const mutate of mutations){const {receipt,context}=fixture();mutate(context.run);assert.throws(()=>validatePreparation(receipt,context));}
  assert.throws(()=>validatePreparationRun({...fixture().context.run,status:'in_progress'},source));
});
test('every required build, package validation, regression and exact push CI remains fail closed',()=>{
  const {context:sample}=fixture();
  for(let i=0;i<sample.jobs.length;i++)for(const mode of ['failure','cancelled','skipped','missing','duplicate']){
    const {receipt,context}=fixture();
    if(mode==='missing')context.jobs.splice(i,1);
    else if(mode==='duplicate')context.jobs.push({...context.jobs[i]});
    else context.jobs[i].conclusion=mode;
    assert.throws(()=>validatePreparation(receipt,context),`${i} ${mode}`);
  }
  for(const [key,value] of [['head_sha','f'.repeat(40)],['event','workflow_dispatch'],['run_attempt',2],['conclusion','cancelled']]){
    const {receipt,context}=fixture();context.ci[key]=value;assert.throws(()=>validatePreparation(receipt,context));
  }
});
test('SDK/toolchain changes, missing clocks, expired or substituted artifacts and different bytes are refused',()=>{
  const mutations=[
    ({identity})=>identity.hashes['rust-toolchain.toml']='c'.repeat(64),
    ({identity})=>identity.sdk[0].version='1.0.2-dev.13',
    ({jobs})=>delete jobs[0].completed_at,
    ({jobs})=>jobs[0].completed_at='invalid',
    ({artifacts})=>artifacts[0].expired=true,
    ({artifacts})=>artifacts[0].id=999,
    ({artifacts})=>artifacts[0].workflow_run.id=701,
    ({artifacts})=>artifacts[0].workflow_run.head_sha='f'.repeat(40),
    ({artifacts})=>artifacts[0].digest='sha256:'+'f'.repeat(64),
    ({assets})=>assets[0].sha256='f'.repeat(64),
    ({packs})=>packs[0].sha256='f'.repeat(64),
  ];
  for(const mutate of mutations){const {receipt,context}=structuredClone(fixture());mutate(context);assert.throws(()=>validatePreparation(receipt,context));}
});
test('receipt cannot change version/inventory or claim validation before work completes',()=>{
  for(const mutate of [r=>r.source.version='1.2.3-dev.5',r=>r.assets[0]=r.assets[1],r=>r.packs.pop(),
    r=>r.prepared_at='2026-02-30T00:00:00Z']){
    const {receipt,context}=structuredClone(fixture());mutate(receipt);assert.throws(()=>validatePreparation(receipt,context));
  }
});
test('preparation uses completed API gates and its successful producer despite runner clock skew',()=>{
  for(const now of ['2026-10-26T23:04:00Z','2026-10-27T01:04:00Z']){
    const {context}=structuredClone(fixture());
    context.run.status='in_progress';context.jobs.at(-1).status='in_progress';
    const receipt=createPreparation({...context,now});
    context.run.status='completed';context.jobs.at(-1).status='completed';
    assert.equal(validatePreparation(receipt,context),receipt);
  }
  for(const mutate of [c=>c.jobs.pop(),c=>c.jobs.push({...c.jobs.at(-1)}),
    c=>c.jobs.at(-1).id='invalid',c=>c.jobs.at(-1).started_at='invalid',
    c=>c.jobs.at(-1).completed_at=time(2),c=>c.jobs.at(-1).completed_at=time(6),
    c=>c.jobs.at(-1).conclusion='failure',c=>c.jobs.at(-1).status='in_progress']){
    const {receipt,context}=structuredClone(fixture());mutate(context);
    assert.throws(()=>validatePreparation(receipt,context));
  }
});

test('source CI may finish during producer setup before the attester reads its successful result',()=>{
  const {context}=structuredClone(fixture());
  context.jobs.at(-1).started_at=time(1);context.run.status='in_progress';context.jobs.at(-1).status='in_progress';
  const receipt=createPreparation(context);
  context.run.status='completed';context.jobs.at(-1).status='completed';
  assert.equal(validatePreparation(receipt,context),receipt);
});

test('actual synthetic asset files bind source, version, platform and binary/runtime hashes',()=>{
  for(const frontend of [false,true]){
  const folder=mkdtempSync(join(tmpdir(),'prepared-assets-'));
  const layout={...manifest,...(frontend?{artifactLayout:'cli-frontend-v2'}:{})};
  try{
    for(const t of manifest.targets){
      const binary=`rsrs-${t.triple}${t.triple.includes('windows')?'.exe':''}`,runtime=`rsrs-${t.triple}-runtime.tar.gz`;
      const content=Buffer.from(t.triple),sha256=createHash('sha256').update(content).digest('hex');
      writeFileSync(join(folder,binary),content);
      if(!frontend)writeFileSync(join(folder,runtime),content);
      writeFileSync(join(folder,`cli-build-${t.triple}.json`),JSON.stringify(frontend
        ? {schema_version:2,artifact_layout:layout.artifactLayout,git_sha:sha,version,target:t.triple,
          binary_file:binary,binary_size:content.length,binary_sha256:sha256,producer:{run_id:source.run_id,run_attempt:source.run_attempt}}
        : {schema_version:1,git_sha:sha,version,target:t.triple,
          binary_file:binary,runtime_file:runtime,binary_sha256:sha256,runtime_sha256:sha256}));
    }
    assert.equal(preparedAssets(folder,source,layout).length,frontend?8:12);
    if(frontend)assert.deepEqual(preparedAssets(folder,{...source,run_id:'701',producer:source},layout),preparedAssets(folder,source,layout));
    const metadata=join(folder,`cli-build-${targets[0][0]}.json`),original=JSON.parse(readFileSync(metadata));
    for(const mutate of frontend?[m=>m.binary_size++,m=>m.producer.run_attempt++,m=>m.producer.run_id='999',
      m=>m.runtime_file='unexpected-runtime.tar.gz',m=>m.schema_version=1]:[m=>m.schema_version=2]){
      const changed=structuredClone(original);mutate(changed);writeFileSync(metadata,JSON.stringify(changed));
      assert.throws(()=>preparedAssets(folder,source,layout));writeFileSync(metadata,JSON.stringify(original));
    }
    writeFileSync(join(folder,`rsrs-${targets[0][0]}`),'tampered');assert.throws(()=>preparedAssets(folder,source,layout));
  }finally{rmSync(folder,{recursive:true,force:true});}
  }
});
test('actual synthetic npm packs must match the full exact-version local pack index',()=>{
  const folder=mkdtempSync(join(tmpdir(),'prepared-packs-'));
  try{
    const packages=[...manifest.targets.map(t=>t.pkg),manifest.npmScope].map((name,i)=>{
      const file=`pack-${i}.tgz`,bytes=Buffer.from(name);writeFileSync(join(folder,file),bytes);
      return {name,version,file,sha256:createHash('sha256').update(bytes).digest('hex')};
    });
    writeFileSync(join(folder,'source-version.json'),JSON.stringify({schema_version:1,git_sha:sha,version,packages}));
    assert.equal(preparedPacks(folder,source,manifest).length,5);
    writeFileSync(join(folder,packages[0].file),'tampered');assert.throws(()=>preparedPacks(folder,source,manifest));
  }finally{rmSync(folder,{recursive:true,force:true});}

});

test('Release attests all full candidate consumers and publishes only verified imported bytes',async()=>{
  const release=readFileSync(new URL('../../.github/workflows/release.yml',import.meta.url),'utf8').replace(/\r\n/g,'\n');
  assert.match(release,/needs: \[plan, validate, native-regression, cloud-sweep, api-smoke\]/);
  assert.match(release,/release-preparation.mjs attest release-assets full-reports\/npm-release-packages release-prepared.json full-reports/);
  assert.match(release,/prepared_binary_run: \$\{\{ steps.prepared.outputs.prepared_binary_run \}\}/);
  assert.equal((release.match(/run-id: \$\{\{ needs.plan.outputs.prepared_binary_run \}\}/g)||[]).length,2);
  assert.equal((release.match(/git archive "\$GITHUB_SHA" npm\/scripts\/\{release-recovery3930,release-recovery-sources,release-preparation,candidate-artifact,extract-artifact,publication-acceptance,release-publication,verified-receipt\}.mjs \| tar -x/g)||[]).length,4);
  const continuation=readFileSync(new URL('../../.github/workflows/release-recovery3930.yml',import.meta.url),'utf8');
  assert.match(continuation,/uses: \.\/.github\/workflows\/release-native-validation.yml/);
  assert.match(continuation,/validation_profile: full/);
  assert.doesNotMatch(continuation,/contents: write|actions: write|GITHUB_(SHA|RUN_ID|RUN_ATTEMPT):|gh run rerun|gh workflow run/);
  const publisher=release.split('\n  publish:\n')[1];
  assert.match(publisher,/needs.prepared-import.result == 'success'/);
  assert.match(publisher,/needs.publication-ready.result == 'success'/);
  assert.match(publisher,/--reuse-packs npm\/release-packages --export-packs npm\/publication-packages/);
  assert.doesNotMatch(publisher,/--pack npm\/publication-packages/);
  const folder=mkdtempSync(join(tmpdir(),'musl-report-origins-'));
  try{
    const logical='published-upgrades-x86_64-unknown-linux-musl';
    const tags=['v1.0.7','v1.0.8','v1.0.9','v1.0.11','v1.0.11-dev.1','v1.0.12','v1.0.15','v1.0.16','v1.0.17-dev.6','v1.0.18','v1.0.19','v1.0.20'];
    const anchor=(id,name,run,conclusion)=>({id,name,digest:'sha256:'+'a'.repeat(64),owner:{run_id:run,run_attempt:1},conclusion});
    const origins={
      old:{directory:join(folder,'old'),anchor:anchor('14424335334',logical+'-recovery-44292748654-1',44292748654,'failure')},
      current:{directory:join(folder,'current'),anchor:anchor('2',logical+'-musl-source-3-1',3,'success')},
      summary:{directory:join(folder,'summary'),anchor:anchor('4','musl3930-composition-5-1',5,'success')}};
    origins.old.anchor.digest='sha256:23646cc75cf81d80852f752e7391994dd0751fe296a112707bfce55e458aef31';
    const reports=[];
    for(const path of ['published-upgrades.json',...tags.map(tag=>tag+'/results/real-account-return-verification.json')]){
      const origin=path==='published-upgrades.json'?origins.summary:path.startsWith('v1.0.8/')?origins.current:origins.old;
      const file=join(origin.directory,path);mkdirSync(join(file,'..'),{recursive:true});
      const bytes=JSON.stringify({test_only:path});writeFileSync(file,bytes);
      reports.push({artifact:logical,path,sha256:createHash('sha256').update(bytes).digest('hex')});
    }
    const result={reports,historical_upgrades:[{target:'x86_64-unknown-linux-musl',passed:tags,not_applicable:[]}]};
    const bound=bindMuslReportOrigins(result,origins);
    assert.equal(bound.reports.filter(r=>r.artifact===origins.old.anchor.name).length,11);
    assert.equal(bound.reports.filter(r=>r.artifact===origins.current.anchor.name).length,1);
    assert.equal(bound.reports.filter(r=>r.artifact===origins.summary.anchor.name).length,1);
    assert.ok(result.reports.every(r=>r.artifact===logical));
    const wrong=structuredClone(result);wrong.reports[1].sha256='0'.repeat(64);
    assert.throws(()=>bindMuslReportOrigins(wrong,origins),/absent from its claimed ZIP/);
    assert.throws(()=>bindMuslReportOrigins(result,{...origins,summary:origins.current}));
    const missing=structuredClone(result);missing.reports.pop();
    assert.throws(()=>bindMuslReportOrigins(missing,origins),/twelve sources/);
    writeFileSync(join(origins.current.directory,'v1.0.8/results/real-account-return-verification.json'),'changed');
    assert.throws(()=>bindMuslReportOrigins(result,origins),/absent from its claimed ZIP/);
  }finally{rmSync(folder,{recursive:true,force:true});}
  const fixed='710e6b13ff588c02157d8d677e59e8f21c16beae',first='d'.repeat(40),validator='e'.repeat(40);
  const composedSource={...source,workflow:muslPreparationWorkflow,run_id:'13',workflow_head_sha:'c'.repeat(40),producer:source,
    fixture_commit:fixed,diagnostic:{...muslDiagnostic}};
  const fixture={commit:fixed,files:{'scripts/test-published-upgrades.py':validator}};
  const logical='published-upgrades-x86_64-unknown-linux-musl';
  const sourceRows=[...Object.entries(muslOldHashes),['v1.0.8','a'.repeat(64)]].map(([tag,digest])=>({source:tag,contract:'historical-385',
    fixture_commit:tag==='v1.0.8'?fixed:first,validator_blob:validator,report_sha256:digest}));
  const old={name:logical+'-recovery-44292748654-1'},current={name:logical+`-musl-source-${muslDiagnostic.run_id}-1`},summary={name:'musl3930-composition-13-1'};
  const proof={manifest_sha256:hash,historical_upgrades:[{target:'x86_64-unknown-linux-musl',passed:sourceRows.map(r=>r.source),not_applicable:[]}],
    reports:[...sourceRows.map(r=>({artifact:r.source==='v1.0.8'?current.name:old.name,path:r.source+'/results/real-account-return-verification.json',sha256:r.report_sha256})),
      {artifact:summary.name,path:'published-upgrades.json',sha256:hash}],continuation:{artifacts:[old,current,summary],musl:{schema:1,kind:'fixed-musl3930-composition',
        producer:source,diagnostic:composedSource.diagnostic,harness_sha:composedSource.workflow_head_sha,fixture,runtime_sha256:hash,candidate_sha256:hash,
        sources:sourceRows,summary_sha256:hash,summary_artifact:summary,source_artifacts:{old,current}}}};
  validateMuslReportProof(proof,composedSource,fixture,first);
  for(const mutate of [p=>p.reports[0].artifact=current.name,p=>p.continuation.musl.sources[0].report_sha256='0'.repeat(64),
    p=>p.continuation.musl.sources.pop(),p=>p.reports.at(-1).artifact=current.name,p=>p.continuation.musl.diagnostic.run_attempt=2]){
    const bad=structuredClone(proof);mutate(bad);assert.throws(()=>validateMuslReportProof(bad,composedSource,fixture,first));
  }
  for(const file of ['dev-legacy-vault-smoke.py','dev-migration-keyring.py','test-published-upgrades.py','test-real-m3-account-switch.py',
    'test-dev-credential-alias-fixtures.py','test-full-release-diagnostics.py','test_published_upgrade_policy.py'])fixture.files['scripts/'+file]=validator;
  const required=[...manifest.targets.map(t=>`validate / Release (${t.triple})`),'validate / npm-dry-run','api-smoke',
    ...manifest.targets.map(t=>`native-regression / Native (${t.triple})`),...manifest.targets.filter(t=>t.triple!=='x86_64-unknown-linux-gnu').map(t=>`cloud-sweep (runner, ${t.triple})`)];
  const originalJobs=required.map((name,i)=>({name,id:100+i,status:'completed',conclusion:name==='native-regression / Native (x86_64-unknown-linux-musl)'?'failure':'success',
    run_id:Number(source.run_id),run_attempt:1,head_sha:source.sha}));
  const oldNative={name:'native-regression / Native (x86_64-unknown-linux-musl)',id:400,status:'completed',conclusion:'failure',run_id:44292748654,run_attempt:1,head_sha:first,
    steps:['Verify OS autostart registration from verified release bytes','Install and probe BGE CPU inference'].map(name=>({name,conclusion:'success'}))};
  const newNative={...oldNative,id:134805531846,conclusion:'success',run_id:Number(muslDiagnostic.run_id),head_sha:composedSource.diagnostic.sha};
  const compose={name:'compose',id:402,status:'completed',conclusion:'success',run_id:13,run_attempt:1,head_sha:composedSource.workflow_head_sha};
  for(const [a,j,id] of [[old,oldNative,'14424335334'],[current,newNative,'600'],[summary,compose,'601']])Object.assign(a,
    {id,job_id:j.id,size:1,digest:'sha256:'+'a'.repeat(64),conclusion:j.conclusion,owner:{run_id:j.run_id,run_attempt:1,sha:j.head_sha}});
  old.digest='sha256:23646cc75cf81d80852f752e7391994dd0751fe296a112707bfce55e458aef31';
  const firstRun={status:'completed',conclusion:'failure',path:recoveryWorkflow,head_branch:'main',event:'workflow_dispatch',head_repository:{full_name:source.repository},head_sha:first};
  const diagnosticRun={...firstRun,id:Number(muslDiagnostic.run_id),run_attempt:1,conclusion:'success',path:'.github/workflows/release-musl3930.yml',head_sha:composedSource.diagnostic.sha};
  const read=async path=>path==='actions/runs/44292748654/attempts/1'?firstRun:path===`actions/runs/${muslDiagnostic.run_id}/attempts/1`?diagnosticRun:
    path==='commits/385c3c9'?{sha:first}:path.startsWith('contents/')?{type:'file',sha:validator}:assert.fail('Unexpected read '+path);
  const readJobs=async owner=>String(owner.run_id)==='44292748654'?[oldNative]:String(owner.run_id)===muslDiagnostic.run_id?[newNative]:assert.fail('Unexpected job owner');
  await inspectMuslComposition(composedSource,proof,originalJobs,[compose],manifest,read,readJobs);
  diagnosticRun.status='in_progress';await assert.rejects(inspectMuslComposition(composedSource,proof,originalJobs,[compose],manifest,read,readJobs));diagnosticRun.status='completed';
  compose.conclusion='cancelled';await assert.rejects(inspectMuslComposition(composedSource,proof,originalJobs,[compose],manifest,read,readJobs));compose.conclusion='success';
  current.owner.run_attempt=2;await assert.rejects(inspectMuslComposition(composedSource,proof,originalJobs,[compose],manifest,read,readJobs));current.owner.run_attempt=1;
  const mixed=spawnSync(process.platform==='win32'?'py':'python3',[...(process.platform==='win32'?['-3']:[]),'-c',String.raw`
import copy,json,pathlib,runpy,tempfile
m=runpy.run_path('scripts/merge-musl3930-upgrades.py')['merge']
tags=['v1.0.7','v1.0.8','v1.0.9','v1.0.11','v1.0.11-dev.1','v1.0.12','v1.0.15','v1.0.16','v1.0.17-dev.6','v1.0.18','v1.0.19','v1.0.20']
policy=runpy.run_path('scripts/published_upgrade_policy.py')['DATABASE_DEV_CHECKPOINTS']
base={k:True for k in ['passed','real_model','distinct_vault_keys','original_records_preserved','original_sync_work_preserved','urk_preserved','old_records_decrypted','post_upgrade_recall_checked','encrypted_tombstones_preserved','standalone_runtime_evidence','runtime_artifact_reused','normal_reopen_checked','post_upgrade_sync_checked']}
base.update({k:False for k in ['old_rpc_checked','forced_restart_checked','live_owner_restart_rejected','safe_restart_checked','supervised_worker_checked','in_process_checked','global_engine_preserved']})
base.update(seed_vault_versions=[1,3],historical_credential_migration_actor='fixture',host_credential_custody={'required':True,'cleanup_passed':True,'verified_accounts':[{'account':'main','vault_version':1,'urk_preserved':True},{'account':'alternate','vault_version':3,'urk_preserved':True}]},native_credential_cleanup={'passed':True,'remaining_entries':[]},upgrade_contract='standalone-v1',legacy_handoff_actor=None,coverage_scope='historical-database',host_checks_delegated_to='resident-concurrency',cloud_cleanup={'passed':True,'remaining_users':[]},runtime_mode='standalone_runtime',runtime_binary_sha256='a'*64,runtime_version='2.0.0-dev.24',runtime_identity_receipts=[{'pid':1}],binary_sha256='b'*64,engine='cpu',sequence=[{'account':a,'model_rows':{'embedder':{'status':'ok'},'model index':{'status':'ok'}}} for a in ['main','alternate','main']])
base['post_upgrade_sync']={k:True for k in ['original_urk_preserved','existing_keys_registered','source_push','independent_pull_decrypted','reverse_marker_source_pull','original_active_preserved','tombstones_propagated','pending_work_acknowledged']}
with tempfile.TemporaryDirectory() as d:
 root=pathlib.Path(d);old=root/'old';new=root/'new';old.mkdir();new.mkdir()
 summary={'target':'x86_64-unknown-linux-musl','candidate_sha256':'b'*64,'selected_sources':sorted(tags),'published_formal_sources':[t for t in tags if '-dev.' not in t],'database_dev_checkpoints':policy,'additional_draft_baselines':[]}
 (old/'published-upgrades.json').write_text(json.dumps(summary))
 (new/'published-upgrades.json').write_text(json.dumps(dict(summary,selected_sources=['v1.0.8'],diagnostic_sources=['v1.0.8'],passed=True,full_coverage=False,all_published_sources_covered=False)))
 for tag in tags:
  r=copy.deepcopy(base)
  if tag in policy:r['database_dev_checkpoint']={'tag':tag,'special_state_seeded':True,'special_state_preserved':True,'verified_accounts':['main','alternate'],'post_sync_special_state_preserved':True,'sync_verified_accounts':['main']}
  p=old/tag/'results';p.mkdir(parents=True);(p/'real-account-return-verification.json').write_text(json.dumps([r]))
 r=copy.deepcopy(base)
 p=new/'v1.0.8'/'results';p.mkdir(parents=True);f=p/'real-account-return-verification.json';f.write_text(json.dumps([r]));original=f.read_bytes()
 evidence=m(old,new,root/'merged','710e6b13ff588c02157d8d677e59e8f21c16beae','a'*64,'2.0.0-dev.24')
 assert len(evidence)==12 and all(x['contract']=='historical-385' for x in evidence)
 assert json.loads((root/'merged/published-upgrades.json').read_text())['sources'][0]['report'].endswith('/results/real-account-return-verification.json')
 for i,mutation in enumerate(['sync','failed','missing']):
  if mutation=='sync':bad=copy.deepcopy(r);del bad['post_upgrade_sync']['original_urk_preserved'];f.write_text(json.dumps([bad]))
  else:f.write_bytes(original)
  if mutation=='failed':bad=copy.deepcopy(base);bad['passed']=False;(old/'v1.0.7/results/real-account-return-verification.json').write_text(json.dumps([bad]))
  if mutation=='missing':(old/'v1.0.9/results/real-account-return-verification.json').unlink()
  try:m(old,new,root/('rejected'+str(i)),'710e6b13ff588c02157d8d677e59e8f21c16beae','a'*64,'2.0.0-dev.24')
  except AssertionError:pass
  else:raise AssertionError('Invalid source evidence accepted')
`],{encoding:'utf8'});
  assert.equal(mixed.status,0,mixed.stderr);
});

import "./release-attester-tests.mjs";

test('preparation requires full GUI and DB evidence and never upgrades critical or old incomplete proofs',()=>{
  const f=fixture();
  for(const change of [r=>delete r.full_result,r=>r.full_result.gui_passed=false,r=>r.full_result.full_passed=false,
    r=>r.full_result.historical_upgrades.pop(),r=>r.full_result.run_attempt++,r=>r.validation_profile='critical',
    r=>r.full_result.schema=1,r=>delete r.full_result.scope,r=>r.full_result.profile_import.status='passed',
    r=>r.validation.inventory.schema=1,r=>delete r.validation.inventory.scope]){
    const receipt=structuredClone(f.receipt);change(receipt);assert.throws(()=>validatePreparation(receipt,f.context));
  }
  assert.throws(()=>createPreparation({...f.context,run:{...f.context.run,status:'in_progress'},profile:'critical'}));
  const skipped=structuredClone(f.context);skipped.jobs.find(j=>j.name.startsWith('validate / Release')).conclusion='skipped';
  assert.throws(()=>validatePreparation(f.receipt,skipped));
});

test('binds the one ARM replay and all original Apple Silicon jobs to their exact owners',()=>{
  const p=approvedRecoveryProducer('44718206331');
  const producerOwner={run_id:Number(p.run_id),run_attempt:1,head_sha:p.sha};
  const arm='aarch64-unknown-linux-gnu';
  const native=t=>`native-regression / Native (${t})`;
  const all={targets:[...manifest.targets,{triple:arm}]};
  const completed={status:'completed',conclusion:'success'};
  const job=(name,id)=>({name,id,...producerOwner,...completed});
  const armFailed={...job(native(arm),136233485215),conclusion:'failure',steps:[
    {name:'Verify and extract the release binary',number:17,...completed},
    {name:'Upload candidate receipt',number:22,...completed},
  ]};
  const shards=Array.from({length:12},(_,i)=>job(`native-regression / Apple Silicon upgrade (v1.0.${i+6})`,200+i));
  const aggregate=job('native-regression / Apple Silicon upgrade aggregate',240);
  const original=[...all.targets.flatMap(({triple:t},i)=>[
    job(`validate / Release (${t})`,50+i),t===arm?armFailed:job(native(t),60+i),
    ...(t==='x86_64-unknown-linux-gnu'?[]:[job(`cloud-sweep (test-runner, ${t})`,70+i)]),
  ]),job('validate / npm-dry-run',80),job('api-smoke',81),...shards,aggregate,
  {...job('preparation-receipt',136275017543),conclusion:'failure'}];
  const harness='fbbe85b0aabd448d30f750d91ae488bda9284cea';
  const replay={id:44726670231,run_attempt:1,head_sha:harness,head_branch:'codex/cli-arm64-gnu-native-replay-44718206331',
    head_repository:{full_name:p.repository},path:'.github/workflows/dev-sweep.yml',event:'workflow_dispatch',...completed};
  const replayJob={name:native(arm),id:136255766094,run_id:replay.id,run_attempt:1,head_sha:harness,...completed,steps:[
    'Require real native Linux ARM64 hardware',
    'Verify exact original CLI and official Runtime for this diagnostic replay',
    'Install and verify the official pinned standalone Runtime',
    'Verify OS autostart registration from verified release bytes',
    'Install and probe BGE CPU inference',
    'Verify selected formal release and database DEV upgrades',
    'Verify resident foreground and background inference under concurrent CLI load',
  ].map(name=>({name,...completed}))};
  const rJobs=[replayJob,{...replayJob,name:'verify-selection',id:136255731145}];
  const consumer={...p,workflow:recoveryWorkflow,run_id:'701',workflow_head_sha:'c'.repeat(40),producer:p};
  const run=(old=original,now=[],proof=replayJob)=>recoveryJobs(old,now,p,consumer,all,{armReplayEvidence:proof});
  assert.equal(validateArmReplay(replay,rJobs),replayJob);
  const joined=run();
  // Empty workflow matrix from recovery run 44736281076, including its synthetic job clocks.
  const skippedCloud={
    id:136288596510,run_id:44736281076,run_attempt:1,head_sha:'36dc605c481480554d2f31dbdc5d25a2f3326bea',
    name:'cloud-sweep (${{ matrix.os }}, ${{ matrix.target }})',status:'completed',conclusion:'skipped',steps:[],
    started_at:'2026-11-20T12:30:30Z',completed_at:'2026-11-20T12:30:29Z',
  };
  assert.deepEqual(run(original,[skippedCloud]),joined);
  for(const invalid of [
    {...skippedCloud,name:'cloud-sweep (ubuntu-24.04-arm, aarch64-unknown-linux-gnu)'},
    {...skippedCloud,status:'in_progress'},
    {...skippedCloud,conclusion:'success'},
    {...skippedCloud,steps:undefined},
    {...skippedCloud,steps:[{name:'Set up job',status:'completed',conclusion:'success'}]},
  ])assert.throws(()=>run(original,[invalid]),/cannot be combined/);
  assert.throws(()=>run(original,[skippedCloud,skippedCloud]),/cannot be combined/);
  assert.equal(joined.evidenceOwners[native(arm)].run_id,replay.id);
  assert.equal(joined.evidenceOwners[aggregate.name].run_id,Number(p.run_id));
  assert.ok(shards.every(s=>joined.evidenceOwners[s.name].run_attempt===1));
  assert.throws(()=>approvedRecoveryProducer('44731065057'),/Unapproved/);
  assert.throws(()=>validateArmReplay({...replay,run_attempt:2},rJobs));
  assert.throws(()=>validateArmReplay({...replay,head_sha:p.sha},rJobs));
  assert.throws(()=>validateArmReplay(replay,[{...replayJob,steps:replayJob.steps.slice(0,-1)},rJobs[1]]));
  assert.throws(()=>run(original,[],{...replayJob,id:9}));
  assert.throws(()=>run(original,[replayJob]),/cannot be combined/);
  assert.throws(()=>run(original.filter(j=>j!==aggregate)),/aggregate/);
  assert.throws(()=>run(original.filter(j=>j!==shards[0])),/twelve/);
  assert.throws(()=>run(original.map(j=>j===aggregate?{...j,run_attempt:2}:j)));
  assert.throws(()=>run(original.map(j=>j.name==='api-smoke'?{...j,conclusion:'failure'}:j)),/exactly/);
  assert.throws(()=>run(original.map(j=>j===armFailed?{...j,steps:j.steps.slice(1)}:j)),/required step/);
});

test('requires the original ARM build and standard receipt to match the unmodified replay proof',()=>{
  const p=approvedRecoveryProducer('44718206331');
  const owner={run_id:Number(p.run_id),run_attempt:1,sha:p.sha};
  const source={...owner,version:p.version};
  const target='aarch64-unknown-linux-gnu';
  const harness={sha:'fbbe85b0aabd448d30f750d91ae488bda9284cea',run_id:'44726670231',run_attempt:'1'};
  const candidate={runtime:{version:'2.0.0-dev.28'}};
  const manifest_sha256=createHash('sha256').update(JSON.stringify(candidate)).digest('hex');
  const anchor=(prefix,digest,conclusion)=>({name:prefix+target,digest:'sha256:'+digest,owner,conclusion});
  const build=anchor('cli-release-','edba437b55930e36c7241bd57a43f1f614d9400b1b95cb8d70bd7a7f299ecb01','success');
  const prior=anchor('candidate-assets-','ef4af3e83cf138f2f9707da4b9fc883b72ff48cc71c683d0de6536d9841e4066','failure');
  const original={kind:'candidate-assets-verification',target,manifest_sha256,candidate,
    assets_verified:true,payloads_verified:true,runtime_producer:{run_id:39},
    payload_identity:{harness_sha:p.sha,cli_binary_sha256:'1'.repeat(64),runtime_binary_sha256:'2'.repeat(64)}};
  const replay={schema:1,kind:'native-replay-inputs',diagnostic_only:true,target,source,harness,candidate,manifest_sha256,
    build:{...build,owner:source},prior:{...prior,owner:source},runtime_producer:original.runtime_producer,
    payload_identity:{...original.payload_identity,harness_sha:harness.sha}};
  const check=(input=replay,previous=original)=>validateArmReplayInputs(input,previous,
    {...build,logical_name:build.name},{...prior,logical_name:prior.name},candidate);
  check();
  assert.throws(()=>check({...replay,diagnostic_only:false}));
  assert.throws(()=>check({...replay,prior:{...replay.prior,digest:build.digest}}));
  assert.throws(()=>check({...replay,kind:'candidate-assets-verification'}));
  assert.throws(()=>check({...replay,runtime_producer:{run_id:40}}));
  assert.throws(()=>check({...replay,payload_identity:{...replay.payload_identity,cli_binary_sha256:'3'.repeat(64)}}));
  assert.throws(()=>check(replay,{...original,assets_verified:false}));
});
