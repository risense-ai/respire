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
import { bash, contract, copy, inventory, job, manifest } from "./release-regression-fixtures.mjs";
test('frontend artifacts extract, verify and pack exact CLI bytes without a fabricated SDK inventory',()=>{
  const root=mkdtempSync(path.join(tmpdir(),'cli-frontend-pack-'));
  const workspace=fileURLToPath(new URL('../../',import.meta.url));
  const script=name=>fileURLToPath(new URL(name,import.meta.url));
  const sha=execFileSync('git',['rev-parse','HEAD'],{cwd:workspace,encoding:'utf8'}).trim();
  const gitDir=execFileSync('git',['rev-parse','--absolute-git-dir'],{cwd:workspace,encoding:'utf8'}).trim();
  const env={...process.env,GIT_DIR:gitDir};
  const run=(name,args=[],extra={})=>spawnSync(process.execPath,[script(name),...args],{cwd:root,env:{...env,...extra},encoding:'utf8',timeout:30000});
  try{
    assert.equal(frontendLayout(manifest),true);
    assert.throws(()=>frontendLayout({...manifest,artifactLayout:'unknown'}));
    for(const folder of ['assets','bins','cli','npm/bin'])mkdirSync(path.join(root,folder),{recursive:true});
    writeFileSync(path.join(root,'cli/Cargo.toml'),`version = "${manifest.cliVersion}"\n`);
    writeFileSync(path.join(root,'npm/artifact-manifest.json'),JSON.stringify(manifest));
    for(const file of ['LICENSE','COMMERCIAL-LICENSE.md','npm/README.md','npm/bin/cli.js'])writeFileSync(path.join(root,file),readFileSync(path.join(workspace,file)));
    for(const target of manifest.targets){
      const bytes=Buffer.alloc(128),arm=target.triple.startsWith('aarch64');
      if(target.triple.includes('windows')){
        bytes.write('MZ');bytes.writeUInt32LE(64,0x3c);bytes.write('PE\0\0',64);
        bytes.writeUInt16LE(arm?0xaa64:0x8664,68);bytes.writeUInt16LE(0x20b,88);
      }else if(target.triple.includes('darwin')){
        bytes.writeUInt32LE(0xfeedfacf,0);bytes.writeUInt32LE(arm?0x100000c:0x1000007,4);
      }else{
        bytes.set([0x7f,0x45,0x4c,0x46,2,1]);bytes.writeUInt16LE(arm?183:62,18);
      }
      const [binary,metadata]=releaseAssetNames(manifest,target.triple);
      assert.equal(releaseAssetNames(manifest,target.triple).length,2);
      writeFileSync(path.join(root,'assets',binary),bytes);
      writeFileSync(path.join(root,'assets',metadata),JSON.stringify({schema_version:2,artifact_layout:manifest.artifactLayout,
        git_sha:sha,version:manifest.cliVersion,target:target.triple,binary_file:binary,binary_size:bytes.length,
        binary_sha256:createHash('sha256').update(bytes).digest('hex'),producer:{run_id:'700',run_attempt:1}}));
      const result=run('extract-artifact.mjs',['assets',path.join('bins',target.triple,'release')],
        {CLI_TARGET:target.triple,CLI_SHA:sha,CLI_VERSION:manifest.cliVersion});
      assert.equal(result.status,0,result.stderr);
      const rejected=run('extract-artifact.mjs',['assets',path.join('candidate',target.triple)],
        {CLI_TARGET:target.triple,CLI_SHA:sha,CLI_VERSION:manifest.cliVersion,
          ARTIFACT_SOURCE:'build',VALIDATION_PROFILE:'full',CANDIDATE_ASSETS:JSON.stringify({schema:0})});
      assert.equal(rejected.status,1,rejected.stderr);
      assert.match(rejected.stdout,/Extracted verified/);
      assert.match(rejected.stderr,/AssertionError/);
      assert.doesNotMatch(rejected.stderr,/unsettled top-level await/);
    }
    const invalidEntry=run('candidate-artifact.mjs');
    assert.equal(invalidEntry.status,1,invalidEntry.stderr);
    assert.match(invalidEntry.stderr,/Expected resolve/);
    const verified=run('verify-packages.mjs',['assets','--assets-only']);assert.equal(verified.status,0,verified.stderr);
    const packed=run('pack.mjs',['--repo',root,'--root',path.join(root,'bins')]);assert.equal(packed.status,0,packed.stderr);
    for(const target of manifest.targets){
      const files=readdirSync(path.join(root,'npm/dist',target.pkg,'bin'));
      assert.deepEqual(files,[manifest.binaryName+(target.triple.includes('windows')?'.exe':'')]);
    }
    const runtime=path.join(root,'assets',`rsrs-${manifest.targets[0].triple}-runtime.tar.gz`);
    writeFileSync(runtime,'stale SDK archive');assert.notEqual(run('verify-packages.mjs',['assets','--assets-only']).status,0);
    rmSync(runtime);
    writeFileSync(path.join(root,'bins',manifest.targets[0].triple,'release/core-runtime.json'),'{}');
    assert.notEqual(run('pack.mjs',['--repo',root,'--root',path.join(root,'bins')]).status,0);
    const workflow=readFileSync(new URL('../../.github/workflows/release-validation.yml',import.meta.url),'utf8');
    const container=readFileSync(new URL('../../scripts/build-linux-musl-container.sh',import.meta.url),'utf8');
    assert.doesNotMatch(workflow,/sdk-ci-cache|fetch-core-sdk|stage-core-runtime|check-sdk-release/);
    assert.doesNotMatch(container,/RSRS_CORE_SDK_DIR|fetch-core-sdk|stage-core-runtime/);
    assert.match(workflow,/Verify real macOS CPU inference/);assert.match(workflow,/require-native-linux-arm64/);
  }finally{rmSync(root,{recursive:true,force:true});}
});
test('published Darwin ARM64 alias requires the exact 64-bit Mach-O CPU header',()=>{
  const alias='aarch64-apple-darwin',entry=manifest.targets.find(t=>t.triple===alias);
  assert.deepEqual(entry.os,['darwin']);assert.deepEqual(entry.cpu,['arm64']);
  const root=mkdtempSync(path.join(tmpdir(),'verify-binary-alias-')),file=path.join(root,'image');
  const macho=(magic,cpu)=>{const bytes=Buffer.alloc(64);bytes.writeUInt32LE(magic,0);bytes.writeUInt32LE(cpu,4);return bytes};
  const elf=Buffer.alloc(64);elf.set([0x7f,0x45,0x4c,0x46,2,1]);elf.writeUInt16LE(183,18);
  const pe=Buffer.alloc(128);pe.write('MZ');pe.writeUInt32LE(64,0x3c);pe.write('PE\0\0',64);pe.writeUInt16LE(0xaa64,68);pe.writeUInt16LE(0x20b,88);
  try{
    for(const target of [alias,['aarch64','apple','darwin'].join('-')]){
      writeFileSync(file,macho(0xfeedfacf,0x100000c));assert.doesNotThrow(()=>verifyBinary(file,target));
    }
    for(const [label,bytes,target] of [
      ['x64 CPU',macho(0xfeedfacf,0x1000007),alias],
      ['ARM32 CPU',macho(0xfeedfacf,12),alias],
      ['32-bit Mach-O',macho(0xfeedface,0x100000c),alias],
      ['fat Mach-O',macho(0xcafebabe,0x100000c),alias],
      ['wrong endian',macho(0xcffaedfe,0x100000c),alias],
      ['ELF',elf,alias],['PE',pe,alias],
      ['near alias',macho(0xfeedfacf,0x100000c),alias+'-other'],
      ['unknown target',macho(0xfeedfacf,0x100000c),'foreign-target'],
      ['x64 target',macho(0xfeedfacf,0x100000c),'x86_64-apple-darwin'],
    ]){
      writeFileSync(file,bytes);assert.throws(()=>verifyBinary(file,target),/Unsupported binary target|Binary header does not match/,label);
    }
  }finally{rmSync(root,{recursive:true,force:true})}
});
test('supported Linux ARM targets require native runners, every source feedback phase and background install proof',async()=>{
  const text=file=>readFileSync(new URL('../../.github/workflows/'+file,import.meta.url),'utf8');
  for(const file of ['release-validation.yml','release-native-validation.yml','release.yml']){
    const workflow=text(file).replace(/\r\n/g,'\n');
    if(file==='release-native-validation.yml'){
      const expression=workflow.match(/include: \$\{\{ fromJSON\((.*)\) \}\}/)?.[1];assert(expression);
      assert(expression.startsWith("inputs.recovery_matrix || "));
      const selectMatrix=shard_macos_upgrades=>JSON.parse(runInNewContext(expression,{
        inputs:{recovery_matrix:'',shard_macos_upgrades},
        format:(template,...values)=>template.replace(/\{(\d+)\}/g,(_,index)=>values[Number(index)]),
      }));
      const full=selectMatrix(false);
      assert.deepEqual(full.map(t=>t.target).sort(),manifest.targets.map(t=>t.triple).sort());
      assert.deepEqual(selectMatrix(true),full.filter(t=>t.target!=='aarch64-apple-darwin'));
      for(const {triple} of manifest.targets.filter(t=>t.triple.startsWith('aarch64-unknown-linux-')))assert.equal(full.find(t=>t.target===triple)?.os,'ubuntu-24.04-arm');
    }else for(const {triple} of manifest.targets.filter(t=>t.triple.startsWith('aarch64-unknown-linux-')))assert(workflow.includes('os: ubuntu-24.04-arm\n            target: '+triple),file+' missing native ARM target '+triple);
    assert(workflow.includes('scripts/require-native-linux-arm64.sh'),file+' lacks hardware check');
  }
  const source=text('token-free-sdk-check.yml');assert.match(source,/if: \$\{\{ !contains\(inputs.target, '-musl'\) \}\}/);
  assert.match(source,/run: bash scripts\/build-linux-musl.sh \$\{\{ inputs.target \}\}/);
  assert.match(source,/if: \$\{\{ inputs.feedback_probe && inputs.target != 'x86_64-apple-darwin' && contains\(inputs.target, '-musl'\) \}\}/);
  assert.match(source,/scripts\/verify-linux-arm64-elf.sh/);assert.match(source,/diary_tests/);assert.match(source,/feedback_contract_probe/);
  assert.match(source,/scripts\/probe-native-arm64-alpine.sh/);
  assert.match(text('release-native-validation.yml'),/scripts\/probe-native-arm64-alpine.sh/);
  const alpine=readFileSync(new URL('../../scripts/probe-native-arm64-alpine.sh',import.meta.url),'utf8');
  assert.match(alpine,/--network none/);assert.match(alpine,/--direct model install-m3 --json/);
  assert.doesNotMatch(alpine,/-e RSRS_CPU_OWNED_ORT_FILE=/);assert.equal((alpine.match(/sha256sum -c >\/dev\/null/g)||[]).length,5);
  const cache='/profile/models/bge-m3/onnx/model_quantized.cpu-v1.ort';
  const copied=alpine.indexOf('cp /cpu-model.ort '+cache),verified=alpine.indexOf('7ff152e912bc234f5c90e4012f99897a7b2ce180f016db2321304057756b64ff  '+cache);
  assert(copied>=0&&verified>copied&&verified<alpine.indexOf('--direct model install-m3'),'default CPU cache must be copied and verified before installation');
  assert.doesNotMatch(alpine,/--check|--status/);
  assert.doesNotMatch(alpine,/--json runtime start/);assert.match(alpine,/--json runtime stop/);assert.match(alpine,/trap .* EXIT/);
  assert.match(alpine,/p\["inference_completed"\] is True/);assert.match(alpine,/p\["fresh_session"\] is True/);
  assert(alpine.indexOf('test -r /cpu-model.ort')<alpine.indexOf('--direct model install-m3'),'mounted CPU artifact must be checked before inference');
  assert.match(alpine,/test -z "\$\{RSRS_CORE_TEST_MODE:-\}"/);assert.doesNotMatch(alpine,/-e (?:GH_TOKEN|GITHUB_TOKEN|CORE_SDK_READ_TOKEN|RSRS_CORE_TEST_MODE)/);
  const background=text('verify-publication.yml').split('  verify-publication-arm64:')[1];
  assert.match(background,/runs-on: ubuntu-24.04-arm/);assert.match(background,/VERIFY_ARM64: 'true'/);
  assert.match(background,/publication-arm64-verified-/);assert.doesNotMatch(background,/id-token:|contents: write|actions: write|npm publish|concurrency:/);
  const native=text('release-native-validation.yml').replace(/\r\n/g,'\n').replace(/^      - &\w+\n        /gm,'      - ');
  const step=name=>native.split('      - name: '+name+'\n')[1]?.split('\n      - ')[0];
  const install=step('Install and verify the official pinned standalone Runtime');
  assert.match(install,/id: pinned-runtime/);assert.match(install,/if: inputs.artifact_source != 'published-assets'/);
  const dataRoot=block=>block.match(/^          RSRS_DATA_DIR: (.+)$/m)?.[1];
  assert.equal(dataRoot(install),'${{ runner.temp }}/release-probe');
  assert.equal(dataRoot(install),dataRoot(step('Install and probe BGE CPU inference')));
  assert(native.indexOf('id: pinned-runtime')<native.indexOf('      - name: Install and probe BGE CPU inference'));
  const script=install.split("<<'NODE'\n")[1].split('\n          NODE')[0].replace(/^          /gm,'').replace(/^import .*;\n/gm,'')
    .replace("const {installPinnedRuntime} = await import('./scripts/ci-install-pinned-runtime.mjs');",'');
  for(const platform of ['linux','win32']) {
    const p=platform==='win32'?path.win32:path.posix, root=platform==='win32'?'C:\\runner\\native-runtime':'/runner/native-runtime';
    const image=Buffer.from('synthetic installed Runtime; never executed'), version='1.0.21-dev.5';
    const digest=createHash('sha256').update(image).digest('hex'), metadata=JSON.stringify({triple:'synthetic-target',runtime_version:version});
    const simulate=async change=>{
      const selected={version,sha256:digest,release:{metadata_json:metadata,metadata_sha256:createHash('sha256').update(metadata).digest('hex')}};
      const state={selected,status:0,sameImage:false,hosted:'true'};change?.(state);
      const tokenSentinel='fixture-only-value';
      const output=[],calls=[],env={GITHUB_ACTIONS:'true',RUNTIME_FIXTURE_HOSTED:state.hosted,CLI_TARGET:'synthetic-target',RUNNER_TEMP:p.dirname(root),RSRS_DATA_DIR:root,GITHUB_OUTPUT:'fixture-output',GH_TOKEN:tokenSentinel,GITHUB_TOKEN:tokenSentinel};
      await runInNewContext(`(async()=>{${script}\n})()`,{assert,createHash,join:p.join,resolve:(...parts)=>p.resolve(...parts),process:{platform,env},
        mkdirSync:()=>{},statSync:()=>({isFile:()=>true}),
        realpathSync:file=>state.sameImage?'same-image':file,
        readFileSync:file=>file==='test-support/runtime-test-fixture.json'?JSON.stringify({required:version}):file===p.join(root,'bin','runtime.json')?JSON.stringify(selected):image,
        appendFileSync:(_file,line)=>output.push(line),
        installPinnedRuntime:async(cli,options,target)=>{assert.equal(target,'synthetic-target');
          calls.push({cli,env:options.env});return {status:state.status}}});
      assert.equal(calls.length,1);assert.equal(calls[0].cli,p.resolve('target/ci','synthetic-target','release','rsrs'+(platform==='win32'?'.exe':'')));
      for(const token of ['GH_TOKEN','GITHUB_TOKEN']){assert.equal(calls[0].env[token],undefined);assert.equal(env[token],tokenSentinel);}
      assert.equal(calls[0].env.RSRS_DATA_DIR,root);assert.equal(calls[0].env.HOME,p.join(root,'home'));
      assert.equal(calls[0].env.RSRS_RUNTIME_TRIPLE,'synthetic-target');
      assert.deepEqual(output,[`path=${p.join(root,'bin','runtime',version,'rsrs-runtime'+(platform==='win32'?'.exe':''))}\n`,`sha256=${digest}\n`,`version=${version}\n`]);
    };
    await simulate();
    for(const change of [s=>s.status=1,s=>s.selected.version='1.0.21-dev.4',s=>s.selected.sha256='a'.repeat(64),
      s=>s.selected.release=null,s=>s.selected.release.metadata_sha256='b'.repeat(64),s=>s.sameImage=true,s=>s.hosted='false',
      s=>{s.selected.release.metadata_json=JSON.stringify({triple:'wrong-gnu-target',runtime_version:version});s.selected.release.metadata_sha256=createHash('sha256').update(s.selected.release.metadata_json).digest('hex');}])await assert.rejects(()=>simulate(change));
  }
  for(const name of ['Verify selected formal release and database DEV upgrades','Verify resident foreground and background inference under concurrent CLI load']) {
    const consumer=step(name);
    for(const [key,output,published] of [['BIN','path','RSRS_RUNTIME_BIN'],['SHA256','sha256','RSRS_PUBLISHED_RUNTIME_SHA256'],['VERSION','version','RSRS_PUBLISHED_RUNTIME_VERSION']])assert(consumer.includes(`NATIVE_RUNTIME_${key}: \u0024{{ inputs.artifact_source == 'published-assets' && env.${published} || steps.pinned-runtime.outputs.${output} }}`));
    assert.match(consumer,/test -n "\$NATIVE_RUNTIME_BIN" && test -n "\$NATIVE_RUNTIME_SHA256" && test -n "\$NATIVE_RUNTIME_VERSION"/);
    assert.match(consumer,/--runtime-binary "\$NATIVE_RUNTIME_BIN" --runtime-sha256 "\$NATIVE_RUNTIME_SHA256" --runtime-version "\$NATIVE_RUNTIME_VERSION"/);
  }
  assert.match(step('Verify selected formal release and database DEV upgrades'),/GH_TOKEN: \$\{\{ github.token \}\}/);
  assert.doesNotMatch(native,/RSRS_ARTIFACT_SOURCE: published-assets|RSRS_PUBLISHED_ASSETS_SHA256:|RSRS_PUBLISHED_BUNDLE_SHA256:/);
  const account=readFileSync(new URL('../../scripts/test-real-m3-account-switch.py',import.meta.url),'utf8');
  assert.match(account,/if key not in \('GH_TOKEN', 'GITHUB_TOKEN'\)/);
  assert.match(account,/web_help=subprocess\.check_output\(\[args.seed_binary,'web','--help'\],env=seed_env,/);
  const probe=step('Install and probe BGE CPU inference');
  assert.match(probe,/NATIVE_RUNTIME_BIN: \$\{\{ inputs.artifact_source == 'published-assets' && env.RSRS_RUNTIME_BIN \|\| steps.pinned-runtime.outputs.path \}\}/);
  assert.match(probe,/RSRS_RUNTIME_TRIPLE: \$\{\{ matrix.target \}\}/);
  assert.match(probe,/unset GH_TOKEN GITHUB_TOKEN/);
  assert.match(probe,/test -n "\$NATIVE_RUNTIME_BIN" && test -n "\$NATIVE_RUNTIME_SHA256" && test -n "\$NATIVE_RUNTIME_VERSION"/);
  assert.match(probe,/native_fixture_process.py --install-published "\$exe"/);
  assert.match(probe,/probe-native-arm64-alpine.sh[^\n]+"\$RSRS_RUNTIME_BIN" "\$NATIVE_RUNTIME_SHA256" "\$NATIVE_RUNTIME_VERSION"/);
  assert.match(alpine,/-v "\$bundle:\/runtime:ro"/);assert.match(alpine,/-v "\$receipt:\/runtime-selection\.json:ro"/);assert.doesNotMatch(alpine,/-e RSRS_RUNTIME_BIN=/);
  assert.match(alpine,/-e RSRS_RUNTIME_TRIPLE=aarch64-unknown-linux-musl/);
  assert.match(alpine,/\$NATIVE_RUNTIME_SHA256  \/runtime\/rsrs-runtime/);
  assert.match(alpine,/case "\$\(\/runtime\/rsrs-runtime --version\)"/);
});
test('new database full gate requires every native, cloud and API success',()=>{
  validateRegressionContract(contract,manifest);assert.equal(contract.schema,2);assert.equal(contract.scope,'database-in-place-upgrade');assert.equal(contract.native.length,manifest.targets.length);assert.equal(contract.cloud.length,manifest.targets.length-1);assert.equal(contract.migration.length,0);assert.equal(contract.migration_gate.length,0);
  for(const change of [c=>c.schema=1,c=>delete c.scope,c=>c.scope='native_profile_migration']){const old=copy(contract);change(old);assert.throws(()=>validateRegressionContract(old,manifest));}
  assert.equal(regressionStatus(inventory(),contract),'success');requireRegressions(inventory(),contract.native,contract);
  for(let index=0;index<inventory().length-1;index++){
    for(const conclusion of ['failure','cancelled','skipped',null]){
      const jobs=inventory();jobs[index].conclusion=conclusion;assert.throws(()=>regressionStatus(jobs,contract));assert.throws(()=>requireRegressions(jobs,contract.native,contract));
    }
    const absent=inventory();absent.splice(index,1);assert.equal(regressionStatus(absent,contract),'pending');assert.throws(()=>requireRegressions(absent,contract.native,contract));
    const pending=inventory();pending[index].status='in_progress';pending[index].conclusion=null;assert.equal(regressionStatus(pending,contract),'pending');
  }
});
test('duplicates, unknown targets and changed declared inventory cannot silently weaken the release/cleanup gate',()=>{
  for(const index of [0,4,7,9,11]){const jobs=inventory();jobs.push(copy(jobs[index]));assert.throws(()=>regressionStatus(jobs,contract));}
  const foreign=inventory();foreign[0].name='native-regression / Native (foreign-target)';assert.throws(()=>regressionStatus(foreign,contract));
  for(const key of ['native','cloud','migration','migration_gate']){const c=copy(contract);if(c[key].length)c[key].pop();else c[key].push('x86_64-pc-windows-msvc');assert.throws(()=>validateRegressionContract(c,manifest));}
});
test('workflow keeps profile import in manual diagnostics and Linux login and model dependencies mandatory',()=>{
  const workflow=readFileSync(new URL('../../.github/workflows/release.yml',import.meta.url),'utf8').replace(/\r\n/g,'\n');
  const part=name=>workflow.split(`\n  ${name}:\n`)[1]?.split(/\n  [a-z][\w-]*:\n/)[0];
  const base=part('cloud-sweep'),publish=part('publish'),ready=part('publication-ready');
  assert.equal(part('native-migration'),undefined);assert.equal(part('migration-gate'),undefined);
  assert.match(base,/needs: \[plan, validate, artifact-inputs, published-assets-verification\]/);
  const diagnostic=readFileSync(new URL('../../.github/workflows/migration-keyring-check.yml',import.meta.url),'utf8');
  assert.match(diagnostic,/workflow_dispatch:/);assert.match(diagnostic,/dev-profile-migration-smoke.py/);assert.match(diagnostic,/dev-secret-service-migration-smoke.py/);assert.doesNotMatch(diagnostic,/continue-on-error/);
  assert.match(base,/dev-login-interruption-smoke.py --native-credentials/);assert.doesNotMatch(base,/dev-secret-service-migration-smoke.py/);
  assert.doesNotMatch(base,/dev-profile-migration-smoke.py/);assert.match(base,/dev-smoke-supplements.py/);assert(base.indexOf('dev-smoke-supplements.py')<base.indexOf('dev-login-interruption-smoke.py'));
  assert.match(ready,/prepared-import.mjs readiness/);assert.doesNotMatch(ready,/native.length===4&&cloud.length===3/);assert.doesNotMatch(ready,/concurrency:|id-token:|contents: write/);assert.match(publish,/needs.publication-ready.result == 'success'/);assert.doesNotMatch(publish,/async function requireCi|async function requireRegression/);
});


test('full-validation clock includes required regressions that finish after fast-manual readiness',()=>{
  const jobs=inventory().map(j=>({...j,completed_at:'2026-10-27T00:03:00Z'}));
  jobs[0].completed_at='2026-10-27T00:08:00Z';
  jobs.find(j=>j.name==='publish').completed_at='2030-01-01T00:00:00Z';
  assert.equal(regressionCompletedAt(jobs,contract,'2026-10-27T00:05:00Z'),Date.parse('2026-10-27T00:08:00Z'));
  assert.equal(regressionCompletedAt(jobs,contract,'2026-10-27T00:09:00Z'),Date.parse('2026-10-27T00:09:00Z'));
});
test('missing, malformed, impossible or reversed required clocks never produce a full-validation duration',()=>{
  const jobs=inventory().map(j=>({...j,completed_at:'2026-10-27T00:03:00Z'}));
  for(const value of [undefined,null,'not-a-time','2026-02-31T00:00:00Z','2026-10-27T00:03:00']){
    const changed=copy(jobs);changed[0].completed_at=value;assert.equal(regressionCompletedAt(changed,contract,'2026-10-27T00:05:00Z'),null);
  }
  const reversed=copy(jobs);reversed[0].started_at='2026-10-27T00:04:00Z';assert.equal(regressionCompletedAt(reversed,contract,'2026-10-27T00:05:00Z'),null);
  assert.equal(regressionCompletedAt(jobs,contract,'not-a-time'),null);
});

// Plumbing simulation only: actual native inference remains a required hosted gate.
test('Alpine preflight mounts the selected CPU cache read-only and refuses missing/corrupt inputs before docker',()=>{
  const root=mkdtempSync(path.join(tmpdir(),'arm-alpine-paths-'));
  try{
    const bin=path.join(root,'bin'),models=path.join(root,'models with spaces'),data=path.join(root,'host profile'),capture=path.join(root,'docker.json');
    mkdirSync(bin);mkdirSync(path.join(models,'onnx'),{recursive:true});mkdirSync(path.join(data,'models/bge-m3/onnx'),{recursive:true});
    const exe=path.join(root,'rsrs'),cached=path.join(data,'models/bge-m3/onnx/model_quantized.cpu-v1.ort'),local=path.join(models,'onnx/model_quantized.cpu-v1.ort'),custom=path.join(root,'explicit cpu.ort');
    const installed=path.join(root,'complete musl runtime'),bundle=path.join(installed,'runtime','1.0.21-dev.5'),runtime=path.join(bundle,'rsrs-runtime'),receipt=path.join(installed,'runtime.json');mkdirSync(bundle,{recursive:true});
    writeFileSync(runtime,'fixture');writeFileSync(path.join(bundle,'core-runtime.json'),'fixture');writeFileSync(receipt,'fixture');
    for(const file of [exe,cached,custom,path.join(models,'tokenizer.json'),path.join(models,'onnx/model_quantized.onnx')])writeFileSync(file,'fixture');
    const stub=(name,body)=>writeFileSync(path.join(bin,name),body,{mode:0o755});
    stub('uname','#!/bin/sh\ncase "$1" in -s) echo Linux;; -m) echo aarch64;; esac\n');
    // The fake digest checker isolates selection/argv tests from the 570MB real model.
    stub('sha256sum','#!/bin/sh\ncat >/dev/null\nexit "${FAKE_HASH_STATUS:-0}"\n');
    const docker=path.join(root,'docker.cjs');
    writeFileSync(docker,`const fs=require('node:fs'),path=require('node:path'),args=process.argv.slice(2);
fs.writeFileSync(process.env.CAPTURE,JSON.stringify(args));
const mount=args.find(arg=>arg.endsWith(':/profile')).slice(0,-':/profile'.length);
fs.writeFileSync(path.join(mount,'model-probe-result.json'),JSON.stringify({status:'ok',details:{backend_load:{evidence:{ready:true,inference_completed:process.env.FAKE_INFERENCE!=='incomplete',fresh_session:true,dimensions:1024,selected:'CPU'}}}}));
`);
    stub('docker','#!/bin/sh\nMSYS2_ARG_CONV_EXCL="*" exec "'+process.execPath.replaceAll('\\','/')+'" "'+docker.replaceAll('\\','/')+'" "$@"\n');
    const script=fileURLToPath(new URL('../../scripts/probe-native-arm64-alpine.sh',import.meta.url));
    const shellPath=file=>file.split(path.sep).join('/');
    const env={...process.env,PATH:[bin,...(process.platform==='win32'?[path.dirname(bash)]:[]),process.env.PATH].join(path.delimiter),RUNNER_ARCH:'ARM64',RSRS_DATA_DIR:data,RSRS_CPU_OWNED_ORT_FILE:'',CAPTURE:capture};
    const run=extra=>{
      const result=spawnSync(bash,[script,'aarch64-unknown-linux-musl',exe,models,path.join(root,'container profile'),runtime,'a'.repeat(64),'1.0.21-dev.5'],{env:{...env,...extra},encoding:'utf8'});
      if(result.error)throw result.error;
      return result;
    };
    for(const [extra,selected] of [[{},cached],[{RSRS_CPU_OWNED_ORT_FILE:custom},custom]]){
      const result=run(extra);assert.equal(result.status,0,result.stderr);const args=JSON.parse(readFileSync(capture));
      assert(args.includes(shellPath(selected)+':/cpu-model.ort:ro'));assert(args.includes(shellPath(models)+':/models:ro'));assert(!args.some(arg=>arg.startsWith('RSRS_CPU_OWNED_ORT_FILE=')));assert.equal(args[args.indexOf('--network')+1],'none');
      const container=args.at(-1),cache='/profile/models/bge-m3/onnx/model_quantized.cpu-v1.ort';
      assert(container.includes('cp /cpu-model.ort '+cache));
      assert(container.includes('7ff152e912bc234f5c90e4012f99897a7b2ce180f016db2321304057756b64ff  '+cache));
      assert(args.includes(shellPath(bundle)+':/runtime:ro'));assert(args.includes(shellPath(receipt)+':/runtime-selection.json:ro'));assert(!args.some(arg=>arg.startsWith('RSRS_RUNTIME_BIN=')));
      assert(args.includes('RSRS_RUNTIME_TRIPLE=aarch64-unknown-linux-musl'));
      assert(args.includes('RSRS_M3_DIR=/profile/models/bge-m3'));
      assert(args.includes('NATIVE_RUNTIME_SHA256='+'a'.repeat(64)));assert(args.includes('NATIVE_RUNTIME_VERSION=1.0.21-dev.5'));
      rmSync(capture);
    }
    assert.notEqual(run({FAKE_INFERENCE:'incomplete'}).status,0,'incomplete inference cannot pass acceptance');rmSync(capture);
    rmSync(cached);writeFileSync(local,'fixture');assert.equal(run({}).status,0);assert(JSON.parse(readFileSync(capture)).includes(shellPath(local)+':/cpu-model.ort:ro'));rmSync(capture);
    assert.notEqual(run({FAKE_HASH_STATUS:'1'}).status,0);assert(!existsSync(capture));
    rmSync(local);assert.notEqual(run({}).status,0);assert(!existsSync(capture));
    // Explicit missing override must not silently fall back to another model.
    writeFileSync(local,'fixture');assert.notEqual(run({RSRS_CPU_OWNED_ORT_FILE:path.join(root,'missing.ort')}).status,0);assert(!existsSync(capture));
    rmSync(path.join(bundle,'core-runtime.json'));assert.notEqual(run({}).status,0);assert(!existsSync(capture));
  }finally{rmSync(root,{recursive:true,force:true});}
});

test('critical build-package identity and legacy full inventories cannot be interchanged or weakened by skipped gates',()=>{
  assert.equal(new Set(manifest.targets.map(t=>t.triple)).size,manifest.targets.length);
  assert.equal(manifest.targets.length,8,'Intel restoration requires eight CLI targets; Runtime/SDK inventories remain separate');
  assert(manifest.targets.some(t=>t.triple==='aarch64-unknown-linux-musl'&&t.pkg==='@rsrsai/linux-arm64'));
  assert(manifest.targets.some(t=>t.triple==='x86_64-apple-darwin'&&t.pkg==='@rsrsai/macos-x64'));
  const critical=regressionContract(manifest,'critical');
  assert.equal(validationProfile(), 'full');assert.throws(()=>validationProfile('unknown'));
  assert.equal(critical.binaries.length,manifest.targets.length);assert.equal(critical.package,true);assert.equal(critical.native.length,0);assert.equal(critical.cloud.length,0);assert.equal(critical.migration.length,0);assert.equal(critical.migration_gate.length,0);assert.equal(critical.api,false);
  validateRegressionContract(critical,manifest,'critical');validateRegressionContract(contract,manifest,'full');
  assert.throws(()=>validateRegressionContract(critical,manifest,'full'));assert.throws(()=>validateRegressionContract(contract,manifest,'critical'));
  const unlabelled=copy(critical);delete unlabelled.validation_profile;assert.throws(()=>validateRegressionContract(unlabelled,manifest));
  const jobs=[...critical.binaries.map(t=>job('validate / Release ('+t+')')),job('validate / npm-dry-run')];
  assert.equal(regressionStatus(jobs,critical),'success');assert.equal(regressionStatus(jobs,contract),'pending');
  for(const index of [0,jobs.length-1]){
    for(const conclusion of ['failure','cancelled','skipped']){const changed=copy(jobs);changed[index].conclusion=conclusion;assert.throws(()=>regressionStatus(changed,critical))}
    const absent=copy(jobs);absent.splice(index,1);assert.equal(regressionStatus(absent,critical),'pending');
  }
  assert.throws(()=>regressionStatus([...jobs,jobs[0]],critical));
  const foreign=copy(jobs);foreign[0].name='native-regression / Native (foreign-target)';assert.throws(()=>regressionStatus(foreign,critical));
});

test('only explicit inactive full matrices are ignored by critical and never enter its completion clock',()=>{
  const critical=regressionContract(manifest,'critical');
  const jobs=[...critical.binaries.map(t=>({...job('validate / Release ('+t+')'),started_at:'2026-11-01T00:00:00Z',completed_at:'2026-11-01T00:01:00Z'})),{...job('validate / npm-dry-run'),started_at:'2026-11-01T00:00:00Z',completed_at:'2026-11-01T00:01:00Z'}];
  const skipped=name=>({name,status:'completed',conclusion:'skipped'});
  const inactive=[skipped('Native migration (${{ matrix.target }})'),skipped('Migration gate (${{ matrix.target }})')];
  // Exact names from GitHub API run 41291097685 attempt1, read during this change.
  const caller=skipped('native-regression'),cloud=skipped('cloud-sweep');
  const all=[...jobs,...inactive,caller,cloud];
  assert.throws(()=>regressionStatus([...jobs,skipped('native-regression / Native (${{ matrix.target }})')],critical));
  assert.equal(regressionStatus(all,critical),'success');assert.equal(regressionCompletedAt(all,critical,'2026-11-01T00:00:30Z'),Date.parse('2026-11-01T00:01:00Z'));
  assert.throws(()=>regressionStatus([...all,inactive[0]],critical));
  for(const state of [{status:'completed',conclusion:'failure'},{status:'in_progress',conclusion:null},{status:'completed',conclusion:null}])assert.throws(()=>regressionStatus([...jobs,{...inactive[0],...state}],critical));
  assert.throws(()=>regressionStatus([...jobs,skipped('Native migration (foreign-target)')],critical));
  const individual=skipped('Native migration (aarch64-apple-darwin)');assert.equal(regressionStatus([...jobs,individual],critical),'success');
  assert.throws(()=>regressionStatus([...jobs,individual,individual],critical));
  assert.throws(()=>regressionStatus([...jobs,{...individual,conclusion:'failure'}],critical));
  assert.throws(()=>regressionStatus([...inventory(),...inactive],contract));
});

test('critical cannot publish; full candidate preparation precedes promotion and public acceptance',()=>{
  const text=file=>readFileSync(new URL('../../.github/workflows/'+file,import.meta.url),'utf8').replace(/\r\n/g,'\n');
  const release=text('release.yml'),part=name=>release.split('\n  '+name+':\n')[1]?.split(/\n  [a-z][\w-]*:\n/)[0];
  const guard=release.split('\n').find(line=>line.includes("if(['latest','stable'].includes(channel))"));
  assert(guard);
  for(const channel of ['latest','stable'])assert.throws(()=>runInNewContext(guard,{channel}),/complete G1/);
  runInNewContext(guard,{channel:'dev'});
  assert.match(release,/validation_profile:[\s\S]*?default: full/);
  assert.match(release,/Require full candidates before publication/);
  const client2Guard=release.split('\n').find(line=>line.includes('Client2 diagnostics require'));
  assert.ok(client2Guard);
  for(const e of [{CLIENT2_RELEASE:'v2.0.0-dev.6'},{CLIENT2_COMMIT:'c'.repeat(40)},{CLIENT2_RELEASE:'v2.0.0-dev.6',CLIENT2_COMMIT:'main'}])assert.throws(()=>runInNewContext(client2Guard,{e}),/require an explicit/);
  runInNewContext(client2Guard,{e:{}});
  runInNewContext(client2Guard,{e:{CLIENT2_RELEASE:'v2.0.0-dev.6',CLIENT2_COMMIT:'c'.repeat(40)}});
  assert.doesNotMatch(release,/^  native-regression-critical(?:-readonly)?:/m);
  assert.match(part('publication-ready'),/prepared-import.mjs readiness/);
  assert.match(part('preparation-receipt'),/Require all full consumers including GUI and historical DB/);
  const daily=text('release-full-regression.yml');
  assert.doesNotMatch(daily,/schedule:/);
  assert.match(daily,/-f publish=false -f validation_profile=full/);
  assert.match(daily,/-f artifact_source=published-assets/);
  assert.match(daily,/-f client2_release="\$CLIENT2_RELEASE" -f client2_commit="\$CLIENT2_COMMIT"/);
  assert.doesNotMatch(daily,/client2_candidate_run_id/);
  assert.doesNotMatch(release,/43721147038|6ffa187685734873721ab51f3a54d8b2c29cd269/);
  assert.match(text('release-validation.yml'),/full_validation:\n        default: false/);
  const selected={client2_release:'v2.0.0-dev.6',client2_commit:'c'.repeat(40)};
  for(const call of ['validate','native-regression'].map(part))assert.match(call,/client2_candidate_run_id: ''/);
  for(const call of ['validate','native-regression'].map(part))for(const key of Object.keys(selected)){
    const expression=call.match(new RegExp('^      '+key+': \\$\\{\\{ (.+) \\}\\}$','m'))?.[1];
    assert(expression);
    const actual=runInNewContext(expression,{inputs:selected});
    assert.equal(actual,selected[key]);
  }
});

test('all release business consumers require published assets and no critical native caller remains',()=>{
  const text=file=>readFileSync(new URL('../../.github/workflows/'+file,import.meta.url),'utf8').replace(/\r\n/g,'\n');
  const release=text('release.yml'),part=name=>release.split('\n  '+name+':\n')[1]?.split(/\n  [a-z][\w-]*:\n/)[0];
  assert.equal(part('native-regression-critical'),undefined);
  assert.equal(part('native-regression-critical-readonly'),undefined);
  assert.match(part('native-regression'),/cache-mode: read/);
  assert.match(text('release-validation.yml'),/^cache-mode: read$/m);
  for(const name of ['native-regression','cloud-sweep','api-smoke']){
    const condition=part(name).match(/    if: \$\{\{ (.+) \}\}/)[1].replace(/needs\.([a-z][a-z0-9-]*)/g,(_,id)=>`needs[${JSON.stringify(id)}]`);
    const needs=new Proxy({},{get:()=>({result:'success',outputs:{validation_profile:'critical'}})});
    for(const artifact_source of ['build','published-assets'])assert.equal(Boolean(runInNewContext(condition,{always:()=>true,needs,inputs:{artifact_source}})),artifact_source==='published-assets',name);
  }
});

test('the actual critical target waiter binds unique successful producer, source, attempt and artifact before consumption',async()=>{
  const native=readFileSync(new URL('../../.github/workflows/release-native-validation.yml',import.meta.url),'utf8').replace(/\r\n/g,'\n');
  const program=native.split("          node - <<'NODE'\n")[1].split('\n          NODE')[0];
  async function run(mutate=()=>{}){
    const sha='a'.repeat(40),target='aarch64-apple-darwin';
    const state={run:{id:700,run_attempt:1,head_sha:sha,status:'in_progress'},jobs:{total_count:2,jobs:[{id:11,run_id:700,run_attempt:1,head_sha:sha,name:'validate / Release ('+target+')',status:'completed',conclusion:'success'},{name:'validate / Release (another-target)',status:'queued',conclusion:null}]},artifacts:{total_count:1,artifacts:[{id:9,name:'cli-release-'+target,expired:false,size_in_bytes:10,digest:'sha256:'+'b'.repeat(64),workflow_run:{id:700,head_sha:sha}}]}};
    mutate(state);const outputs=[],exits=[],errors=[];let artifactReads=0,queuedProducerReads=0,delays=0,now=0;
    await runInNewContext(program,{require:name=>name==='child_process'?{execFileSync:(_cmd,args)=>{const url=args[1];if(state.run.status==='queued'&&(url.endsWith('/artifacts?per_page=100')||url.endsWith('/jobs?per_page=100')))queuedProducerReads++;if(url.endsWith('/artifacts?per_page=100')){artifactReads++;return JSON.stringify(state.artifacts)}if(url.endsWith('/jobs?per_page=100'))return JSON.stringify(state.jobs);return JSON.stringify(state.run)}}:{appendFileSync:(_path,value)=>outputs.push(value)},process:{env:{GITHUB_REPOSITORY:'risense-ai/respire-cli',GITHUB_RUN_ID:'700',GITHUB_RUN_ATTEMPT:'1',GITHUB_SHA:sha,CLI_TARGET:target,PRODUCER_PREFIX:'validate / ',GITHUB_OUTPUT:'fixture-output'},exit:value=>exits.push(value)},console:{log:()=>{},error:value=>errors.push(value)},setTimeout:resolve=>{delays++;if(state.queued==='resume'){state.run.status='in_progress';resolve();return;}if(state.queued==='expire'){now=45*60*1000+1;resolve();return;}throw Error('still waiting; no target consumption')},Date:{now:()=>now}});
    return {outputs,exits,errors,artifactReads,queuedProducerReads,delays};
  }
  const good=await run();assert.deepEqual(good.outputs,['artifact_id=9\n']);assert.deepEqual(good.exits,[]);
  const mutations=[s=>s.jobs.jobs[0].id=0,s=>s.jobs.jobs[0].run_id=701,s=>s.jobs.jobs[0].run_attempt=2,s=>s.jobs.jobs[0].head_sha='c'.repeat(40),s=>s.run.run_attempt=2,s=>s.run.head_sha='c'.repeat(40),s=>s.run.status='completed',s=>s.jobs.total_count=3,s=>{s.jobs.jobs.push(s.jobs.jobs[0]);s.jobs.total_count++},s=>s.jobs.jobs[0].conclusion='failure',s=>s.jobs.jobs[0].conclusion='cancelled',s=>s.jobs.jobs[0].conclusion='skipped',s=>s.jobs.jobs[0].status='queued',s=>{s.artifacts.artifacts.push(s.artifacts.artifacts[0]);s.artifacts.total_count++},s=>s.artifacts.artifacts[0].expired=true,s=>s.artifacts.artifacts[0].workflow_run.id=701,s=>s.artifacts.artifacts[0].workflow_run.head_sha='c'.repeat(40),s=>s.artifacts.artifacts[0].digest='wrong',s=>s.artifacts.artifacts[0].id=0];
  for(const mutate of mutations){const bad=await run(mutate);assert.deepEqual(bad.outputs,[]);assert.deepEqual(bad.exits,[1])}
  const failed=await run(s=>s.jobs.jobs[0].conclusion='failure');assert.equal(failed.artifactReads,0,'an uploaded artifact never bypasses a failed producer');
  const queued=await run(s=>{s.run.status='queued';s.queued='resume'});
  assert.deepEqual(queued.outputs,['artifact_id=9\n']);assert.deepEqual(queued.exits,[]);assert.equal(queued.delays,1);assert.equal(queued.queuedProducerReads,0);
  const expired=await run(s=>{s.run.status='queued';s.queued='expire'});
  assert.deepEqual(expired.outputs,[]);assert.deepEqual(expired.exits,[1]);assert.equal(expired.artifactReads,0);assert.equal(expired.queuedProducerReads,0);assert.match(expired.errors[0],/unavailable within 45 minutes/);
  for(const status of ['completed','waiting','pending','unknown',undefined]){const bad=await run(s=>s.run.status=status);assert.deepEqual(bad.exits,[1]);assert.equal(bad.artifactReads,0);assert.match(bad.errors[0],/run status refused/)}
  for(const [field,value,label] of [['id',701,'run ID mismatch'],['run_attempt',2,'run attempt mismatch'],['head_sha','c'.repeat(40),'workflow SHA mismatch']]){
    const bad=await run(s=>{s.run[field]=value;s.run.status='queued';s.queued='resume'});assert.deepEqual(bad.exits,[1]);assert.equal(bad.delays,0);assert.match(bad.errors[0],new RegExp(label));
  }
});

import "./release-published-consumer-tests.mjs";
