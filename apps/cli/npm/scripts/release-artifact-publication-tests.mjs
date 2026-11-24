import test from "node:test";
import { createHash } from "node:crypto";
import { publishedAssetNames, validatePublishedRelease, validatePublishedMetadata, inspectPublishedRelease, inspectRuntimeProducer, downloadPublishedAsset, verifyPublishedTarget, consumePublishedTarget, extractPublishedArchive, verifyCorePayload, validatePublishedFull, validateFullGUI, validatePublishedPackage } from "./extract-artifact.mjs";
import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { tmpdir } from "node:os";
import { execFileSync } from "node:child_process";
import { gzipSync, gunzipSync } from "node:zlib";
import { installPinnedRuntime } from "../../scripts/ci-install-pinned-runtime.mjs";
import { copy, hash, jobs, release, sha, source, version } from "./release-publication-fixtures.mjs";

test('public release assets bind exact ID, dereferenced tag SHA, size and digest; paginated APIs never accept a draft', async () => {
  const bytes = Buffer.from('synthetic asset only'), digest = createHash('sha256').update(bytes).digest('hex');
  const asset = {id: 8, name: 'fixed.bin', size: bytes.length, sha256: digest};
  const part = {repository: 'risense-ai/respire-cli', release_id: 7, version: '1.0.21-dev.5', source_sha: 'a'.repeat(40), producer:{run_id:91,run_attempt:2}, assets: [asset]};
  const release = {id: 7, tag_name: 'v1.0.21-dev.5', draft: false, published_at: '2026-11-05T00:00:00Z'};
  const actual = {...asset, state: 'uploaded', digest: `sha256:${digest}`};
  validatePublishedRelease(part, release, [actual], part.source_sha);
  for (const bad of [{draft: true}, {id: 9}, {tag_name: 'v1.0.21-dev.6'}, {published_at: null}]) {
    assert.throws(() => validatePublishedRelease(part, {...release, ...bad}, [actual], part.source_sha));
  }
  for (const bad of [{id: 9}, {name: 'other.bin'}, {size: 1}, {state: 'new'}, {digest: 'sha256:' + 'b'.repeat(64)}]) {
    assert.throws(() => validatePublishedRelease(part, release, [{...actual, ...bad}], part.source_sha));
  }
  assert.throws(() => validatePublishedRelease(part, release, [actual], 'b'.repeat(40)));
  assert.throws(() => validatePublishedRelease(part, release, [actual, actual], part.source_sha));
  const calls = [], tag = 'b'.repeat(40);
  const fetcher = async url => {
    calls.push(url);
    let value;
    if (url.endsWith('/releases/7')) value = release;
    else if (url.endsWith('/git/ref/tags/v1.0.21-dev.5')) value = {object: {type: 'tag', sha: tag}};
    else if (url.endsWith('/git/tags/' + tag)) value = {object: {type: 'commit', sha: part.source_sha}};
    else if (url.endsWith('page=1')) value = [actual, ...Array.from({length: 99}, (_, i) => ({id: 100 + i, name: `extra-${i}`}))];
    else if (url.endsWith('page=2')) value = [];
    else if (url.includes('/actions/runs/91/attempts/2')) value = {id:91,run_attempt:2,head_sha:part.source_sha,status:'completed',conclusion:'success',path:'.github/workflows/release.yml',repository:{full_name:part.repository}};
    else throw Error('Unexpected metadata request ' + url);
    return {status: 200, json: async () => value};
  };
  assert.equal((await inspectPublishedRelease(part, {fetcher})).source_sha, part.source_sha);
  assert.ok(calls.some(url => url.endsWith('page=2')));
  for (const changed of [{run_attempt:3},{head_sha:'f'.repeat(40)},{conclusion:'failure'},
    {status:'in_progress'},{path:'.github/workflows/other.yml'},{repository:{full_name:'foreign/cli'}}]) {
    await assert.rejects(inspectPublishedRelease(part,{fetcher:async url=>{
      const response=await fetcher(url);
      return url.includes('/actions/runs/')?{status:200,json:async()=>({...await response.json(),...changed})}:response;
    }}),/producer run\/source\/attempt/);
  }
  await assert.rejects(inspectPublishedRelease(part, {fetcher: async () => ({status: 404})}), /HTTP 404/);
  await assert.rejects(inspectPublishedRelease(part, {fetcher: async url => {
    if (url.includes('/git/')) return {status: 200, json: async () => ({object: {type: 'tag', sha: tag}})};
    return fetcher(url);
  }}), /exact commit/);
  const root = mkdtempSync(path.join(tmpdir(), 'published-assets-test-'));
  try {
    const fetchBytes = async () => new Response(bytes);
    await downloadPublishedAsset(part, asset, root, {fetcher: fetchBytes});
    assert.deepEqual(readFileSync(path.join(root, asset.name)), bytes);
    await assert.rejects(downloadPublishedAsset(part, asset, root, {fetcher: fetchBytes}), /EEXIST/);
    assert.deepEqual(readFileSync(path.join(root, asset.name)), bytes, 'never remove an existing asset');
    const bad = {...asset, name: 'bad.bin', sha256: 'f'.repeat(64)};
    await assert.rejects(downloadPublishedAsset(part, bad, root, {fetcher: fetchBytes}), /hash\/size/);
    assert.equal(existsSync(path.join(root, bad.name)), false);
    await assert.rejects(downloadPublishedAsset(part, {...asset, name: 'oversize.bin', size: 1}, root, {fetcher: fetchBytes}), /approved size/);
    assert.equal(existsSync(path.join(root, 'oversize.bin')), false);
  } finally { rmSync(root, {recursive: true, force: true}); }
});
test('published CLI and Runtime metadata cannot swap source, version, target, hashes or unsafe payload inventory', () => {
  const target = 'x86_64-unknown-linux-gnu', hash = 'a'.repeat(64);
  const spec = {cli: {version, source_sha: sha, producer:{run_id:91,run_attempt:2}, assets: [
    {name: `rsrs-${target}`, size:64, sha256: hash}]},
    runtime: {version, source_sha: sha, assets: [{name: `rsrs-runtime-${target}`, sha256: hash},
      {name: `rsrs-runtime-${target}.tar.gz`, sha256: hash}]}};
  const cli = {schema_version: 2, artifact_layout:'cli-frontend-v2', git_sha: sha, version, target, binary_file: `rsrs-${target}`,
    binary_size:64, binary_sha256: hash, producer:spec.cli.producer};
  const runtime = {schema_version: 2, source_commit: sha, runtime_version: version, target_triple: target,
    binary_file: `rsrs-runtime-${target}`, archive_file: `rsrs-runtime-${target}.tar.gz`, binary_sha256: hash,
    archive_sha256: hash, protocol_range: {minimum: 1, maximum: 1}, capabilities: {business_executor: '1', mcp_server: '1'},
    files: [{path: 'rsrs-runtime', size: 10, sha256: hash}]};
  validatePublishedMetadata(spec, target, cli, runtime);
  for (const invalid of [{git_sha: 'c'.repeat(40)}, {version: '1.0.21-dev.99'}, {target: 'other'},
    {binary_sha256: 'f'.repeat(64)}, {runtime_file: 'other.tar.gz'}, {binary_size:65}, {producer:{run_id:92,run_attempt:2}}, {producer:{run_id:91,run_attempt:3}}]) {
    assert.throws(() => validatePublishedMetadata(spec, target, {...cli, ...invalid}, runtime));
  }
  for (const invalid of [{source_commit: 'c'.repeat(40)}, {runtime_version: '1.0.21-dev.99'},
    {target_triple: 'other'}, {archive_sha256: 'f'.repeat(64)}, {binary_sha256: 'f'.repeat(64)},
    {protocol_range: {minimum: 1, maximum: 2}}, {capabilities: {}}, {files: []},
    {files: [{path: '../escape', size: 1, sha256: hash}]}, {files: [runtime.files[0], runtime.files[0]]}]) {
    assert.throws(() => validatePublishedMetadata(spec, target, cli, {...runtime, ...invalid}));
  }
});

