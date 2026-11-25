import assert from 'node:assert/strict';
import {test} from 'node:test';
import {canonicalTuples, checkGapShrink, compare, parseGapRoster, runtimeRows} from './test-v1-route-parity.mjs';

const route = {method: 'GET', path: '/v1/memories/{memoryId}', operation_id: 'readMemory'};
const expected = ['GET /v1/memories/{memoryId} readMemory'];

test('metadata must agree in both directions; methods and names are not interchangeable', () => {
  assert.doesNotThrow(() => compare(expected, [route]));
  for (const wrong of [
    {method: 'HEAD'}, {path: '/v1/memories/{id}'},
    {path: '/v1/{*unmatched}'}, {operation_id: 'writeMemory'},
  ]) assert.throws(() => compare(expected, [{...route, ...wrong}]));
  assert.throws(() => compare(expected, []), /missing:/);
  assert.throws(() => compare(expected, [route, {...route, method: 'PATCH', operation_id: 'patchMemory'}]),
    /unexpected:/);
  assert.throws(() => compare(expected, [route, route]), /duplicate/);
});

test('path parameter declarations survive local references and operation overrides', () => {
  const document = {openapi: '3.1.0', paths: {[route.path]: {
    parameters: [{$ref: '#/components/parameters/MemoryId'}],
    get: {operationId: route.operation_id, parameters: [{in: 'query', name: 'memoryId'}]},
  }}, components: {parameters: {MemoryId: {in: 'path', name: 'memoryId', required: true}}}};
  assert.deepEqual(canonicalTuples(document), expected);
  document.paths[route.path].get.parameters.push({in: 'path', name: 'memoryId', required: false});
  assert.throws(() => canonicalTuples(document), /path parameter must be required/);
  document.paths[route.path].get.parameters.pop();
  document.components.parameters.MemoryId.name = 'id';
  assert.throws(() => canonicalTuples(document), /path parameter names/);
});

test('no export, no test, and duplicate exports fail', () => {
  const line = 'RESPIRE_V1_ROUTES_JSON=' + JSON.stringify([route]);
  const pass = 'test result: ok. 1 passed; 0 failed; 0 ignored;';
  assert.deepEqual(runtimeRows(line + '\n' + pass), [route]);
  assert.throws(() => runtimeRows(pass));
  assert.throws(() => runtimeRows(line));
  assert.throws(() => runtimeRows([line, line, pass].join('\n')));
});

test('reviewed gaps must match exactly and shrink when a route is bound', () => {
  const gaps = [
    "DELETE /v1/spaces/{spaceId} deleteSpace",
    "GET /v1/backups/export exportBackup",
    "GET /v1/spaces listSpaces",
    "GET /v1/spaces/{spaceId} getSpace",
    "PATCH /v1/spaces/{spaceId} updateSpace",
    "POST /v1/backups/import importBackup",
    "POST /v1/host/transactions/login executeLoginTransaction",
    "POST /v1/host/transactions/logout executeLogoutTransaction",
    "POST /v1/organize/split splitOrganizeNode",
    "POST /v1/spaces createSpace",
    "POST /v1/spaces/{spaceId}/members inviteSpaceMember"
  ];
  const canonical = [...expected, ...gaps];
  const options = {acknowledged: gaps};
  assert.deepEqual(compare(canonical, [route], options), {registered: 1, canonical: 12, missing: 11});
  assert.throws(() => compare(canonical, [route]), /missing:/);
  assert.throws(() => compare(canonical, [], options), /missing: GET .*memories/);
  const added = {method: 'GET', path: '/v1/spaces', operation_id: 'listSpaces'};
  assert.throws(() => compare(canonical, [route, added], options), /remove stale reviewed gap/);
  assert.throws(() => compare(canonical.slice(0, -1), [route], options), /remove stale reviewed gap/);
  assert.throws(() => compare(canonical, [route, {...added, operation_id: 'listOtherSpaces'}], options), /unexpected:/);
});


test('trusted predecessor prevents adding or exchanging exact debt tuples', () => {
  const old = ['GET /v1/spaces listSpaces', 'POST /v1/spaces createSpace'];
  assert.doesNotThrow(() => checkGapShrink(old, [old[1]]));
  const regression = 'GET /v1/health getHealth';
  assert.throws(() => checkGapShrink(old, [old[1], regression]), /cannot add reviewed route gaps/);
  const wire = (rows) => Buffer.from(JSON.stringify(rows, null, 2) + '\n');
  assert.deepEqual(parseGapRoster(wire(old)), old);
  assert.throws(() => parseGapRoster(wire([old[0], old[0]])), /duplicate/);
  assert.throws(() => parseGapRoster(wire([...old].reverse())), /noncanonical/);
});
