const { i18nMessages, readFrontend } = require('./frontend-source.cjs');
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const root = path.resolve(__dirname, '..');
const read = readFrontend;
const source = read('client.js');
const section = source.slice(source.indexOf('function setRefreshText('), source.indexOf('\nall("nav [data-page]")', source.indexOf('async function refresh()')));
assert.match(section, /async function refresh\(\)/);
assert.equal((section.match(/setRefreshText\(\$\(/g) || []).length, 6);
// Execute the production helper and refresh body, not a rewritten algorithm.
// This deliberately models text-node replacement, not a browser or Windows UIA.
class Element {
  constructor() { this.node = { nodeValue: '' }; this.writes = 0; this.classList = { toggle() {} }; }
  get textContent() { return this.node.nodeValue; }
  set textContent(value) { this.writes++; this.node = { nodeValue: value == null ? '' : `${value}` }; }
}
const elements = Object.fromEntries(['connection-dot', 'connection-label', 'client-version', 'client-platform', 'nav-memory-count', 'status-summary', 'sync-summary'].map(id => [`#${id}`, new Element()]));
const document = {
  documentElement: {}, body: { querySelectorAll: () => [] },
  createTreeWalker() {
    const nodes = Object.values(elements).map(e => e.node); let index = 0;
    return { nextNode() { this.currentNode = nodes[index++]; return !!this.currentNode; } };
  },
};
const i18n = new Function('document', 'NodeFilter', 'localStorage', read('ui/i18n.js').replace(/^export /gm, '') + '\nreturn {t, currentLanguage, setLanguage, applyTranslations};')(document, { SHOW_TEXT: 4 }, { setItem() {} });
const core = new Function(read('ui/core.js').replace(/^export /gm, '') + '\nreturn {number};')();
let desktop, status, failure, stateFailure, stateWait;
const events = [], notices = [];
const ctx = {};
const bindings = {
  ctx, i18nMessages, $: selector => elements[selector], ...i18n, ...core,
  async api(name) {
    events.push(name);
    if (name === 'state') { if (stateFailure) throw Error('state failed'); if (stateWait) await stateWait; return desktop; }
    assert.equal(name, 'runtime-status');
    if (failure) throw Error('status failed'); return status;
  },
  appearance() { events.push('appearance'); },
  view: { update() { events.push('view.update'); } },
  refreshNavCounts() { events.push('nav'); },
  notify(...args) { notices.push(args); },
};
const { refresh, setRefreshText } = new Function(...Object.keys(bindings), 'let refreshing = false;\n' + section + '\nreturn {refresh, setRefreshText};')(...Object.values(bindings));
const fields = Object.values(elements).slice(1);
const nodes = () => fields.map(e => e.node);
const writes = () => fields.map(e => e.writes);
let count = 0;
async function test(name, fn) { await fn(); console.log(`PASS ${++count}: ${name}`); }
(async () => {
  desktop = { connected: true, version: '2.0.0-dev.test', platform: 'windows/amd64', health: { bin: 'fixture', account_id: 'fixture-account', instance_id: 'fixture-instance' } };
  status = { summary: { local_alive: 12, workspace: 'normal', sync_live: { phase: 'idle', pending: 0, conflicts: 0 } } };
  await test('initial English values and existing call order', async () => {
    await refresh();
    assert.deepEqual(events, ['state', 'appearance', 'runtime-status', 'view.update', 'nav']);
    assert.equal(elements['#connection-label'].textContent, 'Runtime connected');
    assert.equal(elements['#client-platform'].textContent, 'windows/amd64');
    assert.equal(elements['#nav-memory-count'].textContent, '12');
    assert.equal(elements['#status-summary'].textContent, '12 memories · normal · Runtime fixture');
    assert.equal(elements['#sync-summary'].textContent, 'Sync idle · pending 0 · conflicts 0');
  });
  await test('100 repeated polls preserve nodes without suppressing view updates', async () => {
    const before = nodes(), counts = writes(); events.length = 0;
    for (let n = 0; n < 100; n++) { await refresh(); i18n.applyTranslations(); }
    assert.deepEqual(nodes(), before); fields.forEach((e, n) => assert.equal(e.node, before[n]));
    assert.deepEqual(writes(), counts);
    assert.equal(events.filter(e => e === 'view.update').length, 100);
  });
  await test('real status count version platform and sync changes propagate', async () => {
    desktop.version = '2.0.0-dev.next'; desktop.platform = 'linux/amd64';
    status.summary.local_alive = 13; status.summary.workspace = 'readonly'; status.summary.sync_live.pending = 2;
    await refresh();
    assert.equal(elements['#client-version'].textContent, 'Client 2.0.0-dev.next');
    assert.equal(elements['#client-platform'].textContent, 'linux/amd64');
    assert.equal(elements['#nav-memory-count'].textContent, '13');
    assert.equal(elements['#status-summary'].textContent, '13 memories · readonly · Runtime fixture');
    assert.equal(elements['#sync-summary'].textContent, 'Sync idle · pending 2 · conflicts 0');
  });
  await test('Chinese language change and repeated poll', async () => {
    i18n.setLanguage('zh'); await refresh();
    assert.equal(elements['#connection-label'].textContent, i18nMessages.m3053ee2f4c18204a);
    assert.equal(elements['#client-version'].textContent, i18nMessages.m4f58b9bfa643d5ab);
    assert.match(elements['#status-summary'].textContent, new RegExp(i18nMessages.m7a00e8e0588e365e,""));
    assert.equal(elements['#sync-summary'].textContent, i18nMessages.ma28aa35ceaf93996);
    const before = writes(); await refresh(); assert.deepEqual(writes(), before);
  });
  await test('English language change restores dynamic labels', async () => {
    i18n.setLanguage('en'); await refresh();
    assert.equal(elements['#client-version'].textContent, 'Client 2.0.0-dev.next');
    assert.match(elements['#status-summary'].textContent, /^13 memories/);
  });
  await test('external DOM drift repaired despite unchanged backend', async () => {
    const expected = fields.map(e => e.textContent);
    fields.forEach(e => { e.textContent = 'external drift'; }); await refresh();
    assert.deepEqual(fields.map(e => e.textContent), expected);
  });
  await test('DOM coercion and nullish platform values remain equivalent', async () => {
    for (const value of [null, undefined, 0, 7, false, true, '', ['x'], { toString: () => 'fixture' }]) {
      const e = new Element(); setRefreshText(e, value);
      assert.equal(e.textContent, value == null ? '' : `${value}`);
      const before = e.writes; setRefreshText(e, value); assert.equal(e.writes, before);
    }
    for (const value of [null, undefined, 0, false]) { desktop.platform = value; await refresh(); assert.equal(elements['#client-platform'].textContent, value == null ? '' : `${value}`); }
    assert.throws(() => setRefreshText(new Element(), Symbol('invalid DOMString')), TypeError);
  });
  await test('missing element still throws instead of hiding selector failures', async () => {
    assert.throws(() => setRefreshText(null, 'value'), TypeError);
  });
  await test('same-scope status failure keeps the last count and shows a translated error until recovery', async () => {
    failure = true; await refresh(); assert.equal(ctx.statusError, 'status failed');
    assert.equal(ctx.status, status);
    assert.equal(elements['#nav-memory-count'].textContent, '13');
    assert.equal(elements['#sync-summary'].textContent, i18nMessages.m23dfe4404e8a5ebe);
    i18n.applyTranslations(); assert.equal(elements['#sync-summary'].textContent, 'Status read failed');
    // Known existing English-error rewrite is intentionally not changed here.
    const before = elements['#sync-summary'].writes; await refresh();
    assert.equal(elements['#sync-summary'].writes, before + 1);
    i18n.applyTranslations(); assert.equal(elements['#sync-summary'].textContent, 'Status read failed');
    failure = false; await refresh(); assert.equal(ctx.statusError, '');
    assert.equal(elements['#sync-summary'].textContent, 'Sync idle · pending 2 · conflicts 0');
  });
  await test('failed status refresh after an instance change clears the previous status', async () => {
    desktop.health.instance_id = 'new-instance'; failure = true; await refresh();
    assert.equal(ctx.status, null);
    assert.equal(ctx.statusScope, '');
    assert.equal(ctx.statusError, 'status failed');
    assert.equal(elements['#nav-memory-count'].textContent, '—');
    failure = false; await refresh(); assert.equal(ctx.status, status);
  });
  await test('disconnect skips runtime request and reconnect updates', async () => {
    desktop.connected = false; events.length = 0; await refresh();
    assert.deepEqual(events, ['state', 'appearance', 'view.update', 'nav']);
    assert.equal(elements['#connection-label'].textContent, 'Runtime disconnected');
    assert.equal(elements['#status-summary'].textContent, 'Runtime disconnected');
    assert.equal(ctx.status, null);
    assert.equal(ctx.statusScope, '');
    assert.equal(elements['#nav-memory-count'].textContent, '—');
    desktop.connected = true; await refresh(); assert.equal(elements['#connection-label'].textContent, 'Runtime connected');
  });
  await test('Overview shows status errors next to the metrics', async () => {
    const overview = fs.readFileSync(path.join(root, 'internal/desktop/frontend/pages/overview.js'), 'utf8');
    assert.match(overview, /<div data-overview-summary>[\s\S]*?<div class="metrics" data-overview-metrics><\/div>'\s*\+\s*'<div data-overview-status-error><\/div><\/div>'/);
    assert.match(overview, /const statusErrorBanner = \$\("\[data-overview-status-error\]", root\)/);
    assert.match(overview, /statusErrorBanner\.innerHTML = ctx\.statusError\s*\? `\$\{i18nMessages\.m0bb307125afa6949\}\$\{h\(ctx\.statusError\)\}<\/p>`\s*: ""/);
    assert.doesNotMatch(overview, /onboarding\.insertAdjacentHTML/);
  });
  await test('Overview keeps one localized connection indicator through disconnect and reconnect', async () => {
    const overview = read('pages/overview.js');
    assert.match(overview, /<span class="badge" role="status" data-runtime-connection>/);
    assert.match(overview, /updateConnectionIndicator\(connection, state\.connected\)/);
    const start = overview.indexOf('export function updateConnectionIndicator(');
    const updateIndicator = new Function('t', overview.slice(start + 'export '.length,
      overview.indexOf('export function overview(', start)) + '\nreturn updateConnectionIndicator;')(i18n.t);
    for (const lang of ['en', 'zh']) {
      i18n.setLanguage(lang);
      const indicator = new Element();
      const connected = lang === 'en' ? 'Runtime connected' : i18nMessages.m3053ee2f4c18204a;
      const disconnected = lang === 'en' ? 'Runtime disconnected' : i18nMessages.mcef1537a814bdc68;
      updateIndicator(indicator, undefined);
      assert.equal(indicator.textContent, disconnected);
      updateIndicator(indicator, true); assert.equal(indicator.textContent, connected);
      const node = indicator.node;
      updateIndicator(indicator, true); assert.equal(indicator.node, node);
      updateIndicator(indicator, false); assert.equal(indicator.textContent, disconnected);
      updateIndicator(indicator, true); assert.equal(indicator.textContent, connected);
    }
    i18n.setLanguage('en');
  });
  await test('state failure notification and next refresh recovery', async () => {
    stateFailure = true; await refresh(); assert.deepEqual(notices.pop(), ['state failed', true]);
    stateFailure = false; events.length = 0; await refresh(); assert.ok(events.includes('view.update'));
  });
  await test('overlapping refresh still guarded', async () => {
    let release; stateWait = new Promise(resolve => { release = resolve; }); events.length = 0;
    const pending = refresh(); await refresh(); assert.deepEqual(events, ['state']);
    release(); await pending; stateWait = null;
    assert.deepEqual(events, ['state', 'appearance', 'runtime-status', 'view.update', 'nav']);
  });
  await test('core configuration notices require the original applied receipt', async () => {
    const models = read('pages/models-settings.js');
    const start = models.indexOf('  function fusionApplicationNotice(');
    const notice = new Function(models.slice(start, models.indexOf('\n  $("[data-recall]"', start)) + '\nreturn fusionApplicationNotice;')();
    for (const state of [undefined, null, {state:'unknown'}, {state:'applied'}, {state:'pending'}]) {
      const value = notice(state);
      assert.equal(value.warning, true);
      assert.doesNotMatch(value.text, new RegExp(i18nMessages.mc7a9aad944a77e4c,""));
    }
    assert.match(notice({state:'pending', reason:'runtime_not_running'}).text, new RegExp(i18nMessages.m3eae6ffd35fab5de,""));
    assert.equal(notice({state:'applied', applies:'next_query', revision:2,
      scope:{account_id:'synthetic', instance_id:'original', generation:1}, effective:{recall_fusion:{}}}).warning, false);
  });
  await test('core configuration Unknown queries the original desktop request without another setter', async () => {
    const models = read('pages/models-settings.js');
    const start = models.indexOf('  // Keep an uncertain operation');
    const listeners = {}, calls = [], notices = [];
    const fusion = {runtime:{effective:{}}, requestId:null};
    let pending, queriedState = 'unconfirmed';
    const save = {disabled:false};
    const nodes = {};
    const $ = selector => selector === '[type=submit]' ? save : (nodes[selector] ||= {
      addEventListener:(_, callback) => {listeners[selector] = callback;},
    });
    const renderStart = models.indexOf('  function renderFusion()');
    const renderFusion = new Function('$', 'root', 'fusion', 'FUSION_NUMERIC',
      models.slice(renderStart, models.indexOf('  // Collect + validate', renderStart)) + '\nreturn renderFusion;')($, {}, fusion, []);
    const coreSource = read('ui/core.js');
    const productionAction = new Function('notify', coreSource.slice(coreSource.indexOf('export async function action('),
      coreSource.indexOf('export function panel(')).replace(/^export /gm, '') + '\nreturn action;')(
      (...args) => notices.push(args));
    const bindings = {root:{}, fusion, summary:envelope => envelope.summary,
      $, action:(button, fn) => (pending = productionAction(button, fn)),
      fusionDraft:() => ({fusion:'legacy'}), renderFusion,
      notify:(...args) => notices.push(args),
      async execute(name) {
        calls.push(name);
        const error = Error('unknown'); error.requestId = 'desktop-original';
        error.operation = {state:'unconfirmed'}; throw error;
      },
      async api(name, args) {
        calls.push([name, args]);
        return {state:queriedState, envelope:{summary:{core_application:{state:'applied'}}}};
      },
      fusionApplicationNotice:() => ({text:'original receipt', warning:false}),
    };
    new Function(...Object.keys(bindings), models.slice(start, models.indexOf('  // Restoring defaults', start)))(...Object.values(bindings));
    const submit = {preventDefault() {}, target:{}};
    const saving = listeners['[data-fusion-form]'](submit);
    assert.equal(save.disabled, true);
    const duplicate = listeners['[data-fusion-form]'](submit);
    assert.equal(save.disabled, true);
    await duplicate;
    await saving;
    await pending;
    assert.equal(fusion.requestId, 'desktop-original');
    assert.equal(save.disabled, true);
    await listeners['[data-fusion-form]'](submit);
    assert.equal(save.disabled, true);
    save.disabled = false; // Even a direct submission cannot bypass the original-request guard.
    await listeners['[data-fusion-form]'](submit);
    assert.equal(save.disabled, true);
    assert.match(notices.at(-1)[0], new RegExp(i18nMessages.md72946e8a9667e47,""));
    listeners['[data-fusion-query]']({currentTarget:{}}); await pending;
    assert.equal(fusion.requestId, 'desktop-original');
    assert.equal(save.disabled, true);
    queriedState = 'done';
    listeners['[data-fusion-query]']({currentTarget:{}}); await pending;
    assert.equal(fusion.requestId, null);
    assert.equal(save.disabled, false);
    assert.deepEqual(calls, ['agent_set', ['operation-recover', {id:'desktop-original'}], ['operation-recover', {id:'desktop-original'}]]);
  });
  const { overviewModelStatus, overviewModelsPanel } = await import('../internal/desktop/frontend/pages/overview-models.js');
  const { readProfileState } = await import('../internal/desktop/frontend/pages/model-kit.js');
  const { modelOperationProgress } = await import('../internal/desktop/frontend/pages/models-view.js');
  await test('Overview retries a failed active selection read on the next update', async () => {
    const source = read('pages/overview.js');
    const load = source.slice(source.indexOf('  async function loadProfileSelection('), source.indexOf('  function update()'));
    let requests = 0, renders = 0;
    const active = { model: 'granite-311m-r2' };
    const profile = { catalog: { details: { active } } };
    const { loadProfileSelection, selection } = new Function('activeModelScope', 'ctx', 'root', 'fetchProfileState', 'readProfileState', 'update',
      `let profileRequest, profileSelection; ${load} return { loadProfileSelection, selection: () => profileSelection };`)(
      () => 'scope', {}, { isConnected: true }, async () => ++requests === 1 ? null : profile, readProfileState, () => renders++);
    const state = { connected: true, health: { capabilities: ['model.catalog.v2'] } };
    await loadProfileSelection(state);
    assert.equal(selection(), undefined);
    assert.equal(renders, 0);
    await loadProfileSelection(state);
    assert.equal(selection(), active);
    assert.equal(renders, 1);
    await loadProfileSelection(state);
    assert.equal(requests, 2);
  });
  await test('Overview resolves serving profile models and legacy engine snapshots', () => {
    const cases = [
      ['granite-311m-r2', 'quantized', 'quint8_avx2', 'Granite 311M R2', 'cpu', 768, 32768],
      ['granite-97m-r2', 'fp32', 'fp32', 'Granite 97M R2', 'npu', 384, 32768],
      ['bge-base-zh-v1.5', 'fp16', 'fp16', 'BGE-base-zh-v1.5', 'gpu', 768, 512],
      ['m3', 'quantized', 'quantized', 'BGE-M3', 'cpu', 1024, 512],
    ];
    for (const [model, variant, expectedVariant, title, engine, dims, context] of cases) {
      const { active } = readProfileState({ catalog: { details: { active: { model, variant, engine } } } });
      const legacy = { model_profile: model, variant, engine };
      for (const [selection, snapshot] of [[active, { variant: 'fp16', engine: 'gpu' }], [null, legacy]]) {
        const markup = overviewModelStatus({ connected: true, engine: snapshot }, {}, selection, {}).details;
        assert.ok(markup.includes(`${title} \u00b7 ${expectedVariant}`), model);
        assert.ok(markup.includes(`class="badge">${engine.toUpperCase()}</span>`), model);
        assert.ok(markup.includes(`${dims.toLocaleString()} <span>dims</span>`), model);
        assert.ok(markup.includes(`${context.toLocaleString()} <span>token context</span>`), model);
        if (model !== 'm3') assert.doesNotMatch(markup, /BGE-M3/);
      }
    }
    const legacy = overviewModelStatus({ connected: true, engine: { variant: 'fp16', engine: 'gpu' } }, {}, null, {});
    assert.match(legacy.details, /BGE-M3 \u00b7 fp16/);
    assert.doesNotMatch(overviewModelStatus({ connected: true }, {}, null, {}).details, /BGE-M3/);
    const pending = overviewModelStatus({ connected: true, engine: { variant: 'fp16', engine: 'gpu' } }, {}, undefined, {});
    assert.doesNotMatch(pending.details, /BGE-M3/);
  });
  await test('Overview index KPI and Models & Index show coverage, telemetry and live progress', () => {
    const overview = read('pages/overview.js');
    assert.match(overview, /data-overview-details[\s\S]*overviewModelsPanel\(\)/);
    const metrics = overview.match(/metricsRoot\.innerHTML = (.*\$\{modelStatus\.metric\})/);
    assert.equal((metrics?.[1].match(/\$\{metric\(/g) || []).length, 3);
    assert.match(overview, /if \(button\.dataset\.go\) ctx\.navigate\(button\.dataset\.go\)/);
    assert.match(overviewModelsPanel(), /<h2>Models &amp; Index<\/h2>.*data-go="models">Model Settings \u2192<\/button>/);
    const state = { connected: true, health: { inference: {
      foreground: { active: true, capacity: 32 }, background: { active: 2 },
    }, rpc: { recent_timing_micros: [[9000, 2500], [7000, 4000]] } } };
    const summary = { indexed_count: 9, local_alive: 12, retrieval_index: { state: 'ready' } };
    const active = { model: 'granite-311m-r2', variant: 'quint8_avx2', engine: 'cpu' };
    const ready = overviewModelStatus(state, summary, active, {});
    assert.match(ready.metric, /Index Status.*>Ready<\/span>/);
    assert.match(ready.metric, />9 \/ 12 \(75%\)<\/strong><progress .*max="100" value="75"/);
    assert.match(ready.metric, /<small>Granite 311M R2 \u00b7 quint8_avx2<\/small>/);
    assert.equal((ready.details.match(/<dt>/g) || []).length, 3);
    assert.match(ready.details, /<dt>Foreground<\/dt><dd>1 \/ 32<\/dd>/);
    assert.match(ready.details, /<dt>Background<\/dt><dd>2<\/dd>/);
    assert.match(ready.details, /<dt>Index Coverage<\/dt><dd>9 \/ 12 \(75%\)<\/dd>/);
    assert.match(ready.details, /<strong>3\.25 ms<\/strong>/);
    assert.match(ready.details, /overview-model-strip[^\n]*>Ready</);
    assert.match(overview, /overviewModelStatus\(state, s, profileSelection, op, operationProgress\)/);
    const renderOperation = op => overviewModelStatus(state, summary, active, op, modelOperationProgress(op));
    const switching = renderOperation({ active: true, phase: 'switch-index', done: 3, total: 12 });
    assert.match(switching.metric, />Shadow Indexing</);
    assert.match(switching.details, /overview-model-strip[^\n]*<progress value="3" max="12">/);
    const streamed = renderOperation({ active: true, phase: 'running', done: 55, total: 100 });
    assert.match(streamed.metric, />Shadow Indexing</);
    assert.match(streamed.details, /<progress value="55" max="100">/);
    const download = renderOperation({ active: true, phase: 'download', progress_percent: 40 });
    assert.match(download.details, /overview-model-strip[^\n]*<progress value="40" max="100">/);
    const busy = overviewModelStatus({ ...state, task: { busy: true, stage: 'model.onnx', done: 1024, total: 2048 } }, summary, active, {});
    assert.match(busy.details, /1\.0 KiB \/ 2\.0 KiB \(50%\).*<progress value="1024" max="2048">/);
    const failed = overviewModelStatus(state, { ...summary, retrieval_index: { state: 'failed', error: '<index error>' } }, active, {});
    assert.match(failed.metric, />Error</);
    assert.match(failed.metric, /&lt;index error&gt;/);
    const paused = overviewModelStatus(state, { ...summary, retrieval_index: { state: 'paused', scheduled: true } }, active, {});
    assert.match(paused.metric, />Paused</);
  });
  console.log(`${count} focused refresh tests passed; browser/native UIA not exercised.`);
})().catch(error => { console.error(error); process.exitCode = 1; });
