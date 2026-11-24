

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

const m = JSON.parse(readFileSync(new URL('../artifact-manifest.json', import.meta.url))); m.npmScope = '@rsrsai/cli'; // Historical receipt layout.
delete m.artifactLayout;
const integrity = 'sha512-' + createHash('sha512').update('synthetic package').digest('base64');
const hash = 'a'.repeat(64), sha = 'b'.repeat(40), version = '1.2.3-dev.4';
const source = {repository: 'risense-ai/respire-cli', workflow: '.github/workflows/release.yml', run_id: '100', run_attempt: '2', sha, version, channel: 'dev', release_id: 300};
const sourceRun = {id: 100, run_attempt: 2, path: source.workflow, head_repository: {full_name: source.repository}, event: 'workflow_dispatch', head_sha: 'c'.repeat(40), status: 'in_progress'};
const scoped = {schema: 1, kind: 'scoped-ready', source,
  packages: [...m.targets.map(t => t.pkg), m.npmScope].map(name => ({name, version, integrity, shasum: 'd'.repeat(40)})),
  binaries: m.targets.map(t => ({package: t.pkg, target: t.triple, file: `rsrs-${t.triple}${t.triple.includes('windows') ? '.exe' : ''}`, sha256: hash})),
  assets: m.targets.flatMap(t => [`cli-build-${t.triple}.json`, `rsrs-${t.triple}-runtime.tar.gz`, `rsrs-${t.triple}${t.triple.includes('windows') ? '.exe' : ''}`]).sort().map(name => ({name, size: 42, sha256: hash})),
};
const aliasRun = {id: 400, run_attempt: 3, path: '.github/workflows/npm-alias.yml', event: 'workflow_run', head_repository: {full_name: source.repository}, status: 'completed', conclusion: 'success'};
const alias = {schema: 1, kind: 'alias-ready', source, scoped_packages: scoped.packages,
  alias: {name: 'rsrs-cli', version, integrity, shasum: 'd'.repeat(40)},
  publisher: {workflow: aliasRun.path, run_id: '400', run_attempt: '3'},
  install: {platform: 'linux-x64-gnu', version, binary_sha256: hash}};
const jobs = [{name: 'publish-integrated', conclusion: 'success'}];
const release = {id: 300, draft: true, tag_name: `v${version}`, target_commitish: sha, prerelease: true,
  assets: scoped.assets.map(a => ({name: a.name, size: a.size, state: 'uploaded', digest: `sha256:${a.sha256}`}))};
const copy = value => structuredClone(value);

export { alias, aliasRun, copy, hash, jobs, m, release, scoped, sha, source, sourceRun, version };
