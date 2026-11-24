import {installPinnedRuntime} from '../../scripts/ci-install-pinned-runtime.mjs';
import {regressionContract} from './release-regression-contract.mjs';
import test from 'node:test';
import {gzipSync,gunzipSync} from 'node:zlib';
import {publishedAssetNames, validatePublishedRelease, validatePublishedMetadata, inspectPublishedRelease, inspectRuntimeProducer, downloadPublishedAsset, verifyPublishedTarget, consumePublishedTarget, extractPublishedArchive, verifyCorePayload, validatePublishedProcessEvidence, validatePublishedFull, validateFullGUI, validatePublishedPackage} from './extract-artifact.mjs';
import assert from 'node:assert/strict';
import {existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync} from 'node:fs';
import {createServer} from 'node:http';
import {tmpdir} from 'node:os';
import path from 'node:path';
import {promisify} from 'node:util';
import {runInNewContext} from 'node:vm';
import {createHash} from 'node:crypto';
import {execFile, execFileSync} from 'node:child_process';
import {waitForPublicInstall, publicInstallArgs, publicInstallPlatform, PUBLICATION_MAX_BUFFER, publicationCommand, scopedPublisherFinished, selectRelease, validateArtifact, assertTagCommit, assertDraft, assertSourceRun, validateAlias, validateScoped, validateSource, readyName, aliasName} from './release-publication.mjs';
import { alias, aliasRun, copy, jobs, m, release, scoped, sha, source, sourceRun, version } from "./release-publication-fixtures.mjs";
test('complete release inventory has one current-run publisher and no historical recovery', () => {
  const workflow = readFileSync(new URL('../../.github/workflows/release.yml', import.meta.url), 'utf8').replace(/\r\n/g, '\n');
  const definitions = [...workflow.split('\njobs:\n')[1].matchAll(/^  ([\w-]+):\n([\s\S]*?)(?=^  [\w-]+:\n|$(?![\s\S]))/gm)];
  assert(definitions.length > 5, 'Use the complete release job inventory');
  assert.doesNotMatch(workflow, /resume_dev6|repair_dev12|repair_dev14|reuse_dev12|dev6-publication-resume/);
  assert.doesNotMatch(workflow, /37716872717|37728949557|38251121403|63c3da9e64a4ce182bfd802518030b102dd362a3|d8314480e10a27767544f781083e35c8e85657f0|fd0e5463f8c59f304ff17d622efb1f6221f9b04c/);
  const inventory = definitions.map(([, id, body]) => ({id, name: body.match(/^    name: (.+)$/m)?.[1] || id, status: 'completed', conclusion: 'success'}));
  const active = inventory.find(j => j.id === 'publish');
  assert.equal(active.name, 'publish');
  assert.equal(inventory.filter(j => j.name === 'publish').length, 1);
  for (const status of ['queued', 'in_progress']) {
    active.status = status;
    assert.equal(scopedPublisherFinished(inventory), false);
  }
  active.status = 'completed';
  for (const conclusion of ['success', 'failure', 'skipped', 'cancelled']) {
    active.conclusion = conclusion;
    assert.equal(scopedPublisherFinished(inventory), true, 'Terminal publisher without a receipt must close the wait');
  }
});

test('large multi-page release inventory survives the real subprocess boundary without truncation', () => {
  const pages = [Array.from({length: 50}, (_, i) => ({id: i + 1000, tag_name: `v0.0.${i}`,
    body: 'Synthetic release notes'.repeat(2200)})), [{...release, draft: true}]];
  const input = JSON.stringify(pages);
  assert(Buffer.byteLength(input) > 1024 * 1024);
  const args = ['-e', 'process.stdout.write(require("node:fs").readFileSync(0))'];
  assert.throws(() => execFileSync(process.execPath, args, {input, encoding: 'utf8'}), error => error.code === 'ENOBUFS');
  const result = publicationCommand(process.execPath, args, {input});
  assert.equal(result, input);
  const decoded = JSON.parse(result);
  assert.equal(selectRelease(decoded, version).id, release.id);
  assert.throws(() => selectRelease([...decoded, [{...release, draft: true}]], version), /exactly one release/);
});

test('publication command output remains bounded above the valid large-list case', () => {
  assert.throws(() => publicationCommand(process.execPath,
    ['-e', `process.stdout.write('x'.repeat(${PUBLICATION_MAX_BUFFER + 1}))`]), error => error.code === 'ENOBUFS');
});