function producerZip(entries) {
  return execFileSync(process.platform === 'win32' ? 'py' : 'python3',
    [...(process.platform === 'win32' ? ['-3'] : []), '-I', '-c', `import base64,io,json,sys,zipfile
b=io.BytesIO()
with zipfile.ZipFile(b,'w',compression=zipfile.ZIP_DEFLATED) as z:
 for name,data,mode in json.load(sys.stdin):
  e=zipfile.ZipInfo(name); e.external_attr=mode<<16; z.writestr(e,base64.b64decode(data))
sys.stdout.buffer.write(b.getvalue())`],
    {input:JSON.stringify(entries.map(([name, bytes, mode=0o100644]) => [name, Buffer.from(bytes).toString('base64'), mode])), timeout:10000});
}
function runtimeProducerFixture(part, target, metadata, archive) {
  const digest = bytes => createHash('sha256').update(bytes).digest('hex');
  const names = publishedAssetNames('runtime', target), producer = part.producer;
  const report = {target, version:part.version, source_commit:part.source_sha, run_id:String(producer.run_id),
    run_attempt:String(producer.run_attempt), archive_sha256:digest(archive),
    binary_sha256:part.assets.find(a => a.name === names[1]).sha256, passed:true, g1Passed:false};
  const jobs = [`Release (${target})`,`Post-release native regression (${target})`].map((name, i) => ({id:i+11, name,
    run_id:producer.run_id, run_attempt:producer.run_attempt, head_sha:part.source_sha, status:'completed', conclusion:'success',
    started_at:'2026-11-05T00:00:00Z', completed_at:'2026-11-05T00:01:00Z'}));
  const zips = new Map(), artifacts = [];
  for (const [i, name, entries] of [[0,`runtime-release-${target}`,[[names[0],metadata],[names[2],archive]]],
    [1,`runtime-smoke-${target}`,[[`runtime-smoke-${target}.json`,JSON.stringify(report)]]]]) {
    const bytes = producerZip(entries); zips.set(i+31, bytes);
    artifacts.push({id:i+31, name, size_in_bytes:bytes.length, digest:`sha256:${digest(bytes)}`, expired:false,
      workflow_run:{id:producer.run_id, head_sha:part.source_sha}, created_at:'2026-11-05T00:00:20Z', updated_at:'2026-11-05T00:00:21Z'});
  }
  return {jobs, artifacts, zips, report,
    fetcher:async url => ({status:200, json:async () => {
      if (url.includes(`/attempts/${producer.run_attempt}/jobs?`)) return {jobs};
      assert(url.includes(`/runs/${producer.run_id}/artifacts?`)); return {artifacts};
    }}),
    artifactTransport:args => zips.get(Number(args.at(-1).split('/').at(-2))),
    replaceZip(index, entries) {
      const bytes=producerZip(entries), artifact=artifacts[index]; zips.set(artifact.id,bytes);
      artifact.size_in_bytes=bytes.length; artifact.digest=`sha256:${digest(bytes)}`;
    }};
}

