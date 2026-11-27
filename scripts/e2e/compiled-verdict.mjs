// Keep a missing required scenario visible even if a future runner exits zero.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';

const output = readFileSync(process.argv[2], 'utf8').split(/\r?\n/);
const gates = [
  ['E2E-002', 'downloadModel', 'slow owned download with observable progress'],
  ['E2E-003', 'switchModel', 'installed CPU candidate and responsive incumbent'],
  ['E2E-004', 'rollbackModel', 'accepted candidate that fails verification with a working incumbent'],
];
const missing = [];
for (const [id, input, requirement] of gates) {
  const records = output.filter(line => new RegExp('^(PASS|FAIL|SKIP) COMPILED RUNTIME ' + id + ' ').test(line));
  if (records.length !== 1 || !records[0].startsWith('PASS ')) missing.push(id + ' requires ' + input + ': ' + requirement);
}
assert.equal(missing.length, 0, missing.join('\n'));
assert.equal(output.filter(line => line === 'SUMMARY passed=7 skipped=0 failures=0 preflight=false').length, 1,
  'All seven compiled scenarios must pass in one verification run');
