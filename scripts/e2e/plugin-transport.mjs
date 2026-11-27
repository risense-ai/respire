import assert from 'node:assert/strict';
import {exactBytes, minimumBytes, payload, wire} from '../../referee/test-cases/plugin-rpc/transport.mjs';

export async function verifyPluginTransport(target) {
  const note = payload('openapi-mcp', 'zh');
  const bytes = wire(note).length;
  assert(bytes > minimumBytes, 'Plugin request body must exceed 100 KiB');
  const stored = await target.request({method:'POST', path:'/v1/memories', body:note});
  assert.equal(stored.status, 201, 'OpenAPI create did not commit the UTF-8 payload');
  const id = stored.data?.id;
  assert(typeof id === 'string' && id, 'OpenAPI create did not return a memory ID');
  const read = await target.request({method:'GET', path:'/v1/memories/' + encodeURIComponent(id)});
  assert.equal(read.status, 200, 'OpenAPI read did not return the stored memory');
  assert.equal(read.data?.id, id);
  exactBytes(read.data?.content, note.content);

  const call = await target.request({method:'POST', path:'/mcp', body:{
    jsonrpc:'2.0', id:'plugin-utf8-read', method:'tools/call',
    params:{name:'memory_show', arguments:{id}},
  }});
  assert.equal(call.status, 200, 'MCP HTTP request failed');
  assert.equal(call.data?.jsonrpc, '2.0');
  assert.equal(call.data?.id, 'plugin-utf8-read');
  assert.equal(call.data?.result?.isError, false, 'MCP memory_show failed');
  const parts = call.data.result.content;
  assert.equal(parts?.length, 1);
  assert.equal(parts[0].type, 'text');
  const envelope = JSON.parse(parts[0].text);
  assert.equal(envelope.command, 'show');
  assert.equal(envelope.status, 'ok');
  assert.equal(envelope.details?.entry?.id, id);
  exactBytes(envelope.details?.entry?.content, note.content);
  console.log('VERIFIED PLUGIN UTF-8: OpenAPI JSON and MCP HTTP returned exact content; request bytes=' + bytes);
}