test('published Runtime producer binds exact attempt reports and build bytes, including metadata', async () => {
  const target='x86_64-pc-windows-msvc', names=publishedAssetNames('runtime',target);
  const payloads=[Buffer.from('{"synthetic":"metadata"}'),Buffer.from('binary'),Buffer.from('archive')];
  const part={repository:'risense-ai/respire-runtime',version,source_sha:sha,producer:{run_id:91,run_attempt:2},
    assets:payloads.map((bytes,i)=>({name:names[i],size:bytes.length,sha256:createHash('sha256').update(bytes).digest('hex')}))};
  const fixture=()=>runtimeProducerFixture(part,target,payloads[0],payloads[2]);
  const run=f=>inspectRuntimeProducer(part,target,f);
  const evidence=await run(fixture());
  const prerelease=fixture();
  prerelease.jobs[1].name=`Pre-release native regression (${target})`;
  assert.deepEqual(await run(prerelease),evidence);
  prerelease.jobs.push(fixture().jobs[1]);
  await assert.rejects(run(prerelease),/producer job\/source\/attempt mismatch/);
  assert.equal(evidence.metadata_sha256,part.assets[0].sha256);assert.equal(evidence.binary_sha256,part.assets[1].sha256);
  assert.equal(evidence.archive_sha256,part.assets[2].sha256);assert.equal(evidence.run_attempt,2);assert.equal(evidence.artifacts.length,2);
  for(const change of [f=>f.jobs.pop(),f=>f.jobs.push(f.jobs[0]),f=>f.jobs[0].run_attempt=1,
    f=>f.jobs[0].run_id++,f=>f.jobs[0].head_sha='f'.repeat(40),f=>f.jobs[1].name='Native regression (other)',
    f=>f.jobs[0].status='in_progress',f=>f.jobs[1].conclusion='failure',f=>f.artifacts.pop(),f=>f.artifacts.push(f.artifacts[0]),
    f=>f.artifacts[0].expired=true,f=>delete f.artifacts[0].digest,f=>f.artifacts[0].workflow_run.id++,
    f=>f.artifacts[0].workflow_run.head_sha='f'.repeat(40),f=>f.artifacts[0].created_at='2026-11-04T00:00:00Z',
    f=>f.artifacts[0].updated_at='2026-11-06T00:00:00Z',f=>f.jobs[0].started_at='invalid',
    f=>f.artifacts[0].digest='sha256:'+'f'.repeat(64),f=>f.artifacts[0].size_in_bytes++,
    f=>f.zips.set(31,Buffer.from('tampered ZIP'))]) {
    const f=fixture();change(f);await assert.rejects(run(f));
  }
  for(const change of [r=>r.run_id='92',r=>r.run_attempt='1',r=>r.source_commit='f'.repeat(40),r=>r.target='other',
    r=>r.version='1.0.21-dev.99',r=>r.passed=false,r=>r.archive_sha256='f'.repeat(64),r=>r.binary_sha256='f'.repeat(64)]) {
    const f=fixture();change(f.report);f.replaceZip(1,[[`runtime-smoke-${target}.json`,JSON.stringify(f.report)]]);
    await assert.rejects(run(f),/native report\/source\/attempt\/bytes/);
  }
  for(const entries of [[[names[0],Buffer.from('{"synthetic":"different metadata"}')],[names[2],payloads[2]]],
    [[names[0],payloads[0]],[names[2],Buffer.from('different archive')]],
    [[names[0],payloads[0]]],[[names[0],payloads[0]],[names[0],payloads[0]],[names[2],payloads[2]]],
    [[names[0],payloads[0],0o120777],[names[2],payloads[2]]]]) {
    const f=fixture();f.replaceZip(0,entries);await assert.rejects(run(f));
  }
});

