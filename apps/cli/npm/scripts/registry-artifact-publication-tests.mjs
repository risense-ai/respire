import { spawnSync } from "node:child_process";
import assert from "node:assert/strict";
import path from "node:path";
import { chmodSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { test } from "node:test";
import { publishAcceptedPackage, currentPackSource, downloadValidatedPacks, packProducer, readValidatedPacks, validatedPackArtifact, verifyPackFile } from "./registry-publication.mjs";
import { tmpdir } from "node:os";
import { bytes, digest, expected, fastClock, fixture, missing, mockNpm, registry } from "./registry-publication-fixtures.mjs";

function pythonFixture(script,args,input) {
  const result=spawnSync(process.platform==='win32'?'py':'python3',
    [...(process.platform==='win32'?['-3']:[]),'-c',script,...args],{encoding:'utf8',input});
  assert.equal(result.status,0,result.stderr || result.error?.message);
}
function sealedPack(t,mutateEntries=entries=>entries) {
  const {root,dir}=fixture(t), tarballPath=path.join(root,expected.filename);
  const pkg={name:expected.name,version:expected.version,dependencies:{'@rsrsai/linux':expected.version}};
  writeFileSync(path.join(dir,'package.json'),JSON.stringify(pkg));
  writeFileSync(path.join(dir,'LICENSE'),'license bytes');
  // The archive/receipt declare 0644; do not inherit the caller's umask.
  for (const file of ['package.json','LICENSE']) chmodSync(path.join(dir,file),0o644);
  const entries=['package.json','LICENSE'].map(file=>({name:`package/${file}`,data:readFileSync(path.join(dir,file),'utf8')}));
  pythonFixture(`import io,json,sys,tarfile
with tarfile.open(sys.argv[1],'w:gz') as t:
 for e in json.loads(sys.argv[2]):
  m=tarfile.TarInfo(e['name']); m.mode=0o644; data=e['data'].encode()
  if 'type' in e: m.type=e['type'].encode(); m.linkname='package/LICENSE'
  m.size=len(data) if m.isfile() else 0
  t.addfile(m,io.BytesIO(data) if m.isfile() else None)
`,[tarballPath,JSON.stringify(mutateEntries(entries))]);
  const body=readFileSync(tarballPath), report={...expected,...digest(body),size:body.length,
    files:entries.map(e=>({path:e.name.slice(8),size:Buffer.byteLength(e.data),mode:0o644})),
    sha256:createHash('sha256').update(body).digest('hex'),tarballPath};
  return {root,dir,body,report};
}
const packSource={repository:'test/cli',run_id:'100',run_attempt:2,sha:'a'.repeat(40)};
function packEvidence(body) {
  return {run:{id:100,run_attempt:2,head_sha:packSource.sha,head_repository:{full_name:packSource.repository},
    path:'.github/workflows/release.yml',head_branch:'main',event:'workflow_dispatch',status:'in_progress'},
  jobs:[{id:200,name:'validate / npm-dry-run',run_id:100,run_attempt:2,head_sha:packSource.sha,status:'completed',conclusion:'success'}],
  artifacts:[{id:300,name:'npm-release-packages',expired:false,workflow_run:{id:100,head_sha:packSource.sha},
    size_in_bytes:body.length,digest:`sha256:${createHash('sha256').update(body).digest('hex')}`}]};
}
test('validated tarball reuse requires exact run/attempt/source, unique successful producer and digested artifact',()=>{
  const evidence=packEvidence(bytes),validate=e=>validatedPackArtifact(e.run,e.jobs,e.artifacts,packSource);
  assert.equal(validate(evidence).id,300);
  for(const change of [e=>e.run.run_attempt++,e=>e.run.head_sha='b'.repeat(40),e=>e.run.head_repository.full_name='other/repo',
    e=>e.jobs.push({...e.jobs[0]}),e=>e.jobs[0].run_attempt++,e=>e.jobs[0].head_sha='b'.repeat(40),
    e=>e.jobs[0].run_id++,e=>e.jobs[0].status='in_progress',e=>e.jobs[0].conclusion='failure',
    e=>e.artifacts.push({...e.artifacts[0]}),e=>e.artifacts[0].expired=true,e=>delete e.artifacts[0].digest,
    e=>e.artifacts[0].workflow_run.head_sha='b'.repeat(40)]) {
    const e=structuredClone(evidence);change(e);assert.throws(()=>validate(e),/Validated pack/);
  }
  assert.deepEqual(packProducer(packSource.sha,{GITHUB_JOB:'npm-dry-run',GITHUB_REPOSITORY:'test/cli',GITHUB_RUN_ID:'100',GITHUB_RUN_ATTEMPT:'2'}),{...packSource,job:'npm-dry-run'});
  assert.equal(packProducer(packSource.sha,{}),undefined);
  assert.throws(()=>packProducer(packSource.sha,{GITHUB_JOB:'publish',GITHUB_RUN_ID:'100'}),/producer identity/);
  assert.deepEqual(packProducer(packSource.sha,{GITHUB_JOB:'publish',GITHUB_REPOSITORY:'test/cli',GITHUB_RUN_ID:'100',GITHUB_RUN_ATTEMPT:'2'}),{...packSource,job:'publish'});
  const parent=structuredClone(evidence), publicationSource={...packSource,job:'publish'};
  parent.run.head_sha='b'.repeat(40);parent.run.head_branch='v1.2.3';parent.run.event='push';
  Object.assign(parent.jobs[0],{name:'publish',head_sha:parent.run.head_sha,status:'in_progress',conclusion:null});
  Object.assign(parent.artifacts[0],{name:`npm-publication-packages-${packSource.run_id}-${packSource.run_attempt}`,workflow_run:{id:100,head_sha:parent.run.head_sha}});
  assert.equal(validatedPackArtifact(parent.run,parent.jobs,parent.artifacts,publicationSource).id,300);
  parent.jobs[0].run_attempt++;assert.throws(()=>validatedPackArtifact(parent.run,parent.jobs,parent.artifacts,publicationSource));
  assert.throws(()=>currentPackSource({}),/incomplete/);
});
test('validated archive is submitted unchanged with no npm pack and with the original journal/ACK boundary',async t=>{
  const {dir,body,report}=sealedPack(t),mock=registry({absent:Infinity}),intents=[],calls=[];
  verifyPackFile(report.tarballPath,report,dir);
  const result=await publishAcceptedPackage(dir,report,'dev',{...mock,...fastClock(),onSubmission:e=>intents.push(e),
    spawnImpl:(command,args)=>{calls.push(args);assert.equal(args[0],'publish');assert.deepEqual(readFileSync(args[1]),body);return {status:0};}});
  assert.equal(result.acceptance.evidence,'npm-exit-zero');assert.equal(intents.length,1);assert.equal(intents[0].state,'unknown');
  assert.equal(calls.length,1);assert.equal(mock.calls.length,2,'no propagation or public installation wait');
  assert(existsSync(report.tarballPath),'caller-owned immutable archive is preserved');
  let submissions=0;
  await assert.rejects(publishAcceptedPackage(dir,report,'dev',{...registry({absent:Infinity}),...fastClock(),
    spawnImpl:(command,args)=>{assert.equal(args[0],'publish');submissions++;return {status:1};}}),/failed or was uncertain/);
  assert.equal(submissions,1,'reused tarballs never authorize retrying an unknown npm result');
});
test('registry preflight and awaited active fence cannot hide changed private submission bytes',async t=>{
  for(const stage of ['preflight','beforeWrite']) {
    const {dir,report,body}=sealedPack(t),existing=new Set(readdirSync(tmpdir()));let intents=0,submissions=0,guards=0,changed=false;
    const tamper=()=>{
      const found=readdirSync(tmpdir()).filter(name=>name.startsWith('rsrs-npm-publish-')&&!existing.has(name))
        .map(name=>path.join(tmpdir(),name,report.filename)).filter(existsSync);
      assert.equal(found.length,1);const changedBytes=Buffer.from(body);changedBytes[changedBytes.length-1]^=1;
      writeFileSync(found[0],changedBytes);changed=true;
    };
    const reads=registry({absent:Infinity});
    await assert.rejects(publishAcceptedPackage(dir,report,'dev',{...fastClock(),
      fetchImpl:async (...args)=>{if(stage==='preflight'&&!changed)tamper();return reads.fetchImpl(...args);},
      beforeWrite:async()=>{guards++;await Promise.resolve();if(stage==='beforeWrite')tamper();},
      onSubmission:()=>intents++,spawnImpl:()=>{submissions++;return {status:0};}}),/SHA256/);
    assert(changed);assert.equal(guards,1,'the active source/fence check is retained');
    assert.equal(intents,0);assert.equal(submissions,0);
    assert.deepEqual(readFileSync(report.tarballPath),body,'test modified the private snapshot, not the original cache file');
  }
});
test('legacy pack snapshots are rechecked after preflight and active fence without requiring a caller SHA256',async t=>{
  const {dir}=fixture(t);
  assert.equal(expected.sha256,undefined);
  for(const stage of ['preflight','beforeWrite']) {
    const npm=mockNpm(),reads=registry({absent:Infinity});let snapshot,intents=0,guards=0,changed=false;
    const tamper=()=>{const body=Buffer.from(readFileSync(snapshot));body[body.length-1]^=1;writeFileSync(snapshot,body);changed=true;};
    await assert.rejects(publishAcceptedPackage(dir,expected,'dev',{...fastClock(),
      spawnImpl:(command,args,options)=>{
        const result=npm.spawnImpl(command,args,options);
        if(args[0]==='pack')snapshot=path.join(args[args.indexOf('--pack-destination')+1],expected.filename);
        return result;
      },
      fetchImpl:async (...args)=>{if(stage==='preflight'&&!changed)tamper();return reads.fetchImpl(...args);},
      beforeWrite:async()=>{guards++;await Promise.resolve();if(stage==='beforeWrite')tamper();},
      onSubmission:()=>intents++}),/SHA256/);
    assert(changed);assert.equal(guards,1);assert.equal(intents,0);
    assert.equal(npm.calls.filter(c=>c.args[0]==='publish').length,0);
    assert(!existsSync(snapshot),'rejected private snapshot is cleaned up');
  }
});
test('tar member paths, links, duplicates, omissions, modes and dependency bytes fail closed before registry or intent',async t=>{
  const mutations=[entries=>[...entries,entries[0]],entries=>entries.slice(1),entries=>[...entries,{name:'../escape',data:'x'}],
    entries=>[...entries,{name:'package/extra',data:'x'}],entries=>entries.map((e,i)=>i?e:{...e,name:'package/../escape'}),
    ...['1','2','6'].map(type=>entries=>entries.map((e,i)=>i?e:{...e,type})),
    entries=>entries.map((e,i)=>i?e:{...e,data:e.data.replace('linux','wrong')})];
  for(const mutate of mutations) {
    const {dir,report}=sealedPack(t,mutate);let writes=0,reads=0;
    await assert.rejects(publishAcceptedPackage(dir,report,'dev',{fetchImpl:()=>{reads++;return missing();},
      onSubmission:()=>writes++,spawnImpl:()=>{writes++;return {status:0};}}));
    assert.equal(reads,0);assert.equal(writes,0);
  }
  const {dir,report}=sealedPack(t);
  report.files[0].mode=0o755;assert.throws(()=>verifyPackFile(report.tarballPath,report,dir),/Command failed/);
  report.files[0].mode=0o644;writeFileSync(path.join(dir,'LICENSE'),'changed');
  assert.throws(()=>verifyPackFile(report.tarballPath,report,dir),/Command failed/);
  writeFileSync(report.tarballPath,'tampered');assert.throws(()=>verifyPackFile(report.tarballPath,report,dir),/SHA256/);
});
test('same-run pack ZIP digest, producer seal and complete inventory are mandatory',t=>{
  const f=sealedPack(t),folder=path.join(f.root,'download'),zip=path.join(f.root,'artifact.zip');
  const {sha256,tarballPath,...pack}=f.report;
  const index={schema_version:1,git_sha:packSource.sha,version:expected.version,
    packages:[{name:expected.name,version:expected.version,file:expected.filename,sha256,pack}],producer:{...packSource,job:'npm-dry-run'}};
  const sums=`${sha256}  ${expected.filename}\n`;
  pythonFixture(`import json,sys,zipfile
with zipfile.ZipFile(sys.argv[1],'w') as z:
 z.write(sys.argv[2],sys.argv[3]); z.writestr('source-version.json',sys.argv[4]); z.writestr('SHA256SUMS',sys.argv[5])
`,[zip,tarballPath,expected.filename,JSON.stringify(index),sums]);
  const body=readFileSync(zip),evidence=packEvidence(body),manifest={cliVersion:expected.version,targets:[],npmScope:expected.name};
  const transport=(args)=>args.at(-1).endsWith('/zip')?body:JSON.stringify(args.includes('--slurp')?
    [args.at(-1).includes('/jobs?')?{jobs:evidence.jobs}:{artifacts:evidence.artifacts}]:evidence.run);
  assert.equal(downloadValidatedPacks(folder,packSource,transport)[0].sha256,sha256);
  assert.equal(readValidatedPacks(folder,packSource,manifest)[0].tarballPath,path.join(folder,expected.filename));
  assert.throws(()=>downloadValidatedPacks(folder,packSource,transport),/must be new/);
  for(const change of [i=>delete i.producer,i=>i.producer.run_attempt++,i=>i.producer.sha='b'.repeat(40),
    i=>i.producer.job='publish',i=>i.packages.push({...i.packages[0]}),i=>i.packages[0].pack.version='0.0.0']) {
    const bad=structuredClone(index);change(bad);writeFileSync(path.join(folder,'source-version.json'),JSON.stringify(bad));
    assert.throws(()=>readValidatedPacks(folder,packSource,manifest),/Validated pack/);
  }
  evidence.artifacts[0].digest=`sha256:${'0'.repeat(64)}`;
  assert.throws(()=>downloadValidatedPacks(path.join(f.root,'bad-digest'),packSource,transport),/ZIP differs/);
  assert(!existsSync(path.join(f.root,'bad-digest')));
  pythonFixture(`import sys,zipfile
with zipfile.ZipFile(sys.argv[1],'w') as z:
 z.writestr('../escape','x'); z.writestr('source-version.json','{}'); z.writestr('SHA256SUMS','')
`,[zip]);
  const unsafe=readFileSync(zip),badEvidence=packEvidence(unsafe);
  const badTransport=args=>args.at(-1).endsWith('/zip')?unsafe:JSON.stringify(args.includes('--slurp')?
    [args.at(-1).includes('/jobs?')?{jobs:badEvidence.jobs}:{artifacts:badEvidence.artifacts}]:badEvidence.run);
  assert.throws(()=>downloadValidatedPacks(path.join(f.root,'unsafe-zip'),packSource,badTransport),/Command failed/);
  assert(!existsSync(path.join(f.root,'unsafe-zip')),'unsafe ZIP names cannot choose an extraction directory');
});
test('pack metadata has independent compressed-ZIP and local-file budgets before parsing or extraction',t=>{
  const f=sealedPack(t),zip=path.join(f.root,'metadata.zip');
  const {sha256,tarballPath,...pack}=f.report;
  const index={schema_version:1,git_sha:packSource.sha,version:expected.version,producer:{...packSource,job:'npm-dry-run'},
    packages:[{name:expected.name,version:expected.version,file:expected.filename,sha256,pack}]};
  const manifest={cliVersion:expected.version,targets:[],npmScope:expected.name},sums=`${sha256}  ${expected.filename}\n`;
  for(const file of ['source-version.json','SHA256SUMS']) {
    const folder=path.join(f.root,file.replace('.','-'));
    const indexText=JSON.stringify(file==='source-version.json'?{...index,padding:'x'.repeat(2100*1024)}:index);
    const sumsText=file==='SHA256SUMS'?'x'.repeat(16*1024+1):sums;
    pythonFixture(`import json,sys,zipfile
data=json.load(sys.stdin)
with zipfile.ZipFile(sys.argv[1],'w',compression=zipfile.ZIP_DEFLATED) as z:
 z.write(sys.argv[2],sys.argv[3]); z.writestr('source-version.json',data['index']); z.writestr('SHA256SUMS',data['sums'])
`,[zip,tarballPath,expected.filename],JSON.stringify({index:indexText,sums:sumsText}));
    const body=readFileSync(zip),e=packEvidence(body);
    assert(body.length<10000,'compressed metadata bomb remains small on the wire');
    const transport=args=>args.at(-1).endsWith('/zip')?body:JSON.stringify(args.includes('--slurp')?
      [args.at(-1).includes('/jobs?')?{jobs:e.jobs}:{artifacts:e.artifacts}]:e.run);
    assert.throws(()=>downloadValidatedPacks(folder,packSource,transport),error=>/Oversized pack metadata/.test(String(error.stderr)));
    assert(!existsSync(folder),'metadata size must be refused before creating the extraction destination');
    mkdirSync(folder);writeFileSync(path.join(folder,'source-version.json'),indexText);writeFileSync(path.join(folder,'SHA256SUMS'),sumsText);
    writeFileSync(path.join(folder,expected.filename),f.body);
    assert.throws(()=>readValidatedPacks(folder,packSource,manifest),/ordinary bounded file/);
    writeFileSync(path.join(folder,'source-version.json'),JSON.stringify(index));writeFileSync(path.join(folder,'SHA256SUMS'),sums);
    const target=path.join(folder,file);rmSync(target);mkdirSync(target);
    assert.throws(()=>readValidatedPacks(folder,packSource,manifest),/ordinary bounded file/);
  }
});
