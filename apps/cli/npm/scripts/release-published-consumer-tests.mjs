import {readSplitPowerShellFixture} from './split-powershell-fixture-source.mjs';
import { publishedAssetNames, parsePublishedAssets, validateArtifactRequest } from "./extract-artifact.mjs";
import { createHash } from "node:crypto";
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { runInNewContext } from "node:vm";
import path from "node:path";
import { tmpdir } from "node:os";
import { verifyBinary } from "./verify-binary.mjs";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { copy, manifest } from "./release-regression-fixtures.mjs";

function publishedFixture() {
  const part = kind => ({repository: `risense-ai/respire-${kind}`, release_id: kind === 'cli' ? 41 : 42,
    version: '1.0.21-dev.5', source_sha: (kind === 'cli' ? 'a' : 'b').repeat(40), producer:{run_id:91,run_attempt:2},
    assets: manifest.targets.map(t=>t.triple).flatMap((target, index) => publishedAssetNames(kind, target).map((name, n) =>
      ({id: index * 3 + n + 1, name, size: 12, sha256: 'c'.repeat(64)})))});
  const spec = {schema: 2, cli: part('cli'), runtime: part('runtime')};
  const text = JSON.stringify(spec), hash = createHash('sha256').update(text).digest('hex');
  return {spec, env: {ARTIFACT_SOURCE: 'published-assets', PUBLISHED_ASSETS: text, PUBLISHED_ASSETS_SHA256: hash,
    GITHUB_EVENT_NAME: 'workflow_dispatch', GITHUB_REPOSITORY: 'risense-ai/respire-cli', REQUEST_PUBLISH: 'false',
    VALIDATION_PROFILE: 'full', GITHUB_SHA: 'e'.repeat(40), RELEASE_REF: 'e'.repeat(40)}};
}
test('published assets require explicit full, immutable manifest and no publication/preparation/recovery', () => {
  const {spec, env} = publishedFixture(), targets = manifest.targets.map(t => t.triple);
  assert.deepEqual(validateArtifactRequest(env, targets), spec);
  assert.equal(validateArtifactRequest({}, targets), null);
  assert.equal(validateArtifactRequest({ARTIFACT_SOURCE: 'build'}, targets), null);
  for (const [key, value] of [['ARTIFACT_SOURCE', 'unknown'], ['GITHUB_EVENT_NAME', 'push'],
    ['GITHUB_REPOSITORY', 'fork/respire-cli'], ['REQUEST_PUBLISH', 'true'], ['REQUEST_PUBLISH', ''],
    ['VALIDATION_PROFILE', 'critical'], ['DEV_VERSION', spec.cli.version], ['PREPARED_RUN', '123'],
    ['REUSE_DEV12', 'true'], ['REPAIR_DEV12', 'true'], ['REPAIR_DEV14', 'true'], ['RESUME_DEV6', 'true'],
    ['RELEASE_REF', 'main'], ['RELEASE_REF', spec.cli.source_sha], ['GITHUB_SHA', 'main'], ['GITHUB_SHA', 'c'.repeat(40)], ['PUBLISHED_ASSETS_SHA256', 'd'.repeat(64)],
    ['PUBLISHED_ASSETS', env.PUBLISHED_ASSETS + '\n']]) {
    assert.throws(() => validateArtifactRequest({...env, [key]: value}, targets), key);
  }
  assert.throws(() => validateArtifactRequest({...env, ARTIFACT_SOURCE: 'build'}, targets));
  for (const change of [s => s.schema = 1, s => s.cli.producer.run_id = 0, s => delete s.runtime.producer, s => s.cli.repository = 'fork/respire-cli',
    s => s.cli.source_sha = 'main', s => s.runtime.release_id = 0, s => s.cli.version = 'dev',
    s => s.cli.assets.pop(), s => s.runtime.assets.push(s.runtime.assets[0]),
    s => s.cli.assets[1].id = s.cli.assets[0].id, s => s.cli.assets[1].name = s.cli.assets[0].name,
    s => s.runtime.assets[0].sha256 = 'bad', s => s.runtime.assets[0].size = 0,
    s => s.cli.assets[0].name = '../escape', s => s.cli.assets[0].extra = 'unbound',
    s => s.cli.assets[0].size = 2 ** 20 + 1, s => s.runtime.assets[0].size = 2 ** 31 + 1]) {
    const bad = copy(spec); change(bad); const text = JSON.stringify(bad);
    assert.throws(() => parsePublishedAssets(text, createHash('sha256').update(text).digest('hex'), targets));
  }
  assert.throws(() => parsePublishedAssets(env.PUBLISHED_ASSETS, env.PUBLISHED_ASSETS_SHA256, targets.slice(1)));
  assert.throws(() => parsePublishedAssets(env.PUBLISHED_ASSETS, env.PUBLISHED_ASSETS_SHA256, targets.map(() => targets[0])));
});
test('published mode cannot enter any build/prepared/publication/recovery job or claim full after download', () => {
  const workflow = readFileSync(new URL('../../.github/workflows/release.yml', import.meta.url), 'utf8').replace(/\r\n/g, '\n');
  const jobs = Object.fromEntries([...workflow.slice(workflow.indexOf('jobs:\n') + 6).matchAll(/^  ([a-z][a-z0-9-]+):\n([\s\S]*?)(?=^  [a-z][a-z0-9-]+:|$(?![\s\S]))/gm)].map(m => [m[1], m[2]]));
  for (const [name, body] of Object.entries(jobs)) {
    if (['artifact-inputs', 'published-assets-verification', 'published-full-complete', 'validate', 'native-regression', 'cloud-sweep', 'api-smoke'].includes(name)) continue;
    const condition = body.match(/^    if: \$\{\{ (.*?) \}\}$/m)?.[1];
    assert.ok(condition, name);
    const suffix = " && (inputs.artifact_source || 'build') == 'build'";
    assert.ok(condition.endsWith(suffix), name + ' must be build-only');
    const original = condition.slice(0, -suffix.length);
    for (const artifact_source of [undefined, 'build', 'published-assets']) {
      const context = {inputs: {artifact_source, publish: true, validation_profile: 'full'},
        github: {event_name: 'workflow_dispatch', repository: 'risense-ai/respire-cli', ref: 'refs/heads/main', sha: 's'},
        always: () => true, cancelled: () => false,
        needs: new Proxy({}, {get: () => ({result: 'success', outputs: {publish: 'true', validation_profile: 'full', sha: 's'}})})};
      const js = value => value.replace(/needs\.([a-z][a-z0-9-]*)/g, (_, name) => `needs[${JSON.stringify(name)}]`);
      const actual = runInNewContext(js(condition), context);
      if (artifact_source === 'published-assets') assert.equal(Boolean(actual), false, name);
      else assert.equal(Boolean(actual), Boolean(runInNewContext(js(original), context)), name + ' default build behavior changed');
    }
  }
  const verification = jobs['published-assets-verification'];
  const actualNeeds = [...verification.matchAll(/^    needs: (.+)$/gm)].map(m => m[1]);
  assert.deepEqual(actualNeeds, ['artifact-inputs'], 'verification must depend only on the validated input job');
  const conditions = [...verification.matchAll(/^    if: \$\{\{ (.*?) \}\}$/gm)].map(m => m[1]);
  assert.equal(conditions.length, 1);
  assert.equal(conditions[0], "always() && needs.artifact-inputs.result == 'success' && inputs.artifact_source == 'published-assets'");
  const expression = conditions[0].replace(/needs\.([a-z][a-z0-9-]*)/g, (_, name) => `needs[${JSON.stringify(name)}]`);
  for (const result of ['success', 'failure', 'cancelled', 'skipped', undefined]) {
    for (const artifact_source of ['published-assets', 'build', undefined, 'unknown']) {
      const needs = new Proxy({'artifact-inputs': {result}}, {get(target, key) {
        assert.equal(key, 'artifact-inputs', 'verification referenced an undeclared dependency');
        return target[key];
      }});
      assert.equal(Boolean(runInNewContext(expression, {always: () => true, needs, inputs: {artifact_source}})),
        result === 'success' && artifact_source === 'published-assets');
    }
  }
  for (const name of ['artifact-inputs', 'published-assets-verification']) {
    const checkouts = [...jobs[name].matchAll(/      - uses: actions\/checkout@v5\n        with:\n([\s\S]*?)(?=      - |$)/g)];
    assert.equal(checkouts.length, 1);
    assert.match(checkouts[0][1], /^          ref: \$\{\{ github.sha \}\}$/m);
    assert.doesNotMatch(checkouts[0][1], /inputs\.ref|source_sha/);
  }
  assert.match(jobs.plan, /needs: artifact-inputs/);
  assert.match(jobs['published-assets-verification'], /contents: read/);
  assert.doesNotMatch(jobs['published-assets-verification'], /actions\/cache|cargo |set-build-version|next-dev-version|download-artifact/);
  assert.match(jobs['published-full-complete'], /--complete-published full-reports/);
  assert.match(jobs['published-full-complete'], /actual process image digests/);
  assert.match(jobs['published-full-complete'], /Object.values\(n\).some\(j=>j.result!=='success'\)/);
  for(const name of ['validate','native-regression','cloud-sweep','api-smoke']) {
    const body=jobs[name], condition=body.match(/^    if: \$\{\{ (.*?) \}\}$/m)[1];
    const expr=condition.replace(/needs\.([a-z][a-z0-9-]*)/g,(_,id)=>`needs[${JSON.stringify(id)}]`);
    for(const result of ['success','failure','cancelled','skipped']) {
      const needs=new Proxy({'artifact-inputs':{result},'published-assets-verification':{result:'success'},plan:{result:'skipped',outputs:{}}},{get:(t,k)=>t[k]??{result:'success',outputs:{}}});
      assert.equal(Boolean(runInNewContext(expr,{always:()=>true,needs,inputs:{artifact_source:'published-assets'}})),result==='success',name);
    }
    assert.match(body,/needs: \[.*artifact-inputs.*published-assets-verification/);
    assert.match(jobs['published-full-complete'],new RegExp('needs: \\[.*'+name));
  }
});

test('published macOS ARM alias preserves the exact Mach-O 64-bit ARM64 header requirement',()=>{
  const root=mkdtempSync(path.join(tmpdir(),'published-macho-header-'));
  const image=path.join(root,'image'), alias='aarch64-apple-darwin';
  const macho=(magic=0xfeedfacf,cpu=0x100000c)=>{const b=Buffer.alloc(64);b.writeUInt32LE(magic,0);b.writeUInt32LE(cpu,4);return b;};
  try {
    writeFileSync(image,macho());
    for(const target of [alias,['aarch64','apple','darwin'].join('-')])assert.doesNotThrow(()=>verifyBinary(image,target));
    for(const bytes of [macho(0xfeedfacf,0x1000007),macho(0xfeedfacf,12),macho(0xfeedface,12),macho(0xbebafeca,0x100000c),macho(0xcafebabe,0x100000c),macho(0xcffaedfe,0xc000001),Buffer.from('\x7fELF'.padEnd(64,'\0')),Buffer.from('MZ'.padEnd(64,'\0'))]) {
      writeFileSync(image,bytes);assert.throws(()=>verifyBinary(image,alias));
    }
    writeFileSync(image,macho());
    for(const target of [alias+'x','x'+alias,'wsedw32-uubqy-qvasSc'])assert.throws(()=>verifyBinary(image,target));
  } finally {rmSync(root,{recursive:true,force:true});}
});

test('published validation exports product identity without overwriting the harness checkout SHA',()=>{
  const root=mkdtempSync(path.join(tmpdir(),'published-output-contract-'));
  try {
    const {spec,env}=publishedFixture(),output=path.join(root,'output');writeFileSync(output,'sha='+env.GITHUB_SHA+'\n');
    const result=spawnSync(process.execPath,['npm/scripts/extract-artifact.mjs','--validate-published'],{cwd:fileURLToPath(new URL('../../',import.meta.url)),env:{...process.env,...env,GITHUB_OUTPUT:output},encoding:'utf8'});
    assert.equal(result.status,0,result.stderr);
    const lines=readFileSync(output,'utf8').trim().split('\n');
    assert.deepEqual(lines,['sha='+env.GITHUB_SHA,'product_sha='+spec.cli.source_sha,'version='+spec.cli.version]);
    const parent=readFileSync(new URL('../../.github/workflows/release.yml',import.meta.url),'utf8');
    assert.match(parent,/sha: \$\{\{ steps.validated.outputs.product_sha \}\}/);
    const reusable=readFileSync(new URL('../../.github/workflows/release-validation.yml',import.meta.url),'utf8');
    const published=reusable.split('if [ "$ARTIFACT_SOURCE" = published-assets ]; then')[1].split('fi')[0];
    assert.match(published,/--validate-published/);assert.doesNotMatch(published,/BUILD_VERSION|set-build-version|next-dev-version/);
  } finally {rmSync(root,{recursive:true,force:true});}
});

test('published consumers declare every condition dependency and cannot execute compile/cache or original staging steps',()=>{
  const root=new URL('../../.github/workflows/',import.meta.url),text=name=>readFileSync(new URL(name,root),'utf8').replace(/\r\n/g,'\n');
  const workflow=text('release.yml'),jobs=Object.fromEntries([...workflow.slice(workflow.indexOf('jobs:\n')+6).matchAll(/^  ([a-z][a-z0-9-]+):\n([\s\S]*?)(?=^  [a-z][a-z0-9-]+:|$(?![\s\S]))/gm)].map(m=>[m[1],m[2]]));
  const js=expr=>expr.replace(/needs\.([a-z][a-z0-9-]*)/g,(_,name)=>`needs[${JSON.stringify(name)}]`);
  for(const name of ['validate','native-regression','cloud-sweep','api-smoke']) {
    const body=jobs[name],dependencies=body.match(/^    needs: \[(.+)\]$/m)[1].split(',').map(v=>v.trim());
    const condition=body.match(/^    if: \$\{\{ (.+) \}\}$/m)[1];
    const make=(mode,failed)=>new Proxy(Object.fromEntries(dependencies.map(id=>[id,{result:id===failed?'failure':id==='plan'&&mode==='published-assets'?'skipped':id==='published-assets-verification'&&mode==='build'?'skipped':'success',outputs:{validation_profile:'full'}}])),{get(t,key){assert(dependencies.includes(key),'undeclared '+name+' dependency '+key);return t[key];}});
    for(const mode of ['build','published-assets']) {
      const context={always:()=>true,needs:make(mode),inputs:{artifact_source:mode,reuse_dev12_validation:false,prepared_run:''}};
      assert.equal(Boolean(runInNewContext(js(condition),context)),true,name+' '+mode);
      const required=mode==='published-assets'?['artifact-inputs','published-assets-verification',...(['native-regression','cloud-sweep'].includes(name)?['validate']:[])]:dependencies.filter(id=>!['artifact-inputs','published-assets-verification'].includes(id));
      for(const failed of required)assert.equal(Boolean(runInNewContext(js(condition),{...context,needs:make(mode,failed)})),false,name+' accepted failed '+failed);
    }
  }
  const validation=text('release-validation.yml'),binaries=validation.split('\n  binaries:\n')[1].split('\n  native-regression:')[0];
  for(const step of binaries.split(/(?=^      - )/m)) {
    if(!/dtolnay\/rust-toolchain|rust-ci-cache|build-linux-musl|fetch-core-sdk|stage-core-runtime|stage-artifacts|set-build-version/.test(step))continue;
    const condition=step.match(/^        if: (.+)$/m)?.[1];assert(condition,'build step lacks a mode guard');
    const expr=condition.replace(/^\$\{\{ /,'').replace(/ \}\}$/,'');
    assert.equal(Boolean(runInNewContext(expr,{inputs:{artifact_source:'published-assets'},contains:()=>false})),false);
  }
  assert.match(validation,/--pack-published published-raw/);
  assert.match(validation,/pattern: published-assets-verification-\*/);
  const native=text('release-native-validation.yml');
  const nativeDefault=native.split('\njobs:\n')[0];
  assert.doesNotMatch(nativeDefault,/GH_TOKEN:|GITHUB_TOKEN:|contents: write/);
  assert.match(nativeDefault,/permissions:\n  contents: read/);
  const nativeJobs=Object.fromEntries([...native.split('\njobs:\n')[1].matchAll(/^  ([a-z][a-z0-9-]+):\n([\s\S]*?)(?=^  [a-z][a-z0-9-]+:|$(?![\s\S]))/gm)].map(m=>[m[1],m[2]]));
  assert.deepEqual(Object.keys(nativeJobs).filter(name=>/contents: write/.test(nativeJobs[name])),[]);
  assert.match(nativeJobs.native,/permissions:\n      contents: read\n      actions: read/);
  assert.doesNotMatch(nativeJobs.native,/contents: write|actions: write|id-token:/);
  assert.doesNotMatch(native,/historical-draft-download|draft-baseline-cache/);
  assert.doesNotMatch(nativeJobs.native,/^    needs:/m);
  assert(nativeJobs.native.includes('--profile "$UPGRADE_PROFILE"'));
  assert(nativeJobs.native.includes('UPGRADE_PROFILE: ${{ inputs.validation_profile }}'));
  const upgrade=readFileSync(new URL('../../scripts/test-published-upgrades.py',import.meta.url),'utf8').replace(/\r\n/g,'\n');
  assert.match(upgrade,/if args.download_only:\n        assert tag=='v1\.0\.10' and release\['draft'\][\s\S]*?\n        return\n    with tarfile\.open/);
  assert.match(upgrade,/if args.download_only:\n    assert args.profile=='full' and not args.draft_baseline_cache[\s\S]*?\n    sys.exit\(0\)\n\ntry:/);
  assert.match(upgrade,/if release\['draft'\] and args.draft_baseline_cache:\n        assert not needed/);
  const extract=native.split('      - name: Verify and extract the release binary\n')[1].split('\n      - ')[0];
  assert.match(extract,/GH_TOKEN: \$\{\{ github.token \}\}/);
  const account=readFileSync(new URL('../../scripts/test-real-m3-account-switch.py',import.meta.url),'utf8');
  assert.match(account,/env = \{key: value for key, value in os.environ.items\(\)\s+if key not in \('GH_TOKEN', 'GITHUB_TOKEN'\)\}/);
  const seed=readFileSync(new URL('../../scripts/m3_account_lifecycle.py',import.meta.url),'utf8');
  assert.match(seed,/seed_env=\{\*\*env,/);
  assert.match(account,/web_help=subprocess\.check_output\(\[args.seed_binary,'web','--help'\],env=seed_env,/);
  const accountSupport=['m3_account_library.py','m3_account_resident_stress.py']
    .map(file=>readFileSync(new URL('../../scripts/'+file,import.meta.url),'utf8')).join('\n');
  assert.doesNotMatch(account+seed+accountSupport,/os\.environ\.(?:pop|clear|update)\(/);
  assert.doesNotMatch(validation.split('\njobs:\n')[0],/GH_TOKEN:|GITHUB_TOKEN:/);
  const consume=validation.split('      - name: Consume only verified published release bytes\n')[1].split('\n      - ')[0];
  assert.match(consume,/GH_TOKEN: \$\{\{ github.token \}\}/);
  assert.match(validation,/GH_TOKEN='\$\{\{ github.token \}\}' node npm\/scripts\/extract-artifact.mjs --pack-published/);
  const login=readFileSync(new URL('../../scripts/dev-login-interruption-smoke.py',import.meta.url),'utf8');
  assert.match(login,/and k not in \("GH_TOKEN", "GITHUB_TOKEN"\)/);
  const loginSupport=['account','cleanup','continuations','control','transfer']
    .map(part=>readFileSync(new URL('../../scripts/login_interruption_'+part+'.py',import.meta.url),'utf8')).join('\n');
  assert.doesNotMatch(login+loginSupport,/os\.environ\.(?:pop|clear|update)\(/);
  const sweep=readSplitPowerShellFixture(new URL('../../scripts/dev-release-sweep.ps1',import.meta.url));
  assert.match(sweep,/foreach \(\$tokenName in @\('GH_TOKEN', 'GITHUB_TOKEN'\)\)\s*\{\s*\[void\]\$psi.Environment.Remove\(\$tokenName\)/);
});