test('published target downloads and verifies both real file sets but records no process/full success before execution', async () => {
  for (const target of ['x86_64-unknown-linux-gnu','x86_64-apple-darwin','aarch64-apple-darwin','x86_64-pc-windows-msvc']) {
  const extension = target.includes('windows') ? '.exe' : '', root = mkdtempSync(path.join(tmpdir(), 'published-target-test-'));
  const hash = bytes => createHash('sha256').update(bytes).digest('hex');
  const image = Buffer.alloc(128);
  if (target.includes('darwin')) { image.writeUInt32LE(0xfeedfacf,0); image.writeUInt32LE(target.startsWith('aarch64')?0x100000c:0x1000007,4); }
  else if (target.includes('windows')) { image.write('MZ'); image.writeUInt32LE(64,0x3c); image.write('PE\0\0',64); image.writeUInt16LE(0x8664,68); image.writeUInt16LE(0x20b,88); }
  else { image.set([0x7f, 0x45, 0x4c, 0x46, 2, 1]); image.writeUInt16LE(62, 18); }
  const license = Buffer.from('synthetic license'), sdk = {sdk_version:'1.0.2-dev.23',manifest_sha256:'a'.repeat(64),archive_sha256:'b'.repeat(64),redistribution:'approved'};
  const core = Buffer.from(JSON.stringify({schema_version:1,target,sdk_version:sdk.sdk_version,redistribution:'permitted-under-included-license',files:[{path:'core-notices/CORE-SDK-LICENSE.txt',sha256:hash(license)}]}));
  const archive = syntheticTar([['core-runtime.json',core],['core-notices/CORE-SDK-LICENSE.txt',license],[`rsrs-runtime-${target}${extension}`,image]]);
  const fileHash = hash(image), archiveHash = hash(archive), manifestHash = 'd'.repeat(64);
  const cli = {schema_version: 2, artifact_layout:'cli-frontend-v2', git_sha: sha, version, target, binary_file: `rsrs-${target}${extension}`,
    binary_size:image.length, binary_sha256: fileHash, producer:{run_id:91,run_attempt:2}};
  const runtime = {schema_version: 2, source_commit: sha, runtime_version: version, target_triple: target,
    binary_file: `rsrs-runtime-${target}${extension}`, archive_file: `rsrs-runtime-${target}.tar.gz`, binary_sha256: fileHash,
    archive_sha256: archiveHash, protocol_range: {minimum: 1, maximum: 1}, capabilities: {business_executor: '1', mcp_server: '1'},
    compat:{min_cli:version,max_cli:'*'},
    files: [{path:'core-runtime.json',size:core.length,sha256:hash(core)},{path:'core-notices/CORE-SDK-LICENSE.txt',size:license.length,sha256:hash(license)}, {path: `rsrs-runtime-${target}${extension}`, size: image.length, sha256: fileHash}]};
  const bytes = new Map();
  const part = (kind, metadata, names, id) => ({repository: `risense-ai/respire-${kind}`, release_id: id, version, source_sha: sha, producer:{run_id:91,run_attempt:2},
    assets: [Buffer.from(JSON.stringify(metadata)), image, ...(kind === 'cli' ? [] : [archive])].map((value, n) => {
      const asset = {id: id * 10 + n, name: names[n], size: value.length, sha256: hash(value)};
      bytes.set(asset.id, value); return asset;
    })});
  const spec = {cli: part('cli', cli, [`cli-build-${target}.json`, cli.binary_file], 10),
    runtime: part('runtime', runtime, [`runtime-build-${target}.json`, runtime.binary_file, runtime.archive_file], 20)};
  const producer=runtimeProducerFixture(spec.runtime,target,bytes.get(spec.runtime.assets[0].id),archive);
  let corrupt = false;
  const fetcher = async url => {
    const kind = url.includes('/respire-runtime/') ? 'runtime' : 'cli', selected = spec[kind];
    if (url.includes('/jobs?') || url.includes('/artifacts?')) return producer.fetcher(url);
    if(url.includes('/contents/')) {
      const value=url.includes('test-support/runtime-test-fixture.json')?{required:version,accept:`>=${version},<2.0.0`}:url.includes('artifact-manifest.json')?{artifactLayout:'cli-frontend-v2',cliVersion:version,targets:[{triple:target}]}:{targets:{[target]:sdk}};
      const body=Buffer.from(JSON.stringify(value));
      return {status:200,json:async()=>({type:'file',encoding:'base64',size:body.length,content:body.toString('base64'),sha:createHash('sha1').update(Buffer.concat([Buffer.from(`blob ${body.length}\0`),body])).digest('hex')})};
    }
    if (url.includes('/actions/runs/')) return {status:200,json:async()=>({id:91,run_attempt:2,head_sha:sha,status:'completed',conclusion:'success',path:'.github/workflows/release.yml',repository:{full_name:selected.repository}})};
    if (url.includes('/releases/assets/')) return new Response(corrupt ? 'bad' : bytes.get(Number(url.split('/').pop())));
    if (url.includes('/git/ref/')) return {status: 200, json: async () => ({object: {type: 'commit', sha}})};
    if (url.includes('/assets?')) return {status: 200, json: async () => selected.assets.map(a => ({...a, state: 'uploaded', digest: `sha256:${a.sha256}`}))};
    return {status: 200, json: async () => ({id: selected.release_id, tag_name: `v${version}`, draft: false, published_at: '2026-11-05T00:00:00Z'})};
  };
  const options={fetcher,artifactTransport:producer.artifactTransport};
  try {
    const destination = path.join(root, 'success');
    const receipt = await verifyPublishedTarget(spec, target, destination, manifestHash, options);
    assert.equal(receipt.assets_verified, true); assert.equal(receipt.payloads_verified, true); assert.equal(receipt.manifest_sha256, manifestHash);
    assert.deepEqual(receipt.product_sources, {cli: sha, runtime: sha});
    assert.equal(receipt.harness_sha, process.env.GITHUB_SHA ?? null);
    assert.equal(receipt.workflow_sha, receipt.harness_sha);
    assert.equal(receipt.full_passed, false); assert.equal(receipt.g1Passed, false); assert.deepEqual(receipt.actual_processes, []);
    assert.equal(receipt.assets.length, 5); assert.ok(receipt.remaining.includes('actual running image digests'));
    assert.equal(receipt.runtime_producer.metadata_sha256,spec.runtime.assets[0].sha256);
    assert.equal(receipt.runtime_producer.run_attempt,2);assert.equal(receipt.runtime_producer.artifacts.length,2);
    assert.deepEqual(JSON.parse(readFileSync(path.join(destination, 'published-assets-receipt.json'))), receipt);
    const inspected=path.join(destination,'inspected/runtime'),licensePath=path.join(inspected,'core-notices/CORE-SDK-LICENSE.txt');
    const installedFiles=runtime.files.map(file=>file.path===runtime.binary_file?{...file,path:'rsrs-runtime'+extension}:file);
    verifyCorePayload(inspected,target,sdk,installedFiles);
    writeFileSync(licensePath,'tampered');assert.throws(()=>verifyCorePayload(inspected,target,sdk,installedFiles),/hash\/size/);writeFileSync(licensePath,license);
    writeFileSync(path.join(inspected,'unexpected'),'extra');assert.throws(()=>verifyCorePayload(inspected,target,sdk,installedFiles),/Runtime\/Core inventory/);rmSync(path.join(inspected,'unexpected'));
    assert.throws(()=>verifyCorePayload(inspected,target,{...sdk,sdk_version:'1.0.2-dev.22'}),/Core identity/);
    const consumed=await consumePublishedTarget(spec,target,destination,path.join(root,'nested/consumer/release'),manifestHash,{fetcher});
    assert.deepEqual(consumed.env,{RSRS_ARTIFACT_SOURCE:'published-assets',RSRS_RUNTIME_BIN:path.resolve(root,'nested/consumer/release/runtime','rsrs-runtime'+extension),RSRS_PUBLISHED_RUNTIME_SHA256:fileHash,RSRS_PUBLISHED_RUNTIME_VERSION:version,RSRS_PUBLISHED_ASSETS_SHA256:manifestHash,RSRS_PUBLISHED_BUNDLE_SHA256:archiveHash,RSRS_PUBLISHED_RUNTIME_ARCHIVE:path.resolve(destination,'runtime',runtime.archive_file),RSRS_PUBLISHED_RUNTIME_METADATA:path.resolve(destination,'runtime',`runtime-build-${target}.json`),RSRS_PUBLISHED_RUNTIME_METADATA_SHA256:hash(Buffer.from(JSON.stringify(runtime))),RSRS_RUNTIME_TRIPLE:target});
    assert.equal(hash(readFileSync(consumed.env.RSRS_RUNTIME_BIN)),fileHash);
    assert.equal(existsSync(path.join(root,'nested/consumer/release/runtime',runtime.binary_file)),false);
    assert.equal(hash(readFileSync(consumed.env.RSRS_PUBLISHED_RUNTIME_ARCHIVE)),archiveHash);
    await assert.rejects(verifyPublishedTarget(spec, target, destination, manifestHash, options), /EEXIST/);
    producer.report.run_attempt='1';
    producer.replaceZip(1,[[`runtime-smoke-${target}.json`,JSON.stringify(producer.report)]]);
    const staleProducer=path.join(root,'stale-producer');
    await assert.rejects(verifyPublishedTarget(spec,target,staleProducer,manifestHash,options),/native report\/source\/attempt\/bytes/);
    const rejectedProducer=JSON.parse(readFileSync(path.join(staleProducer,'published-assets-receipt.json')));
    assert.equal(rejectedProducer.assets_verified,false);assert.equal(rejectedProducer.full_passed,false);
    corrupt = true;
    const failed = path.join(root, 'failed');
    await assert.rejects(verifyPublishedTarget(spec, target, failed, manifestHash, options), /hash\/size/);
    const failure = JSON.parse(readFileSync(path.join(failed, 'published-assets-receipt.json')));
    assert.equal(failure.assets_verified, false); assert.equal(failure.full_passed, false);
    assert.match(failure.error, /hash\/size/); assert.deepEqual(failure.actual_processes, []);
  } finally { rmSync(root, {recursive: true, force: true}); }
  }
});