test('exact scoped packages, draft and installed alias produce a complete barrier', () => {
  validateSource(source); validateScoped(scoped, m); assertSourceRun(sourceRun, source);
  assertDraft(release, scoped); validateAlias(alias, scoped, aliasRun, jobs);
  assert.equal(readyName(source), 'scoped-ready-100-2');
  assert.equal(aliasName(source, aliasRun), 'alias-ready-100-2-400-3');
  const frontend={...m,artifactLayout:'cli-frontend-v2'},receipt=copy(scoped);
  receipt.assets=receipt.assets.filter(a=>!a.name.endsWith('-runtime.tar.gz'));
  assert.throws(()=>validateScoped(receipt,frontend));
  receipt.validation_inventory=regressionContract(frontend);
  validateScoped(receipt,frontend);
  const old=copy(receipt);old.validation_inventory.schema=1;assert.throws(()=>validateScoped(old,frontend));
  assert.throws(()=>validateScoped(scoped,frontend));
  assert.throws(()=>validateScoped(receipt,m));
  assert.throws(()=>validateScoped(receipt,{...frontend,artifactLayout:'unknown'}));
});
test('selected source may differ from the dispatch workflow head SHA', () => {
  assert.notEqual(sourceRun.head_sha, source.sha);
  assert.doesNotThrow(() => assertSourceRun(sourceRun, source));
});
test('all source identity and current-attempt checks fail closed', () => {
  for (const mutation of [{repository: 'foreign/repo'}, {workflow: '.github/workflows/ci.yml'}, {run_id: '0'}, {run_attempt: 'x'}, {sha: 'main'}, {version: 'latest'}, {channel: 'latest'}, {release_id: 0}]) {
    assert.throws(() => validateSource({...source, ...mutation}));
  }
  for (const mutation of [{id: 101}, {run_attempt: 1}, {path: '.github/workflows/ci.yml'}, {event: 'pull_request'}, {status: 'completed'}, {status: 'queued'}, {head_repository: {full_name: 'foreign/repo'}}]) {
    assert.throws(() => assertSourceRun({...sourceRun, ...mutation}, source));
  }
});
test('missing, duplicate, mismatched or digestless scoped packages never open gate', () => {
  for (const mutate of [r => r.packages.pop(), r => r.packages.push(r.packages[0]), r => r.packages[0].version = '1.2.3', r => r.packages[0].integrity = '', r => r.packages[0].shasum = 'bad']) {
    const r = copy(scoped); mutate(r); assert.throws(() => validateScoped(r, m));
  }
});
test('all native binaries and complete asset identities must remain bound', () => {
  for (const mutate of [r => r.assets.pop(), r => r.assets[0].size = -1, r => r.assets[0].sha256 = '', r => r.binaries.pop(), r => r.binaries[0].sha256 = 'f'.repeat(64), r => r.binaries[0].target = 'wrong']) {
    const r = copy(scoped); mutate(r); assert.throws(() => validateScoped(r, m));
  }
  for (const mutate of [r => r.draft = false, r => r.id++, r => r.target_commitish = 'main', r => r.tag_name = 'v1.2.3', r => r.prerelease = false, r => r.assets.pop(), r => r.assets[0].digest = 'sha256:' + 'f'.repeat(64), r => r.assets[0].state = 'new']) {
    const r = copy(release); mutate(r); assert.throws(() => assertDraft(r, scoped));
  }
});
test('alias failure, no receipt, still-running job, stale run and stale attempt cannot publish parent', () => {
  assert.throws(() => validateAlias(null, scoped, aliasRun, jobs));
  assert.throws(() => validateAlias(alias, scoped, {...aliasRun, conclusion: 'failure'}, jobs));
  assert.throws(() => validateAlias(alias, scoped, {...aliasRun, status: 'in_progress'}, jobs));
  assert.throws(() => validateAlias(alias, scoped, aliasRun, [{name: 'publish-integrated', conclusion: null}]));
  assert.throws(() => validateAlias(alias, scoped, {...aliasRun, run_attempt: 4}, jobs));
  for (const mutate of [r => r.source.run_attempt = '1', r => r.source.run_id = '101', r => r.source.sha = 'f'.repeat(40), r => r.source.version = '1.2.3-dev.5', r => r.source.release_id++, r => r.publisher.run_id = '401', r => r.publisher.workflow = '.github/workflows/ci.yml', r => r.scoped_packages[0].integrity = '', r => r.alias.name = 'other', r => r.alias.version = '1.2.3', r => r.install.binary_sha256 = 'f'.repeat(64), r => r.install.version = '1.2.3']) {
    const r = copy(alias); mutate(r); assert.throws(() => validateAlias(r, scoped, aliasRun, jobs));
  }
});
test('safe failed-job rerun needs fresh receipts while immutable package identities remain unchanged', () => {
  const retried = copy(scoped); retried.source.run_attempt = '3';
  assert.notEqual(readyName(retried.source), readyName(scoped.source));
  assert.deepEqual(retried.packages, scoped.packages);
  assert.throws(() => validateAlias(alias, retried, aliasRun, jobs));
  const newAlias = copy(alias); newAlias.source.run_attempt = '3';
  validateAlias(newAlias, retried, aliasRun, jobs);
});

