const { i18nMessages, readFrontend } = require('./frontend-source.cjs');
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const root = path.resolve(__dirname, '..');
const frontend = path.join(root, 'internal/desktop/frontend');
const read = readFrontend;

// The settings page renders the proxy snapshot the desktop bridge reports
// (/api/state → proxy). Execute the production row builder, not a copy.
const proxy = new Function(
  read('ui/proxy.js').replace(/^export /gm, '') + '\nreturn {proxyRows};',
)();

const cases = [
  {
    snapshot: { mode: 'system', source: 'system', http: 'http://127.0.0.1:7897', https: 'http://127.0.0.1:7897', noProxy: 'localhost,127.0.0.1' },
    expect: [[i18nMessages.m805c408a0d547ad7, i18nMessages.m24bbc60c5fb3b169], [i18nMessages.mfe51a8c86091b1e6, 'http://127.0.0.1:7897'], [i18nMessages.mb7a8cb3914c9c746, 'localhost,127.0.0.1']],
  },
  {
    snapshot: { mode: 'system', source: 'environment', https: 'http://env:3128', noProxy: 'corp.example' },
    expect: [[i18nMessages.m805c408a0d547ad7, i18nMessages.me523a41d10a6490b], [i18nMessages.m67047f316c5f3c88, 'http://env:3128'], [i18nMessages.mb7a8cb3914c9c746, 'corp.example']],
  },
  {
    snapshot: { mode: 'direct', source: 'off' },
    expect: [[i18nMessages.m805c408a0d547ad7, i18nMessages.m29a31f33c0978424]],
    count: 1,
  },
  {
    snapshot: { mode: 'system', source: 'unavailable', note: i18nMessages.me9c4d2a5e769ee12 },
    expect: [[i18nMessages.m805c408a0d547ad7, i18nMessages.m17555f381702f8c7], [i18nMessages.ma675fb3508dd344f, i18nMessages.me9c4d2a5e769ee12]],
  },
  {
    snapshot: {},
    expect: [[i18nMessages.m805c408a0d547ad7, i18nMessages.m90b31ad093acf71f]],
    count: 1,
  },
];

for (const item of cases) {
  const rows = proxy.proxyRows(item.snapshot);
  for (const [label, value] of item.expect) {
    assert.ok(rows.some(([k, v]) => k === label && v === value), `missing ${label}: ${value} in ${JSON.stringify(rows)}`);
  }
  if (item.count) assert.equal(rows.length, item.count, `row count for ${JSON.stringify(item.snapshot)}`);
}

// The preference control and its status container must stay on the page.
const settings = read('pages/settings.js');
assert.match(settings, /data-pref=\\?"proxyMode\\?"/, 'proxy mode selector missing');
assert.match(settings, /data-proxy/, 'proxy status container missing');
assert.match(settings, /proxyRows\(/, 'proxy rows must be rendered from the shared builder');

// Every label the panel renders must exist in the Chinese→English table, or
// the English UI would keep Chinese proxy rows.
const i18n = read('ui/i18n.js');
const labels = new Set([...cases.flatMap((item) => item.expect.map(([label]) => label)), i18nMessages.m5e84ea61e8386af7, i18nMessages.mb06325c5660f0c29]);
for (const label of labels) {
  assert.ok(i18n.includes(`["${label}",`), `missing i18n entry for ${label}`);
}

console.log(`settings proxy panel: ${cases.length} cases passed`);