function syntheticTar(entries) {
  const blocks=[];
  for(const [name,content,type='0'] of entries) {
    const bytes=Buffer.from(content), header=Buffer.alloc(512);
    header.write(name,0,100); header.write('0000755\0',100); header.write('0000000\0',108); header.write('0000000\0',116);
    header.write(bytes.length.toString(8).padStart(11,'0')+'\0',124); header.write('00000000000\0',136);
    header.fill(32,148,156); header.write(type,156); header.write('ustar\0',257);
    const checksum=[...header].reduce((a,b)=>a+b,0); header.write(checksum.toString(8).padStart(6,'0')+'\0 ',148);
    blocks.push(header,bytes,Buffer.alloc((512-bytes.length%512)%512));
  }
  return gzipSync(Buffer.concat([...blocks,Buffer.alloc(1024)]));
}
test('published archive extractor refuses links, traversal, duplicates, bad checksums and truncated payloads',async()=>{
  const root=mkdtempSync(path.join(tmpdir(),'published-tar-test-'));
  try {
    const good=path.join(root,'good.tgz');writeFileSync(good,syntheticTar([['nested/file',Buffer.from('value')]]));
    await extractPublishedArchive(good,path.join(root,'good'));assert.equal(readFileSync(path.join(root,'good/nested/file'),'utf8'),'value');
    let i=0;
    for(const entries of [[['../escape','x']],[['/absolute','x']],[['dir/link','', '2']],[['hard','', '1']],[['device','', '3']],[['file','a'],['FILE','b']],[['dir/CON.txt','x']],[['dir./file','x']]]) {
      const file=path.join(root,`bad-${i}.tgz`);writeFileSync(file,syntheticTar(entries));await assert.rejects(extractPublishedArchive(file,path.join(root,`out-${i++}`)));
    }
    const damaged=gunzipSync(readFileSync(good));damaged[0]^=1;writeFileSync(path.join(root,'checksum.tgz'),gzipSync(damaged));
    await assert.rejects(extractPublishedArchive(path.join(root,'checksum.tgz'),path.join(root,'checksum')),/checksum/);
    const long='nested/'+ 'a'.repeat(120),extended=path.join(root,'long.tgz');
    writeFileSync(extended,syntheticTar([['././@LongLink',long+'\0','L'],['placeholder','long value']]));
    await extractPublishedArchive(extended,path.join(root,'long'));assert.equal(readFileSync(path.join(root,'long',long),'utf8'),'long value');
    writeFileSync(path.join(root,'truncated.tgz'),readFileSync(good).subarray(0,25));
    await assert.rejects(extractPublishedArchive(path.join(root,'truncated.tgz'),path.join(root,'truncated')));
  } finally {rmSync(root,{recursive:true,force:true});}
});
import {publishedFullFixture} from './full-report-fixture.mjs';
test('published full joins every existing report and live image identity; copied summaries and partial jobs cannot pass',()=>{
  const root=mkdtempSync(path.join(tmpdir(),'published-full-join-'));
  try {
    const f=publishedFullFixture(root),run=()=>validatePublishedFull(f.spec,f.targets,f.manifestHash,root,f.jobs,f.context,{client2:{release:'v2.0.0-dev.6',source_sha:'f'.repeat(40)}});
    const complete=run();assert.equal(complete.full_passed,true);assert.equal(complete.harness_sha,f.context.sha);
    assert.equal(complete.schema,2);assert.equal(complete.scope,'database-in-place-upgrade');
    assert.deepEqual(complete.profile_import,{required:false,status:'not-run',diagnostic_workflow:'.github/workflows/migration-keyring-check.yml'});
    assert(!complete.reports.some(r=>r.path.endsWith('migration-coverage.json')||r.path.endsWith('migration-platform-gate.json')));
    const linux=f.targets.find(t=>t.includes('linux')&&t!=='x86_64-unknown-linux-gnu');
    const combinedFile=path.join(root,'release-sweep-'+linux,'combined-coverage.json'),combinedBytes=readFileSync(combinedFile);
    for(const change of [r=>r.schema=1,r=>delete r.scope,r=>r.profile_import.status='passed',r=>r.profile_import.status='failed']){
      const old=JSON.parse(combinedBytes);change(old);writeFileSync(combinedFile,JSON.stringify(old));assert.throws(run);
    }
    writeFileSync(combinedFile,combinedBytes);
    assert.notEqual(complete.harness_sha,complete.product_sources.cli);
    assert.equal(complete.actual_processes.length,f.targets.length+Object.values(f.upgradeReports).reduce((count,r)=>count+r.sources.filter(row=>row.passed).length,0)+2*f.targets.filter(t=>t.includes('linux')&&t!=='x86_64-unknown-linux-gnu').length);
    assert.equal(complete.historical_upgrades.length,f.targets.length);
    const guiFile=path.join(root,'client2-published-online-x86_64-pc-windows-msvc','report.json'),guiBytes=readFileSync(guiFile);
    rmSync(guiFile);
    const independent=validatePublishedFull(f.spec,f.targets,f.manifestHash,root,f.jobs,f.context,{independent:true});
    assert.equal(independent.full_passed,true);
    assert.equal(independent.gui_passed,null);
    assert.equal(independent.historical_upgrades.length,f.targets.length);
    assert.throws(run);
    writeFileSync(guiFile,guiBytes);
    const selected={release:'v2.0.0-dev.6',source_sha:'f'.repeat(40),
      sealed:{run_id:102,run_attempt:2,artifact_id:103,artifact_zip_digest:'sha256:'+'b'.repeat(64)}};
    const sealedGui=JSON.parse(guiBytes);
    Object.assign(sealedGui,{official_exe_verified:false,source_candidate_verified:true,candidate_inputs:sealedGui.published_inputs,
      sealed_client2:{...selected.sealed,repository:'risense-ai/respire-client2',source_sha:selected.source_sha,
        version:selected.release.slice(1),manifest_sha256:'c'.repeat(64),exe_sha256:sealedGui.exe_sha256,six_package_receipts_verified:true}});
    const checkSeal=(report, candidate=true)=>validateFullGUI(report,f.spec,f.manifestHash,{candidate,client2:selected});
    assert.equal(checkSeal(sealedGui),sealedGui);
    assert.throws(()=>checkSeal(sealedGui,false),/seal differs/);
    for(const mutate of [g=>g.sealed_client2.run_attempt++,g=>g.sealed_client2.artifact_id++,
      g=>g.sealed_client2.artifact_zip_digest='sha256:'+'0'.repeat(64),g=>g.sealed_client2.six_package_receipts_verified=false,
      g=>g.sealed_client2.exe_sha256='0'.repeat(64),g=>g.restart_native_gui.offline.automatic_key_handoff=false,
      g=>g.cli_processes.pop(),g=>g.cleanup_exit_confirmed=false]){
      const altered=copy(sealedGui);mutate(altered);assert.throws(()=>checkSeal(altered));
    }
    for(const mutate of [g=>delete g.cli_processes,g=>g.cli_processes.pop(),g=>g.cli_processes[0].sha256='0'.repeat(64),
      g=>g.cli_processes[0].parent_verified=false,g=>g.cli_processes[0].lifetime_verified=false,g=>g.source_sha='0'.repeat(40),
      g=>g.release='v2.0.0-dev.5',g=>g.restart_native_gui.offline.automatic_key_handoff=false,g=>g.cleanup_error='observer failed']){
      const gui=JSON.parse(guiBytes);mutate(gui);writeFileSync(guiFile,JSON.stringify(gui));assert.throws(run);
    }
    const originalGui=JSON.parse(guiBytes);
    originalGui.cli_processes.unshift(null);
    writeFileSync(guiFile,JSON.stringify(originalGui));
    assert.equal(run().full_passed,true);
    originalGui.cli_processes.pop();
    writeFileSync(guiFile,JSON.stringify(originalGui));
    assert.throws(run,/GUI did not observe the actual CLI image/);
    writeFileSync(guiFile,guiBytes);
    assert(complete.historical_upgrades.every(r=>r.profile==='full'&&r.all_published_sources_covered));
    for(const target of f.targets) {
      const critical=copy(f.upgradeReports[target]);
      critical.profile='critical';critical.full_coverage=critical.all_published_sources_covered=false;
      critical.selected_sources=critical.selected_sources.filter(tag=>tag!=='v1.0.19');
      critical.sources=critical.sources.filter(row=>row.version!=='1.0.19');
      f.put('published-upgrades-'+target,'published-upgrades.json',critical);
    }
    const critical=run();assert.equal(critical.full_passed,false);
    assert(critical.historical_upgrades.every(r=>r.profile==='critical'&&!r.all_published_sources_covered));
    for(const target of f.targets)f.put('published-upgrades-'+target,'published-upgrades.json',f.upgradeReports[target]);
    for(const change of [j=>j.pop(),j=>j.push(j[0]),j=>j[0].conclusion='skipped',j=>j[0].conclusion='cancelled',j=>j[0].conclusion='failure',j=>j[0].status='in_progress',j=>j[0].run_attempt=1,j=>j[0].head_sha=f.spec.cli.source_sha,j=>j[0].run_id=900]) {
      const jobs=copy(f.jobs);change(jobs);assert.throws(()=>validatePublishedFull(f.spec,f.targets,f.manifestHash,root,jobs,f.context));
    }
    const first=f.targets[0],artifact='resident-concurrency-'+first,file='real-account-return-verification.json';
    const checkpoint=Object.keys(f.upgradeReports[first].database_dev_checkpoints)[0];
    for(const change of [r=>r.passed=false,r=>r.profile='unknown',r=>r.upgrade_contract='legacy-shim-v1',
      r=>r.full_coverage=false,r=>r.all_published_sources_covered=false,r=>r.profile='critical',
      r=>r.candidate_sha256='f'.repeat(64),r=>r.target='other',r=>r.published_formal_sources.push(r.published_formal_sources[0]),
      r=>delete r.database_dev_checkpoints[checkpoint],r=>r.database_dev_checkpoints[checkpoint]='unapproved policy',
      r=>{r.selected_sources=r.selected_sources.filter(tag=>tag!==checkpoint);r.sources=r.sources.filter(row=>'v'+row.version!==checkpoint);},
      r=>{r.selected_sources=r.selected_sources.filter(tag=>tag!=='v1.0.19');r.sources=r.sources.filter(row=>row.version!=='1.0.19');},
      r=>r.sources.pop(),r=>r.sources.push(r.sources[0]),r=>r.sources[0].passed=false,
      r=>r.sources[0].upgrade_contract='legacy-shim-v1',r=>r.sources[0].formal=!r.sources[0].formal,
      r=>{delete r.sources[0].passed;r.sources[0].status='not-applicable';r.sources[0].reason='not available';}]) {
      const changed=copy(f.upgradeReports[first]);change(changed);
      f.put('published-upgrades-'+first,'published-upgrades.json',changed);assert.throws(run);
    }
    f.put('published-upgrades-'+first,'published-upgrades.json',f.upgradeReports[first]);
    for(const source of f.upgradeReports[first].sources) {
      const changed=copy(f.upgradeReports[first]);changed.sources.find(row=>row.version===source.version).passed=false;
      f.put('published-upgrades-'+first,'published-upgrades.json',changed);assert.throws(run,/Failed historical upgrade/);
    }
    f.put('published-upgrades-'+first,'published-upgrades.json',f.upgradeReports[first]);
    for(const source of f.upgradeReports[first].sources) {
      const upgradeFile='v'+source.version+'/results/real-account-return-verification.json';
      for(const change of [r=>delete r.runtime_installed_after_legacy_data,r=>r.runtime_installed_after_legacy_data=false,
        r=>r.runtime_installed_after_legacy_data='true',r=>delete r.legacy_handoff_actor,r=>r.legacy_handoff_actor='product',
        r=>r.legacy_handoff_actor='fixture',
        r=>r.upgrade_contract='legacy-shim-v1',r=>r.passed=false,r=>r.runtime_binary_sha256='f'.repeat(64)]) {
        const changed=copy(f.upgradedRuntime);change(changed);
        f.put('published-upgrades-'+first,upgradeFile,[changed]);assert.throws(run);
      }
      f.put('published-upgrades-'+first,upgradeFile,[f.upgradedRuntime]);
    }
    const intel=f.targets.find(target=>target==='x86_64-apple-darwin');
    const intelReport=copy(f.upgradeReports[intel]);
    const gap=intelReport.sources.find(row=>row.status==='not-applicable');
    gap.reason='invented gap';f.put('published-upgrades-'+intel,'published-upgrades.json',intelReport);
    assert.throws(run,/Unapproved historical platform gap/);
    f.put('published-upgrades-'+intel,'published-upgrades.json',f.upgradeReports[intel]);
    for(const profile of ['full','critical']) {
      const firstPlatform=copy(f.upgradeReports[intel]);
      firstPlatform.profile=profile;firstPlatform.full_coverage=firstPlatform.all_published_sources_covered=profile==='full';
      firstPlatform.published_formal_sources=['v1.0.19'];firstPlatform.first_platform_release=true;
      firstPlatform.upgrade_status='not-applicable: first Intel macOS release; fresh inference is validated separately';
      firstPlatform.selected_sources=firstPlatform.selected_sources.filter(tag=>tag!=='v1.0.20'&&(profile==='full'||tag!=='v1.0.19'));
      firstPlatform.sources=firstPlatform.sources.filter(row=>firstPlatform.selected_sources.includes('v'+row.version));
      f.put('published-upgrades-'+intel,'published-upgrades.json',firstPlatform);
      const joined=run();assert.equal(joined.full_passed,profile==='full');
      assert.equal(joined.historical_upgrades.find(r=>r.target===intel).passed.length,0);
      firstPlatform.first_platform_release=false;f.put('published-upgrades-'+intel,'published-upgrades.json',firstPlatform);
      assert.throws(run,/No actual applicable upgrade/);
    }
    f.put('published-upgrades-'+intel,'published-upgrades.json',f.upgradeReports[intel]);
    const producerReceipt=path.join(root,'published-assets-verification-'+first,'published-assets-receipt.json');
    const originalReceipt=readFileSync(producerReceipt);
    for(const kind of ['cli','runtime'])for(const field of ['run_id','run_attempt']){
      const changed=JSON.parse(originalReceipt);changed.releases[kind].producer[field]++;
      writeFileSync(producerReceipt,JSON.stringify(changed));assert.throws(run,/producer identity/);
    }
    for(const change of [r=>delete r.runtime_producer,r=>r.runtime_producer.run_attempt++,r=>r.runtime_producer.run_id++,
      r=>r.runtime_producer.source_sha='f'.repeat(40),r=>r.runtime_producer.target='other',r=>r.runtime_producer.metadata_sha256='f'.repeat(64),
      r=>r.runtime_producer.archive_sha256='f'.repeat(64),r=>r.runtime_producer.binary_sha256='f'.repeat(64),r=>r.runtime_producer.artifacts.pop()]) {
      const changed=JSON.parse(originalReceipt);change(changed);writeFileSync(producerReceipt,JSON.stringify(changed));
      assert.throws(run,/Runtime producer byte binding/);
    }
    writeFileSync(producerReceipt,originalReceipt);
    for(const change of [r=>r.passed=false,r=>r.runtime_mode='explicit_cli_shim',r=>r.standalone_runtime_evidence=false,r=>r.runtime_binary_sha256='c'.repeat(64),r=>r.runtime_version='1.0.21-dev.3',r=>r.runtime_identity_receipts=[],r=>r.runtime_identity_receipts[0].pid=0,r=>delete r.runtime_identity_receipts[0].process_start_id,r=>r.runtime_identity_receipts[0].protocol_v=2,r=>delete r.runtime_identity_receipts[0].account_id,r=>r.runtime_identity_receipts[0].image_sha256='c'.repeat(64),r=>r.runtime_identity_receipts[0].bundle_sha256='c'.repeat(64),r=>r.runtime_identity_receipts[0].manifest_sha256='c'.repeat(64)]) {
      const row=copy(f.runtime);change(row);f.put(artifact,file,[row]);assert.throws(run);f.put(artifact,file,[f.runtime]);
    }
    for(const [folder,name] of [['release-sweep-'+f.targets.find(t=>t.includes('linux')&&t!==first),'outbox-coverage.json'],...f.upgradeReports[first].sources.map(row=>['published-upgrades-'+first,'v'+row.version+'/results/real-account-return-verification.json']),['dev-api-coverage','published-api-identity.json'],['published-assets-verification-'+first,'published-assets-receipt.json']]) {
      const location=path.join(root,folder,name),bytes=readFileSync(location);rmSync(location);assert.throws(run);writeFileSync(location,bytes);
    }
    assert.equal(run().full_passed,true);
  } finally {rmSync(root,{recursive:true,force:true});}
});