test('workflow contract keeps trust identities, unprivileged waiting and acyclic publication locks', () => {
  const root = new URL('../../', import.meta.url);
  const child = readFileSync(new URL('.github/workflows/npm-alias.yml', root), 'utf8').replace(/\r\n/g, '\n');
  const parent = readFileSync(new URL('.github/workflows/release.yml', root), 'utf8').replace(/\r\n/g, '\n');
  const helper = readFileSync(new URL('npm/scripts/release-publication.mjs', root), 'utf8').replace(/\r\n/g, '\n');
  assert.doesNotMatch(child, /workflow_run:/);
  assert.match(child, /release_attempt:/);
  assert.doesNotMatch(child.slice(0, child.indexOf('jobs:')), /concurrency:/);
  const waiting = child.split('  await-scoped:')[1].split('  publish-integrated:')[0];
  assert.doesNotMatch(waiting, /concurrency:|id-token:/);
  const publishing = child.split('  publish-integrated:')[1].split('\n  publish:')[0];
  assert.match(publishing, /needs: await-scoped/);
  assert.match(publishing, /ref: \$\{\{ needs.await-scoped.outputs.sha \}\}/);
  assert.match(publishing, /group: npm-rsrs-cli-publication\n      queue: max\n      cancel-in-progress: false/);
  assert.match(publishing, /contents: read\n      actions: read\n      id-token: write/);
  assert.doesNotMatch(publishing, /contents: write|actions: write|workflow_call/);
  assert.match(waiting, /Read this Release attempt scoped publication receipt once/);
  assert.doesNotMatch(waiting, /concurrency:|id-token:/);
  assert.match(child.split('\n  publish:')[1], /if: github.event_name == 'workflow_dispatch'/);
  const final = parent.split('\n  publish:')[1];
  const indices = ['--publish --accepted-only --verified-packs npm/publication-packages --tag', 'scoped-accepted', 'name: scoped-accepted-', 'gh workflow run npm-alias.yml', 'await-alias-accepted', 'release-publication.mjs finalize-accepted'].map(s => final.indexOf(s));
  assert(indices.every(i => i >= 0));
  assert.deepEqual(indices, [...indices].sort((a,b) => a-b));
  assert.equal((helper.match(/'--draft=false'/g) || []).length, 1);
  assert(final.indexOf('release-publication.mjs protocol') < final.indexOf('--publish --accepted-only --verified-packs npm/publication-packages --tag'));
  assert.match(parent, /tags: \['v\*'\]/);
  assert.match(final, /group: cli-release-publication\n      queue: max\n      cancel-in-progress: false/);
  assert.match(final, /contents: write\n      actions: write\n      id-token: write/);
  const resolver=helper.split('async function awaitScoped()')[1].split('// Retry only')[0];
  assert.doesNotMatch(resolver,/while\s*\(|sleep\(|deadline/);
  assert.match(resolver,/Exact scoped receipt missing/);
  for (const name of ['Bind successful scoped npm submissions', 'Require every matching npm submission', 'Make the complete release public']) {
    const step=final.slice(final.indexOf(`- name: ${name}`)).split('\n      - ')[0];
    assert.match(step,/run: /);
    assert.doesNotMatch(step,/if:/);
  }
});

test('source CI cancels duplicates only within the same ref and SHA',()=>{
  const workflow=readFileSync(new URL('../../.github/workflows/ci.yml',import.meta.url),'utf8').replace(/\r\n/g,'\n');
  const concurrency=workflow.split('\nconcurrency:\n')[1].split('\njobs:')[0];
  const group=concurrency.match(/^  group: (.+)$/m)?.[1];
  assert.equal(group,'cli-ci-${{ github.ref }}-${{ github.sha }}');
  assert.match(concurrency,/^  cancel-in-progress: true$/m);
  const key=(ref,sha)=>group.replace('${{ github.ref }}',ref).replace('${{ github.sha }}',sha);
  assert.equal(key('refs/heads/main',sha),key('refs/heads/main',sha));
  assert.notEqual(key('refs/heads/main',sha),key('refs/heads/main','c'.repeat(40)));
  assert.notEqual(key('refs/heads/main',sha),key('refs/pull/1/merge',sha));
  const release=readFileSync(new URL('../../.github/workflows/release.yml',import.meta.url),'utf8').replace(/\r\n/g,'\n');
  assert.match(release,/prepared-import.mjs readiness/);
  const helper=readFileSync(new URL('./release-preparation.mjs',import.meta.url),'utf8');
  assert.match(helper,/workflows\/ci.yml\/runs\?head_sha=\$\{source.sha\}/);
  assert(helper.includes("r.head_sha===source.sha&&r.event==='push'&&r.head_branch==='main'"));
  assert(helper.includes('candidates.sort')||helper.includes('.sort((a,b)=>b.id-a.id)'));
  assert.match(helper,/const ci=api\(`repos\/\$\{REPO\}\/actions\/runs\/\$\{candidates\[0\].id\}`\)/);
  assert.match(helper,/completed\(\{...ci,/);
});

test('empty DEV draft resumption permits only a rerun to bypass its own active owner',()=>{
  const workflow=readFileSync(new URL('../../.github/workflows/release.yml',import.meta.url),'utf8').replace(/\r\n/g,'\n');
  const program=workflow.split('      - id: plan\n')[1].split("node - <<'NODE'\n")[1].split('\n          NODE')[0];
  const exercise=({owner='100',attempt='2',status='in_progress',reserved={},accepted=false,override=version,ref='main',legacyActive=''}={})=>{
    const calls=[],outputs=[];
    const draft={draft:true,prerelease:true,asset_count:0,body:`Release workflow run: ${owner}`,...reserved};
    const execute=()=>runInNewContext(program,{
      require:name=>{
        if(name==='fs')return {readFileSync:()=>`version = "${version}"`,appendFileSync:(_,value)=>outputs.push(value)};
        if(name==='./npm/artifact-manifest.json')return {cliVersion:version};
        assert.equal(name,'child_process');
        return {execFileSync:(command,args)=>{
          calls.push([command,...args]);
          if(command==='git'&&args[0]==='merge-base')return '';
          if(command==='git'&&args[0]==='rev-parse')return sha;
          if(command==='node'&&args[0]==='npm/scripts/next-dev-version.mjs')return version;
          if(command==='node'&&args[1]==='guard-reservation-accepted'){
            if(accepted)throw Error('accepted reservation refused');
            return '';
          }
          if(command==='gh'&&args[0]==='api'){
            if(args[1].includes('/releases?'))return JSON.stringify(draft);
            if(args[1].endsWith(`/actions/runs/${owner}`))return JSON.stringify({status});
            if(args[1].includes('/actions/workflows/release.yml/runs?'))return legacyActive;
          }
          if(command==='gh'&&args[0]==='release'&&args[1]==='edit')return '';
          assert.fail(`Unexpected plan command: ${command} ${args.join(' ')}`);
        }};
      },
      process:{env:{RELEASE_REF:ref,RELEASE_PUBLISH:'true',RELEASE_DEV_VERSION:override,
        GITHUB_REPOSITORY:source.repository,GITHUB_RUN_ID:'100',GITHUB_RUN_ATTEMPT:attempt,GITHUB_OUTPUT:'output'}},
    });
    return {execute,calls,outputs};
  };
  for(const options of [{},{attempt:'3'},{owner:'200',status:'completed'},
    {override:'',ref:`refs/tags/v${version}`}]){
    const check=exercise(options);check.execute();
    assert.equal(check.outputs.length,1);
    assert.equal(check.calls.filter(call=>call[0]==='gh'&&call[1]==='release'&&call[2]==='edit').length,1);
    const guard=check.calls.findIndex(call=>call.includes('guard-reservation-accepted'));
    assert(guard>=0&&guard<check.calls.findIndex(call=>call[1]==='release'));
  }
  for(const options of [{attempt:'1'},{attempt:''},{attempt:'invalid'},
    {owner:'200'},{owner:'200',status:'queued'},{owner:'200',attempt:'3'},
    {reserved:{draft:false}},{reserved:{prerelease:false}},{reserved:{asset_count:1}},
    {accepted:true},{override:''},{reserved:{body:''},legacyActive:'200'}]){
    const check=exercise(options);
    assert.throws(check.execute,/active|empty DEV draft|accepted reservation/);
    assert.equal(check.outputs.length,0);
    assert(!check.calls.some(call=>call[0]==='gh'&&call[1]==='release'));
  }
});

test('promotion reuses only exact imported scoped tarballs and never repacks publication bytes',()=>{
  const workflow=readFileSync(new URL('../../.github/workflows/release.yml',import.meta.url),'utf8').replace(/\r\n/g,'\n');
  const publisher=workflow.split('\n  publish:')[1];
  assert.match(publisher,/needs.prepared-import.result == 'success'/);
  assert.match(publisher,/name: imported-npm-packages/);
  assert.match(publisher,/git archive "\$GITHUB_SHA" npm\/scripts\/verify-packages\.mjs \| tar -x/);
  assert.match(publisher,/--reuse-packs npm\/release-packages --export-packs npm\/publication-packages/);
  assert.doesNotMatch(publisher,/--pack npm\/publication-packages|REUSE_VALIDATED_PACKS/);
  assert.match(publisher,/--publish --accepted-only --verified-packs npm\/publication-packages --tag/);
  const verifier=readFileSync(new URL('./verify-packages.mjs',import.meta.url),'utf8');
  for(const guard of ['readImportedPreparation','assertImportSelection','assertImportPin','readValidatedPacks','verifyPackFile'])assert(verifier.includes(guard));
});
test('two overlapping Releases progress without waiting while holding the opposite alias lock', () => {
  // Model the actual lock graph asserted above. B's early ready wait has no lock.
  let publication = 'A', aliasLock = null;
  const ready = new Set(['A']), done = new Set();
  const acquireAlias = id => { if (!ready.has(id) || aliasLock) return false; aliasLock = id; return true; };
  assert.equal(acquireAlias('B'), false); // B waits for scoped-ready without locking.
  assert.equal(acquireAlias('A'), true);
  done.add('A'); aliasLock = null;
  assert(done.has(publication)); publication = 'B'; ready.add('B');
  assert.equal(acquireAlias('B'), true); done.add('B'); aliasLock = null;
  assert(done.has(publication));
});

test('existing lightweight or resolved annotated tag must identify the release source', () => {
  assertTagCommit(null, source);
  assertTagCommit({type: 'commit', sha: source.sha}, source);
  assert.throws(() => assertTagCommit(null, source, {allowMissing: false}));
  assertTagCommit({type: 'commit', sha: source.sha}, source, {allowMissing: false});
  assert.throws(() => assertTagCommit({type: 'commit', sha: 'f'.repeat(40)}, source));
  assert.throws(() => assertTagCommit({type: 'tag', sha: source.sha}, source));
});

test('missing is pending, but duplicate, expired or foreign-run artifacts are rejected', () => {
  const a = {expired: false, workflow_run: {id: 100}};
  assert.equal(validateArtifact([], 100), null);
  assert.equal(validateArtifact([a], 100), a);
  assert.throws(() => validateArtifact([a, a], 100));
  assert.throws(() => validateArtifact([{...a, expired: true}], 100));
  assert.throws(() => validateArtifact([a], 101));
});

test('all Actions uploads, including both handshake receipts, retain evidence for one day', () => {
  const root = new URL('../../.github/workflows/', import.meta.url);
  let uploads = 0;
  for (const file of readdirSync(root).filter(name => /\.ya?ml$/.test(name))) {
    const workflow = readFileSync(new URL(file, root), 'utf8');
    for (const match of workflow.matchAll(/^([ \t]*)(- )?uses: actions\/upload-artifact@[^\s]+.*$/gm)) {
      const stepIndent = match[1].length - (match[2] ? 0 : 2);
      const tail = workflow.slice(match.index + match[0].length);
      const nextStep = tail.search(new RegExp(`\\n[ \t]{0,${stepIndent}}\\S`));
      const body = nextStep < 0 ? tail : tail.slice(0, nextStep);
      assert.match(body, /^[ \t]+retention-days: 1[ \t]*$/m, `${file}: every upload must retain exactly one day`);
      uploads++;
    }
  }
  assert(uploads > 0, 'inspect the actual remaining uploads rather than a historical job count');
  const compatibility = readFileSync(new URL('client2-launcher-regression.yml', root), 'utf8');
  assert.match(compatibility, /name: client2-launcher-raw-\$\{\{ matrix.target \}\}-\$\{\{ github.run_id \}\}-\$\{\{ github.run_attempt \}\}/);
  const launcherRaw = [
    '          name: client2-launcher-raw-${{ matrix.target }}-${{ github.run_id }}-${{ github.run_attempt }}',
    '          path: |',
    '            ${{ runner.temp }}/client2-launcher-regression.json',
    '            ${{ runner.temp }}/client2-gui-probe/report.json',
    '            ${{ runner.temp }}/client2-gui-probe/native-gui.json',
    '            ${{ runner.temp }}/client2-gui-probe/workspace.png',
    '          if-no-files-found: warn',
    '          retention-days: 1',
  ].join('\n');
  assert(compatibility.replace(/\r\n/g, '\n').includes(launcherRaw),
    'original Client2 raw and exact GUI evidence paths must belong to the same named one-day upload');
  const candidate = readFileSync(new URL('migration-keyring-check.yml', root), 'utf8');
  assert.doesNotMatch(candidate, /runtime_candidate_sha:|source-account-regression:|\.account-runtime/);
  assert.match(candidate, /release_run_id:/);
  assert.match(candidate, /targeted-native-migration\/migration-platform-gate\.json/);
  assert.match(candidate, /native-migration-keyring\/keyring-selftest\.json/);
  const release = readFileSync(new URL('release.yml', root), 'utf8');
  assert.match(release, /prepared-import:/); assert.match(release, /preparation-receipt:/);
  for(const name of ['published-assets-verification-', 'published-full-result', 'npm-publication-packages-', 'scoped-accepted-', 'publication-accepted-'])assert(release.includes('name: '+name));
  const crates = readFileSync(new URL('publish-crates.yml', root), 'utf8');
  assert.match(crates, /name: protocol-package-verification/);
  assert.doesNotMatch(crates, /name: sdk-package-verification/);
});

test('draft release is resolved from every list page with exactly one matching tag', () => {
  const draft = {...release, draft: true};
  assert.equal(selectRelease([[{id: 299, tag_name: 'v1.2.3-dev.3'}], [draft]], version), draft);
  assert.throws(() => selectRelease([[{id: 299, tag_name: 'v1.2.3-dev.3'}]], version), /exactly one release/);
  assert.throws(() => selectRelease([[draft], [draft]], version), /exactly one release/);
  assert.throws(() => selectRelease([[{...draft, id: 0}]], version), /exactly one release/);
  assert.throws(() => selectRelease([null], version), /Invalid paginated/);
  assert.throws(() => selectRelease([[{...draft, draft: false}]], version), /must still be a draft/);
  const helper = readFileSync(new URL('./release-publication.mjs', import.meta.url), 'utf8');
  assert.doesNotMatch(helper, /releases\/tags\//);
  assert.match(helper, /selectRelease\(pages\(`/);
});


test('alias job and parent receipt budgets cover both propagation waits and clean install', () => {
  const root = new URL('../../', import.meta.url);
  const helper = readFileSync(new URL('npm/scripts/release-publication.mjs', root), 'utf8').replace(/\r\n/g, '\n');
  const child = readFileSync(new URL('.github/workflows/npm-alias.yml', root), 'utf8').replace(/\r\n/g, '\n');
  const parent = readFileSync(new URL('.github/workflows/release.yml', root), 'utf8').replace(/\r\n/g, '\n');
  const minutes = (text, pattern) => {
    const match = text.match(pattern);
    assert(match, 'The publication timeout must remain explicitly bound to its call or job');
    return Number(match[1]);
  };
  const scoped = minutes(helper, /verifyPackages\(scoped\.packages, \{[^}]*timeoutMs: (\d+) \* 60 \* 1000/);
  const alias = minutes(helper, /source\.channel, \{requireInstallIndex: true, timeoutMs: (\d+) \* 60 \* 1000/);
  const install = minutes(helper, /options\.timeoutMs \?\? (\d+) \* 60 \* 1000;\n  const deadline = now\(\)/);
  const job = minutes(child.split('  publish-integrated:')[1].split('\n  publish:')[0], /timeout-minutes: (\d+)/);
  const receipt = minutes(helper, /async function awaitAlias\(\) \{[\s\S]*?Date\.now\(\) \+ (\d+) \* 60 \* 1000;/);
  assert.equal(alias, scoped);
  assert(scoped >= 30);
  assert(job > scoped + alias + install, 'Job must leave room for setup and packaging');
  assert(receipt > job, 'Receipt observer must outlive the publisher');
  assert.doesNotMatch(parent,/dev6-publication-resume:|release-publication.mjs await-alias\s/);
  const acceptedObserver=parent.split('- name: Require every matching npm submission acceptance')[1].split('\n      - ')[0];
  const acceptance=readFileSync(new URL('npm/scripts/publication-acceptance.mjs',root),'utf8');
  const acceptedReceipt=minutes(acceptance,/async function awaitAliasAccepted\(\)\s*\{[\s\S]*?Date\.now\(\)\+(\d+)\*60\*1000/);
  assert(minutes(acceptedObserver,/timeout-minutes: (\d+)/)>acceptedReceipt);
});

test('npm resolver visibility delay retries exact pinned packages inside one deadline', async () => {
  let elapsed = 0, attempts = 0;
  const remaining = [], logged = [];
  const specs = [...scoped.packages, alias.alias].map(p => `${p.name}@${p.version}`);
  const output = await waitForPublicInstall(timeout => {
    remaining.push(timeout);
    elapsed += 2;
    if (++attempts <= 2) throw Object.assign(new Error('npm failed'), {status: 1,
      stdout: JSON.stringify({error: {code: 'ETARGET', summary: `No matching version found for ${specs[attempts - 1]}.`}})});
    return 'installed';
  }, specs, {timeoutMs: 30, pollMs: 5, now: () => elapsed, sleep: async ms => {elapsed += ms;},
    onRetry: (_, attempt) => logged.push(attempt)});
  assert.equal(output, 'installed');
  assert.deepEqual(remaining, [30, 23, 16]);
  assert.deepEqual(logged, [1, 2]);
});

test('npm resolver failures remain bounded and preserve the original error', async () => {
  const error = Object.assign(new Error('npm failed'), {status: 1,
    stdout: JSON.stringify({error: {code: 'ETARGET', summary: `No matching version found for rsrs-cli@${version}.`}})});
  for (const timeoutMs of [11, 30]) {
    let elapsed = 0, attempts = 0;
    await assert.rejects(waitForPublicInstall(() => {attempts++; throw error;}, [`rsrs-cli@${version}`],
      {timeoutMs, pollMs: 5, now: () => elapsed, sleep: async ms => {elapsed += ms;}}), actual => actual === error);
    assert.equal(attempts, 3);
  }
  let attempts = 0;
  await assert.rejects(waitForPublicInstall(() => {attempts++; throw error;}, [`rsrs-cli@${version}`],
    {timeoutMs: 5, pollMs: 5}), actual => actual === error);
  assert.equal(attempts, 1);
});

test('foreign ETARGET, malformed JSON, integrity, authorization and process failures never retry', async () => {
  const report = summary => JSON.stringify({error: {code: 'ETARGET', summary}});
  for (const fields of [
    {stdout: report('No matching version found for foreign@1.0.0.')},
    {stdout: report(`No matching version found for rsrs-cli@1.2.3-dev.99.`)},
    {stdout: 'invalid JSON'},
    {stdout: JSON.stringify({error: {code: 'EINTEGRITY', summary: 'bad digest'}})},
    {stdout: JSON.stringify({error: {code: 'E401', summary: 'unauthorized'}})},
    {status: null, signal: 'SIGTERM', code: 'ETIMEDOUT', stdout: report(`No matching version found for rsrs-cli@${version}.`)},
  ]) {
    const error = Object.assign(new Error('npm failed'), {status: 1}, fields);
    let attempts = 0;
    await assert.rejects(waitForPublicInstall(() => {attempts++; throw error;}, [`rsrs-cli@${version}`]),
      actual => actual === error);
    assert.equal(attempts, 1);
  }
});

test('real publication install opts into npm freshness and structured errors without changing its pin', () => {
  const args = publicInstallArgs('/isolated/install', '/isolated/cache', version);
  assert(args.includes('--prefer-online'));
  assert(args.includes('--json'));
  assert.equal(args.at(-1), `rsrs-cli@${version}`);
  assert.equal(args[args.indexOf('--registry') + 1], 'https://registry.npmjs.org');
});

test('actual npm resolver recovers from an offline registry index delay with a fresh request', async t => {
  const root = mkdtempSync(path.join(tmpdir(), 'rsrs-install-index-test-'));
  t.after(() => rmSync(root, {recursive: true, force: true}));
  const npmCli = [
    path.join(path.dirname(process.execPath), 'node_modules/npm/bin/npm-cli.js'),
    path.resolve(path.dirname(process.execPath), '../lib/node_modules/npm/bin/npm-cli.js'),
  ].find(existsSync);
  assert(npmCli, 'The test requires the npm installed beside Node');
  const env = {...process.env};
  for (const key of Object.keys(env)) if (/^(?:npm_config_|NODE_AUTH_TOKEN$|NPM_TOKEN$|GH_TOKEN$|GITHUB_TOKEN$|ACTIONS_ID_TOKEN_REQUEST_)/i.test(key)) delete env[key];
  Object.assign(env, {HOME: root, USERPROFILE: root, NPM_CONFIG_CACHE: path.join(root, 'cache'),
    NPM_CONFIG_USERCONFIG: path.join(root, 'empty.npmrc'), NPM_CONFIG_GLOBALCONFIG: path.join(root, 'global.npmrc')});
  writeFileSync(env.NPM_CONFIG_USERCONFIG, ''); writeFileSync(env.NPM_CONFIG_GLOBALCONFIG, '');
  const pkg = path.join(root, 'package');
  mkdirSync(pkg);
  writeFileSync(path.join(pkg, 'package.json'), JSON.stringify({name: 'rsrs-cli', version}));
  const execute = promisify(execFile);
  await execute(process.execPath, [npmCli, 'pack', '--ignore-scripts', '--json', '--pack-destination', root], {cwd: pkg, env, timeout: 30000});
  const tarball = readFileSync(path.join(root, readdirSync(root).find(file => file.endsWith('.tgz'))));
  let visible = false, indexRequests = 0, retries = 0, attempts = 0;
  const server = createServer((request, response) => {
    response.setHeader('Content-Type', 'application/json');
    if (request.url === '/rsrs-cli') {
      indexRequests++;
      const metadata = {name: 'rsrs-cli', version, dist: {
        tarball: `http://127.0.0.1:${server.address().port}/fixture.tgz`,
        integrity: `sha512-${createHash('sha512').update(tarball).digest('base64')}`,
        shasum: createHash('sha1').update(tarball).digest('hex')}};
      response.end(JSON.stringify({name: 'rsrs-cli', 'dist-tags': {dev: version},
        versions: visible ? {[version]: metadata} : {}}));
    } else if (request.url === '/fixture.tgz') {
      response.setHeader('Content-Type', 'application/octet-stream'); response.end(tarball);
    } else { response.statusCode = 404; response.end('{}'); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise(resolve => {server.close(resolve); server.closeAllConnections();}));
  const args = publicInstallArgs(path.join(root, 'install'), env.NPM_CONFIG_CACHE, version);
  args[args.indexOf('--registry') + 1] = `http://127.0.0.1:${server.address().port}`;
  args.splice(1, 0, '--ignore-scripts');
  await waitForPublicInstall(async timeout => {
    attempts++;
    try { return (await execute(process.execPath, [npmCli, ...args], {env, timeout})).stdout; }
    catch (error) {
      // execFileSync (production) calls this exit code status; execFile calls it code.
      error.status = error.code; throw error;
    }
  }, [`rsrs-cli@${version}`], {timeoutMs: 30000, pollMs: 1, onRetry: error => {
    assert.equal(JSON.parse(error.stdout).error.code, 'ETARGET');
    retries++; visible = true;
  }});
  assert.equal(retries, 1);
  assert.equal(attempts, 2);
  assert(indexRequests >= 2, 'The retry must fetch the registry again rather than reuse stale cache');
  assert.equal(JSON.parse(readFileSync(path.join(root, 'install/node_modules/rsrs-cli/package.json'))).version, version);
});


test('public installation selects explicit libc and requires actual matching host architecture',()=>{
  assert.equal(publicInstallPlatform('linux-x64-gnu',{platform:'linux',arch:'x64'}).libc,'glibc');
  for(const [platform,libc] of [['linux-arm64','musl'],['linux-arm64-gnu','glibc']]){
    assert.deepEqual(publicInstallPlatform(platform,{platform:'linux',arch:'arm64'}),{arch:'arm64',libc,pkg:'@rsrsai/'+platform,platform});
    assert.throws(()=>publicInstallPlatform(platform,{platform:'linux',arch:'x64'}));
    assert.throws(()=>publicInstallPlatform(platform,{platform:'darwin',arch:'arm64'}));
  }
  assert.throws(()=>publicInstallPlatform('foreign',{platform:'linux',arch:'arm64'}));
});

import "./release-artifact-publication-tests.mjs";
