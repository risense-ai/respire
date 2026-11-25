import assert from 'node:assert/strict';
import {test} from 'node:test';
import {canonicalTuples, compare, runtimeRows} from './test-v1-route-parity.mjs';

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