test('published pack adapter retains version, repository, launcher, optional dependencies and SDK license assertions',()=>{
  const manifest={cliVersion:version,npmScope:'rsrs-cli',targets:[{pkg:'@rsrsai/linux-x64'}]};
  const launcher={name:manifest.npmScope,version,repository:{url:'https://github.com/risense-ai/respire-cli.git'},license:'SEE LICENSE IN LICENSE',bin:{rsrs:'./bin/cli.js'},optionalDependencies:{'@rsrsai/linux-x64':version}};
  const native={name:'@rsrsai/linux-x64',version,repository:launcher.repository,license:launcher.license};
  const files=new Set(['LICENSE','bin/core-notices/CORE-SDK-LICENSE.txt']),core={redistribution:'permitted-under-included-license'};
  validatePublishedPackage(launcher,manifest,launcher.name,null,files);validatePublishedPackage(native,manifest,native.name,core,files);
  for(const mutate of [p=>p.name='other',p=>p.version='1.0.0',p=>p.repository.url='https://foreign/repo.git',p=>p.dependencies={},p=>p.bin={other:'./bin/cli.js'},p=>p.optionalDependencies={},p=>p.license='MIT']) {
    const pkg=copy(launcher);mutate(pkg);assert.throws(()=>validatePublishedPackage(pkg,manifest,launcher.name,null,files));
  }
  for(const bad of [new Set(),new Set(['LICENSE','LICENSE-MIT-LEGACY']),new Set(['LICENSE'])])assert.throws(()=>validatePublishedPackage(native,manifest,native.name,core,bad));
  assert.throws(()=>validatePublishedPackage(native,manifest,native.name,{redistribution:'forbidden'},files));
  const frontend={...manifest,artifactLayout:'cli-frontend-v2'},frontendFiles=new Set(['LICENSE','bin/rsrs']);
  validatePublishedPackage(native,frontend,native.name,null,frontendFiles);
  validatePublishedPackage(launcher,frontend,launcher.name,null,new Set(['LICENSE','bin/cli.js']));
  assert.throws(()=>validatePublishedPackage(native,frontend,native.name,core,frontendFiles));
  assert.throws(()=>validatePublishedPackage(native,frontend,native.name,null,files));
  assert.throws(()=>validatePublishedPackage(native,frontend,native.name,null,new Set(['bin/rsrs'])));
});

