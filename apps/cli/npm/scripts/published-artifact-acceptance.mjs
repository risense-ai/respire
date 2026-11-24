import { execFileSync } from "node:child_process";
import { join } from "node:path";
import { existsSync, readFileSync } from "node:fs";
import { hashPattern, ownKeys, positiveID, publishedAssetNames, requireValue, sha256, sourcePattern } from "./published-artifact-values.mjs";
import { compareVersion, payloadFiles, verifyFile } from "./published-artifact-payload.mjs";
import { validatePublishedProcessEvidence } from "./published-artifact-process-evidence.mjs";

function publishedUpgradeSelection(report) {
  // Use the producer's policy, including mandatory DEV checkpoints and exact platform gaps.
  return JSON.parse(execFileSync(process.platform === 'win32' ? 'py' : 'python3', [...(process.platform === 'win32' ? ['-3'] : []), '-I', '-B', '-c', `
import json, sys
sys.path.insert(0, '.')
from published_upgrade_policy import DATABASE_DEV_CHECKPOINTS, platform_applicability, select_upgrade_sources
report = json.load(sys.stdin)
target = report['target']
extension = '.exe' if 'windows' in target else ''
required = [f'rsrs-{target}{extension}', f'rsrs-{target}-runtime.tar.gz', f'cli-build-{target}.json']
sources, gaps = {}, {}
for tag in [*report['published_formal_sources'], *DATABASE_DEV_CHECKPOINTS]:
    try:
        gaps[tag] = platform_applicability(tag, target, [])
    except ValueError:
        pass  # This target/release has no policy exemption for absent artifacts.
    present = tag not in gaps or any(row.get('version') == tag[1:] and row.get('passed') is True for row in report['sources'])
    sources[tag] = {'assets': [{'name': name} for name in required] if present else []}
result = {'checkpoints': DATABASE_DEV_CHECKPOINTS, 'gaps': gaps,
          'selected': sorted(select_upgrade_sources(sources, target, report['profile']))}
if report.get('macos_shards'):
    import runpy
    result['model_key'] = runpy.run_path('native-model-cache.py')['cache_key']()
print(json.dumps(result))
`], {cwd:new URL('../../scripts/',import.meta.url), input:JSON.stringify(report), encoding:'utf8', windowsHide:true}));
}
export function validatePublishedFull(spec, targets, manifestHash, directory, jobs, context, {candidate,client2=candidate?.client2,evidenceOwners,independent=Boolean(candidate && !client2),macosShardsRequired=false} = {}) {
  requireValue(sourcePattern.test(context.sha || '') && positiveID(context.run_id) && positiveID(context.run_attempt), 'Invalid full run identity');
  const processes = [], reports = [], historicalUpgrades = [];
  function job(predicate, label) {
    const matches = jobs.filter(predicate);
    requireValue(matches.length === 1 && matches[0].status === 'completed' && matches[0].conclusion === 'success'
      && matches[0].run_id === (evidenceOwners?.[matches[0].name] || context).run_id
      && matches[0].run_attempt === (evidenceOwners?.[matches[0].name] || context).run_attempt
      && matches[0].head_sha === (evidenceOwners?.[matches[0].name] || context).sha, 'Missing successful exact-attempt full job: ' + label);
    return matches[0];
  }
  function checkRunId(job) {
    const id = job.check_run_url?.match(/\/check-runs\/([1-9]\d*)$/)?.[1];
    requireValue(id, 'Missing check run identity: '+job.name);
    return id;
  }
  function read(artifact, name) {
    const folder = join(directory,artifact);
    const paths = payloadFiles(folder).filter(path => path === name || path.endsWith('/'+name));
    requireValue(paths.length === 1, 'Missing or duplicate full report: ' + artifact + '/' + name);
    const bytes = readFileSync(join(folder,paths[0]));
    reports.push({artifact,path:paths[0],sha256:sha256(bytes)}); return JSON.parse(bytes);
  }
  const identity = (r,target) => {
    const binaryHash = spec.cli.assets.find(a => a.name === publishedAssetNames('cli',target)[1]).sha256;
    requireValue(r.source_sha === spec.cli.source_sha && r.version === spec.cli.version && r.binary_sha256 === binaryHash, 'Full report product identity differs');
  };
  for (const target of targets) {
    job(j => j.name === `validate / Release (${target})`,target+' asset validation');
    job(j => j.name === `native-regression / Native (${target})`,target+' native');
    const verified = candidate ? read('candidate-assets-'+target,'candidate-assets-receipt.json') : read('published-assets-verification-'+target,'published-assets-receipt.json');
    if(candidate) {
      requireValue(verified.kind==='candidate-assets-verification' && JSON.stringify(verified.candidate)===JSON.stringify(candidate)
        && verified.target===target && verified.manifest_sha256===manifestHash && verified.assets_verified===true && verified.payloads_verified===true
        && JSON.stringify(verified.cli_assets)===JSON.stringify(publishedAssetNames('cli',target).map(name=>spec.cli.assets.find(a=>a.name===name)))
        && verified.payload_identity?.cli_binary_sha256===spec.cli.assets.find(a=>a.name===publishedAssetNames('cli',target)[1]).sha256
        && hashPattern.test(verified.payload_identity?.runtime_core?.sdk_manifest_expected_sha256||'')
        && hashPattern.test(verified.payload_identity?.runtime_core?.sdk_archive_expected_sha256||''), 'Candidate asset/Core identity differs');
    } else {
    requireValue(verified.assets_verified === true && verified.payloads_verified === true && !verified.error
      && verified.target === target && verified.manifest_sha256 === manifestHash && verified.harness_sha === context.sha
      && Number(verified.run_id) === context.run_id && Number(verified.run_attempt) === context.run_attempt
      && verified.product_sources.cli === spec.cli.source_sha && verified.product_sources.runtime === spec.runtime.source_sha, 'Published asset proof differs');
    for (const kind of ['cli','runtime']) {
      const release = verified.releases?.[kind], expected = spec[kind];
      requireValue(release?.repository === expected.repository && release.release_id === expected.release_id
        && release.source_sha === expected.source_sha && release.version === expected.version
        && release.producer?.run_id === expected.producer.run_id
        && release.producer?.run_attempt === expected.producer.run_attempt, 'Full producer identity differs');
    }
    const binding = verified.runtime_producer, runtimeNames = publishedAssetNames('runtime', target);
    requireValue(binding?.run_id === spec.runtime.producer.run_id && binding.run_attempt === spec.runtime.producer.run_attempt
      && binding.source_sha === spec.runtime.source_sha && binding.target === target
      && ['metadata_sha256','binary_sha256','archive_sha256'].every((key, i) =>
        binding[key] === spec.runtime.assets.find(asset => asset.name === runtimeNames[i]).sha256)
      && Array.isArray(binding.artifacts) && binding.artifacts.length === 2
      && [`runtime-release-${target}`,`runtime-smoke-${target}`].every(name => binding.artifacts.filter(artifact =>
        artifact.name === name && positiveID(artifact.id) && positiveID(artifact.job_id) && hashPattern.test(artifact.zip_sha256)).length === 1),
    'Full Runtime producer byte binding differs');
    const expectedAssets=['cli','runtime'].flatMap(kind=>publishedAssetNames(kind,target).map(name=>({repository:spec[kind].repository,...spec[kind].assets.find(a=>a.name===name)})));
    requireValue(Array.isArray(verified.assets) && verified.assets.length===expectedAssets.length && expectedAssets.every(a=>verified.assets.filter(b=>['repository','id','name','size','sha256'].every(k=>a[k]===b[k])).length===1), 'Full asset identities differ');
    }
    if(candidate) {
      const binding=verified.runtime_producer,names=publishedAssetNames('runtime',target);
      requireValue(binding?.run_id===spec.runtime.producer.run_id && binding.run_attempt===spec.runtime.producer.run_attempt
        && binding.source_sha===spec.runtime.source_sha && binding.target===target
        && ['metadata_sha256','binary_sha256','archive_sha256'].every((key,i)=>binding[key]===spec.runtime.assets.find(a=>a.name===names[i]).sha256)
        && binding.artifacts?.length===2 && [`runtime-release-${target}`,`runtime-smoke-${target}`].every(name=>binding.artifacts.filter(a=>a.name===name&&positiveID(a.id)&&positiveID(a.job_id)&&hashPattern.test(a.zip_sha256)).length===1), 'Candidate Runtime producer byte binding differs');
    }
    const native = read('resident-concurrency-'+target,'real-account-return-verification.json');
    requireValue(Array.isArray(native) && native.length === 1 && native[0].binary_sha256 === spec.cli.assets.find(a => a.name === publishedAssetNames('cli',target)[1]).sha256, 'Native report binary differs');
    processes.push(...validatePublishedProcessEvidence(native[0],spec,target,manifestHash));
    requireValue(read('resident-concurrency-'+target,'resident-stress-metrics.json').passed === true, 'Resident stress incomplete');
    read('resident-concurrency-'+target,'fixture-dependencies.json');
    const bounded = read('resident-concurrency-'+target,'fixture-process-verification.json');
    requireValue(Array.isArray(bounded) && bounded.length > 0 && bounded.every(r => r.passed === true), 'Fixture process bounds incomplete');
    const legacyUpgrades = 'published-upgrades-'+target;
    const aggregateJob = 'native-regression / Apple Silicon upgrade aggregate';
    const aggregateOwner = evidenceOwners?.[aggregateJob] || context;
    const attemptUpgrades = legacyUpgrades+'-'+aggregateOwner.run_id+'-'+aggregateOwner.run_attempt;
    const sharded = target === 'aarch64-apple-darwin' && (macosShardsRequired || existsSync(join(directory,attemptUpgrades))
      || jobs.some(j => j.name === aggregateJob && j.conclusion !== 'skipped'));
    let aggregateCheckRunId;
    if (sharded) {
      requireValue(!existsSync(join(directory,legacyUpgrades)), 'Mixed serial and sharded Apple Silicon upgrade evidence');
      aggregateCheckRunId = checkRunId(job(j => j.name === aggregateJob, 'Apple Silicon aggregate'));
    }
    const upgradeArtifact = sharded ? attemptUpgrades : legacyUpgrades;
    const upgrades = read(upgradeArtifact,'published-upgrades.json');
    const full = upgrades.profile === 'full';
    requireValue(upgrades.passed === true && ['critical','full'].includes(upgrades.profile) && (!candidate || full)
      && upgrades.full_coverage === full && upgrades.all_published_sources_covered === full
      && upgrades.upgrade_contract === 'standalone-v1'
      && upgrades.target === target && upgrades.candidate_sha256 === native[0].binary_sha256
      && Array.isArray(upgrades.published_formal_sources) && upgrades.published_formal_sources.length > 0
      && new Set(upgrades.published_formal_sources).size === upgrades.published_formal_sources.length
      && upgrades.published_formal_sources.every(tag => /^v\d+\.\d+\.\d+$/.test(tag) && compareVersion(tag.slice(1),'1.0.6') >= 0)
      && Array.isArray(upgrades.selected_sources) && Array.isArray(upgrades.sources)
      && upgrades.sources.every(row => row && typeof row === 'object' && !Array.isArray(row))
      && upgrades.additional_draft_baselines?.length === 0, 'Historical upgrade coverage or identity differs');
    const policy = publishedUpgradeSelection(upgrades);
    requireValue(ownKeys(upgrades.database_dev_checkpoints,Object.keys(policy.checkpoints))
      && Object.entries(policy.checkpoints).every(([tag,reason]) => upgrades.database_dev_checkpoints[tag] === reason)
      && JSON.stringify([...upgrades.selected_sources].sort()) === JSON.stringify(policy.selected)
      && upgrades.sources.length === policy.selected.length, 'Required formal or database DEV upgrade sources differ');
    let shardPlan, shardChecks;
    if (sharded) {
      shardPlan = read(upgradeArtifact,'macos-shard-plan.json');
      const proof = upgrades.macos_shards;
      const runtimeNames = publishedAssetNames('runtime',target);
      const expectedIdentity = {
        cli_sha256:native[0].binary_sha256, runtime_sha256:spec.runtime.assets.find(a => a.name === runtimeNames[1]).sha256,
        runtime_version:spec.runtime.version, manifest_sha256:manifestHash,
        bundle_sha256:spec.runtime.assets.find(a => a.name === runtimeNames[2]).sha256,
        model_key:policy.model_key, fixture_sha:aggregateOwner.sha,
      };
      const run = shardPlan.run;
      requireValue(full && proof?.plan_sha256 === reports.at(-1).sha256 && proof.check_run_id === aggregateCheckRunId
        && shardPlan.schema === 1 && shardPlan.target === target && run?.repository === spec.cli.repository
        && run.run_id === String(aggregateOwner.run_id) && run.run_attempt === String(aggregateOwner.run_attempt) && run.workflow_sha === aggregateOwner.sha
        && ownKeys(proof.run,Object.keys(run)) && Object.keys(run).every(k => run[k] === proof.run[k])
        && [shardPlan.identity,proof.identity].every(identity => ownKeys(identity,Object.keys(expectedIdentity))
          && Object.keys(expectedIdentity).every(k => expectedIdentity[k] !== undefined && identity[k] === expectedIdentity[k]))
        && ownKeys(shardPlan.inputs,policy.selected) && Array.isArray(proof.source_evidence)
        && new Set(Object.values(shardPlan.inputs).map(item => item?.release_id)).size === policy.selected.length
        && proof.source_evidence.length === policy.selected.length
        && new Set(proof.source_evidence.map(row => row?.process_report_sha256)).size === policy.selected.length
        && ['published_formal_sources','database_dev_checkpoints','registry_formal_versions','additional_draft_baselines']
          .every(key => JSON.stringify(shardPlan.inventory?.[key]) === JSON.stringify(upgrades[key])),
      'Apple Silicon upgrade shard plan or byte identity differs');
      const shardPrefix = 'native-regression / Apple Silicon upgrade (';
      const names = policy.selected.map(tag => shardPrefix+tag+')');
      requireValue(jobs.filter(j => j.name.startsWith(shardPrefix)).length === names.length,
        'Unexpected Apple Silicon upgrade shard job inventory');
      shardChecks = new Map(policy.selected.map((tag,index) => {
        const name = names[index];
        const shard = job(j => j.name === name, name);
        requireValue(shard.run_id === aggregateOwner.run_id && shard.run_attempt === aggregateOwner.run_attempt
          && shard.head_sha === aggregateOwner.sha, 'Apple Silicon shard and aggregate owners differ: '+name);
        return [tag,checkRunId(shard)];
      }));
      requireValue(new Set([aggregateCheckRunId,...shardChecks.values()]).size === names.length+1,
        'Duplicate Apple Silicon check run identity');
    } else {
      requireValue(upgrades.macos_shards === undefined, 'Apple Silicon shard evidence has no exact aggregate job');
    }
    const passed = [], notApplicable = [];
    for (const version of upgrades.selected_sources) {
      const row = upgrades.sources.filter(r => 'v'+r.version === version);
      requireValue(row.length === 1 && row[0].formal === upgrades.published_formal_sources.includes(version), 'Missing or duplicate historical upgrade result');
      if (row[0].status === 'not-applicable') {
        requireValue(row[0].passed === undefined && policy.gaps[version] && row[0].reason === policy.gaps[version], 'Unapproved historical platform gap');
        notApplicable.push(version);
      } else {
        requireValue(row[0].passed === true && row[0].status === undefined
          && row[0].upgrade_contract === 'standalone-v1', 'Failed historical upgrade source');
        const check = read(upgradeArtifact,version+'/results/real-account-return-verification.json');
        requireValue(Array.isArray(check) && check.length === 1 && check[0].binary_sha256 === native[0].binary_sha256, 'Upgrade process binary differs');
        requireValue(check[0].upgrade_contract === 'standalone-v1' && check[0].runtime_installed_after_legacy_data === true
          && check[0].legacy_handoff_actor === null, 'Historical database upgrade must install Runtime after legacy data without starting a legacy host');
        if (sharded) {
          const source = shardPlan.inputs[version];
          const names = [`rsrs-${target}`,`rsrs-${target}-runtime.tar.gz`,`cli-build-${target}.json`];
          const evidence = upgrades.macos_shards.source_evidence.filter(item => item?.source === version);
          requireValue(positiveID(source?.release_id) && ownKeys(source.assets,names)
            && names.every(name => positiveID(source.assets[name]?.id) && /^sha256:[a-f0-9]{64}$/.test(source.assets[name]?.digest || ''))
            && new Set(names.map(name => source.assets[name].id)).size === names.length
            && evidence.length === 1 && evidence[0].process_report_sha256 === reports.at(-1).sha256
            && evidence[0].check_run_id === shardChecks.get(version)
            && check[0].seed_source === version && 'sha256:'+check[0].seed_sha256 === source.assets[names[0]].digest,
          'Apple Silicon historical source or shard report digest differs: '+version);
        }
        processes.push(...validatePublishedProcessEvidence(check[0],spec,target,manifestHash));
        passed.push(version);
      }
    }
    requireValue(passed.length > 0 || (target === 'x86_64-apple-darwin' && upgrades.first_platform_release === true
      && [...upgrades.published_formal_sources,...Object.keys(policy.checkpoints)].every(tag => policy.gaps[tag])
      && upgrades.upgrade_status === 'not-applicable: first Intel macOS release; fresh inference is validated separately'), 'No actual applicable upgrade was exercised');
    historicalUpgrades.push({target,profile:upgrades.profile,all_published_sources_covered:full,passed,not_applicable:notApplicable});
    if (target !== 'x86_64-unknown-linux-gnu') {
      job(j => j.name.startsWith('cloud-sweep (') && j.name.endsWith(', '+target+')'),target+' cloud');
      const base = read('base-release-sweep-'+target,'coverage.json');
      requireValue(base.source_sha === spec.cli.source_sha && base.binary_sha256 === native[0].binary_sha256
        && base.observed_version === spec.cli.version && base.expected_version === spec.cli.version
        && base.base_complete === true && base.failed === 0 && base.required_missing?.length === 0, 'Base sweep incomplete');
      if (target.includes('linux')) {
        const combined = read('release-sweep-'+target,'combined-coverage.json'); identity(combined,target);
        requireValue(combined.schema === 2 && combined.scope === 'database-in-place-upgrade'
          && combined.profile_import?.required === false && combined.profile_import.status === 'not-run'
          && combined.complete === true, 'Full supplement join incomplete or scope differs');
        const outcomes = read('release-sweep-'+target,'suite-results.json');
        requireValue(Array.isArray(outcomes) && outcomes.length===5 && ['outbox','ai_inject','runtime','legacy_vault','login_interruption'].every(name=>outcomes.filter(r=>r.suite===name&&r.passed===true&&r.exit===0).length===1), 'Full suite outcomes incomplete');
        for (const name of ['ai-inject-coverage.json','legacy-vault-coverage.json','interruption-coverage.json']) {
          const r=read('release-sweep-'+target,name); identity(r,target);
          requireValue(r.passed===true || name==='ai-inject-coverage.json' && r.status==='passed', 'Full raw supplement incomplete');
        }
        for (const name of ['outbox-coverage.json','runtime-coverage.json']) {
          const r = read('release-sweep-'+target,name); identity(r,target);
          processes.push(...validatePublishedProcessEvidence(r,spec,target,manifestHash));
        }
        const login = read('secret-service-profile-migration-'+target,'interruption-coverage.json'); identity(login,target);
        requireValue(login.passed === true, 'Native login interruption incomplete');
      }
    }
  }
  const login = read('gnu-login-interruption','interruption-coverage.json'); identity(login,'x86_64-unknown-linux-gnu');
  requireValue(login.passed === true, 'GNU login interruption incomplete');
  job(j => j.name === 'validate / npm-dry-run','npm packaging');
  const packages=read('npm-release-packages',candidate?'source-version.json':'published-package-validation.json');
  const expectedPackages=JSON.parse(readFileSync(new URL('../artifact-manifest.json',import.meta.url)));
  const packageNames=[...expectedPackages.targets.map(t=>t.pkg),expectedPackages.npmScope];
  requireValue(packageNames.length===targets.length+1 && packageNames.every(name=>packages.packages?.filter(p=>p.name===name).length===1) && (candidate ? packages.git_sha===spec.cli.source_sha && packages.version===spec.cli.version
    && packages.producer?.repository===spec.cli.repository && Number(packages.producer.run_id)===spec.cli.producer.run_id && Number(packages.producer.run_attempt)===spec.cli.producer.run_attempt && packages.producer.sha===spec.cli.source_sha && packages.producer.job==='npm-dry-run' : packages.artifact_source==='published-assets' && packages.harness_sha===context.sha && packages.manifest_sha256===manifestHash
    && packages.product_sources?.cli===spec.cli.source_sha && packages.product_sources?.runtime===spec.runtime.source_sha
    && packages.run_id===context.run_id && packages.run_attempt===context.run_attempt)
    && packages.packages?.length===targets.length+1 && new Set(packages.packages.map(p=>p.name)).size===targets.length+1
    && packages.packages.every(p=>p.version===spec.cli.version && hashPattern.test(p.sha256)), 'Published package validation differs');
  for(const packed of packages.packages) verifyFile(join(directory,'npm-release-packages'),{path:packed.file,sha256:packed.sha256});
  const gui=independent ? null : read('client2-published-online-x86_64-pc-windows-msvc','report.json');
  if(gui)validateFullGUI(gui,spec,manifestHash,{candidate:Boolean(candidate),client2});
  job(j => j.name === 'api-smoke','API');
  const apiOwner=evidenceOwners?.['api-smoke'] || context;
  const apiIdentity=read('dev-api-coverage','published-api-identity.json');
  requireValue(apiIdentity.harness_sha===apiOwner.sha && apiIdentity.manifest_sha256===manifestHash
    && apiIdentity.run_id===apiOwner.run_id && apiIdentity.run_attempt===apiOwner.run_attempt, 'API report run binding differs');
  for (const name of ['dev-api-coverage.json','dev-github-api-coverage.json']) {
    const api = read('dev-api-coverage',name);
    requireValue(api.complete === true && !api.error
      && (name!=='dev-api-coverage.json' || api.cli_workflow_sha===apiOwner.sha)
      && apiIdentity.reports?.[name]===reports.at(-1).sha256, 'API coverage incomplete or stale');
  }
  return {schema:2,scope:'database-in-place-upgrade',
    profile_import:{required:false,status:'not-run',diagnostic_workflow:'.github/workflows/migration-keyring-check.yml'},
    kind:candidate?'candidate-full':'published-assets-full',manifest_sha256:manifestHash,harness_sha:context.sha,
    product_sources:{cli:spec.cli.source_sha,runtime:spec.runtime.source_sha},...context,
    ...(client2?.sealed?{sealed_client2:gui.sealed_client2}:{}),
    reports,actual_processes:processes,historical_upgrades:historicalUpgrades,
    full_passed:historicalUpgrades.every(report => report.profile === 'full'),gui_passed:gui ? true : null,g1Passed:false,
    limits:['CLI full acceptance does not authorize cross-product fullG1 or stable publication','Process image receipts hash OS-identified executable files, not loaded memory pages','Eight configured CLI platforms and the existing Windows Client2 GUI subset; real user database copies are not covered','API fixtures are server-contract evidence, not CLI asset execution','Core bytes are checked against official metadata and exact-source SDK locks, not an independent private SDK archive download',candidate?'Candidate installation uses frozen local CLI and Runtime bytes; public-channel discovery remains postpublication evidence':'npm packaging uses the trusted harness wrapper plus immutable published binaries; it is not a download of the published npm tarballs']};
}
export function validateFullGUI(gui,spec,manifestHash,{candidate=false,client2}={}) {
  const target='x86_64-pc-windows-msvc';
  requireValue(gui.status==='connectivity-passed' && gui.environment_ready===true && gui.cleanup_exit_confirmed===true && !gui.cleanup_error && !gui.error
    && gui.cli_sha256===spec.cli.assets.find(a=>a.name===publishedAssetNames('cli',target)[1]).sha256
    && gui.runtime_archive_sha256===spec.runtime.assets.find(a=>a.name===publishedAssetNames('runtime',target)[2]).sha256
    && (gui.official_exe_verified===true || gui.source_candidate_verified===true)
    && hashPattern.test(gui.exe_sha256||'') && sourcePattern.test(gui.source_sha||gui.source_candidate?.source_commit||'')
    && gui.native_window_visible===true && gui.isolated_webview_bound===true && gui.native_accessibility_read===true
    && gui.foreground_verified===true && gui.gui_input_observed===true && gui.target_desktop_screenshot_saved===true
    && gui.restart?.completed===true && gui.restart.original_tree_absent===true
    && gui.recovery?.status==='recovery-subset-passed' && gui.recovery.original_operation_not_repeated===true
    && gui.recovery.saved_restart?.saved_restart_no_prompt===true, 'Missing successful real GUI/restart/recovery/cleanup evidence');
  for(const r of [gui.initial_native_gui,gui.restart_native_gui])requireValue(r?.status==='native-connectivity-passed' && r.identity_verified===true
    && r.offline?.status==='offline-subset-passed' && r.offline.recall_hit===true && r.offline.runtime_normal_stop_confirmed===true, 'GUI DB/recall acceptance incomplete');
  requireValue(gui.restart_native_gui.offline.automatic_start===true && gui.restart_native_gui.offline.automatic_key_handoff===true, 'GUI restart key handoff incomplete');
  if(client2) {
    requireValue(sourcePattern.test(client2.source_sha||'') && gui.source_sha===client2.source_sha
      && gui.release===client2.release, 'GUI Client2 source/release differs from the selected product');
    if(client2.sealed) {
      const proof=gui.sealed_client2, seal=client2.sealed;
      requireValue(candidate && gui.source_candidate_verified===true && gui.official_exe_verified===false
        && proof?.repository==='risense-ai/respire-client2' && proof.source_sha===client2.source_sha
        && `v${proof.version}`===client2.release && proof.six_package_receipts_verified===true
        && proof.run_id===seal.run_id && proof.run_attempt===seal.run_attempt && proof.artifact_id===seal.artifact_id
        && proof.artifact_zip_digest===seal.artifact_zip_digest && hashPattern.test(proof.manifest_sha256||'')
        && proof.exe_sha256===gui.exe_sha256, 'GUI Client2 seal differs from the selected product');
    } else requireValue(gui.official_exe_verified===true, 'GUI Client2 public executable was not verified');
  }
  const cliHash=spec.cli.assets.find(a=>a.name===publishedAssetNames('cli',target)[1]).sha256;
  // Earlier PowerShell reports can start with a null from an uninitialized array.
  for(const phase of ['initial','restart'])requireValue(gui.cli_processes?.some(p=>p?.phase===phase && p.sha256===cliHash
    && positiveID(p.pid) && positiveID(p.parent_pid) && typeof p.started_ticks==='string' && /^\d+$/.test(p.started_ticks)
    && p.parent_verified===true && p.session_verified===true && p.lifetime_verified===true), 'GUI did not observe the actual CLI image');
  const inputs=candidate?gui.candidate_inputs:gui.published_inputs;
  requireValue(inputs?.manifest_sha256===manifestHash && inputs.cli_source===spec.cli.source_sha && inputs.runtime_source===spec.runtime.source_sha,'GUI input identity differs');
  return gui;
}