test('official Runtime CI fixture retries only exact pinned metadata or paired archive network failures', async () => {
  const pin = JSON.parse(readFileSync(new URL('../../test-support/runtime-test-fixture.json', import.meta.url), 'utf8'));
  const target = 'x86_64-unknown-linux-gnu';
  const url = `https://github.com/risense-ai/respire-runtime/releases/download/v${pin.required}/runtime-build-${target}.json`;
  const prefix = `release metadata request failed (${url}): ${url}: `;
  const envelope = error => ({command: 'cli', status: 'fail', summary: {reason: 'runtime_error'},
    items: [], actions: [], errors: [error], details: {error_type: 'runtime'}, related: []});
  const result = doc => ({status: 1, signal: null, stdout: Buffer.from(JSON.stringify(doc)), stderr: Buffer.from('first attempt diagnostics\n')});
  const readTimeout = 'Network Error: Network Error: Error encountered in the status line: timed out reading response: Network Error: Error encountered in the status line: timed out reading response: timed out reading response';
  const win = 'A connection attempt failed because the connected party did not properly respond after a period of time, or established connection failed because connected host has failed to respond. (os error 10060)';
  const connectTimeout = `Network Error: Network Error: Error encountered in the status line: ${win}: Network Error: Error encountered in the status line: ${win}: ${win}`;
  const exercise = async (first, expectedCalls, second = {status: 0, stdout: Buffer.from('{}'), stderr: Buffer.alloc(0)}) => {
    let calls = 0; const waits = [], output = [], errors = []; const env = {RSRS_DATA_DIR: 'isolated-fixture'};
    const actual = await installPinnedRuntime('fixture-cli', {env}, target, {
      spawn(cli, args, options) {
        calls++; assert.equal(cli, 'fixture-cli'); assert.deepEqual(args, ['--json', 'runtime', 'install', '--version', pin.required]);
        assert.equal(options.env, env); assert.deepEqual(options.stdio, ['inherit', 'pipe', 'pipe']);
        return calls === 1 ? first : second;
      },
      pause: async ms => { waits.push(ms); },
      stdout: {write: bytes => output.push(Buffer.from(bytes))},
      stderr: {write: bytes => errors.push(Buffer.from(bytes))},
    });
    assert.equal(calls, expectedCalls); assert.deepEqual(waits, expectedCalls === 2 ? [2000] : []);
    assert.equal(actual, expectedCalls === 2 ? second : first);
    assert.deepEqual(output[0], first.stdout); assert.deepEqual(errors[0], first.stderr);
  };
  for (const reason of ['status code 429', 'status code 502', 'status code 503', 'status code 504', readTimeout, connectTimeout]) {
    await exercise(result(envelope(prefix + reason)), 2);
  }
  const first = result(envelope(prefix + 'status code 503'));
  await exercise(first, 2, first); // A second transient failure must stop after two attempts.
  const archive = url.replace(/\/runtime-build-([^/]+)\.json$/, '/rsrs-runtime-$1.tar.gz');
  const originFailure = (origin, reason) => {
    const asset = archive.replace('https://github.com', origin);
    return `${origin}: download failed (${asset}): ${asset}: ${reason}`;
  };
  const pair = [originFailure('https://github.com', 'status code 502'), originFailure('https://gh-proxy.com/https://github.com', readTimeout)];
  const aggregate = entries => 'runtime download failed: ' + entries.join('; ');
  await exercise(result(envelope(aggregate(pair))), 2);
  for (const entries of [[pair[0]], [...pair].reverse(), [...pair, pair[0]],
    [pair[0], pair[1].replace(readTimeout, 'archive checksum failed')],
    [pair[0].replace(`v${pin.required}/`, 'v0.0.0/'), pair[1]],
    [pair[0], pair[1].replace(target, 'aarch64-unknown-linux-gnu')],
    [pair[0], pair[1].replace(readTimeout, 'Permission denied (os error 13)')]]) await exercise(result(envelope(aggregate(entries))), 1);
  for (const error of [prefix + 'status code 401', prefix + 'status code 403', prefix + 'status code 404',
    prefix + 'status code 500', prefix + 'status code 503; archive checksum failed',
    prefix.replace('https://github.com/', 'https://evil.invalid/') + 'status code 503',
    prefix.replace(`v${pin.required}/`, 'v0.0.0/') + 'status code 503',
    prefix.replace(target, 'aarch64-unknown-linux-gnu') + 'status code 503',
    'runtime download failed: https://github.com: archive checksum failed; https://gh-proxy.com: status code 503',
    'invalid runtime release metadata', 'archive checksum failed', 'Permission denied (os error 13)']) {
    await exercise(result(envelope(error)), 1);
  }
  for (const mutate of [d => d.errors.push(prefix + 'status code 503'), d => d.details = {},
    d => d.summary.reason = 'other', d => d.command = 'runtime', d => d.status = 'ok',
    d => d.items.push({error: 'schema'}), d => d.schema = 2]) {
    const doc = envelope(prefix + 'status code 503'); mutate(doc); await exercise(result(doc), 1);
  }
  await exercise({...first, stdout: Buffer.from('not JSON')}, 1);
  await exercise({...first, error: new Error('spawn failed')}, 1);
  await exercise({...first, signal: 'SIGTERM'}, 1);
  await exercise({...first, status: 2}, 1);
  await exercise({...first, status: 0}, 1);
});
