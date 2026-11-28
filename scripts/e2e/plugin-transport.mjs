import assert from 'node:assert/strict';
import {exactBytes, minimumBytes, payload, wire} from '../../test-cases/plugin-rpc/transport.mjs';

// The pinned target accepts only /v1 and /rpc. Keep MCP separate and
// address only the already-owned fixture listener, with no redirects.
async function readMcp({RSRS_RPC_PORT: port, RSRS_RPC_TOKEN: token}, id) {
  assert(/^[0-9]{1,5}$/.test(port) && Number(port) > 0 && Number(port) <= 65535);
  assert(typeof token === 'string' && token.length > 0, 'Missing fixture authorization');
  const response = await fetch('http://127.0.0.1:' + port + '/mcp', {
    method:'POST', redirect:'error', signal:AbortSignal.timeout(15_000),
    headers:{authorization:'Bearer ' + token, 'content-type':'application/json', accept:'application/json'},
    body:JSON.stringify({
      jsonrpc:'2.0', id:'plugin-utf8-read', method:'tools/call',
      params:{name:'memory_show', arguments:{id}},
    }),
  });
  assert.equal(response.status, 200, 'MCP HTTP request failed');
  assert.equal(response.headers.get('content-type')?.split(';')[0], 'application/json');
  return response.json();
}

export async function verifyPluginTransport({target, env}) {
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

  const call = await readMcp(env, id);
  assert.equal(call?.jsonrpc, '2.0');
  assert.equal(call?.id, 'plugin-utf8-read');
  assert.equal(call?.result?.isError, false, 'MCP memory_show failed');
  const parts = call.result.content;
  assert.equal(parts?.length, 1);
  assert.equal(parts[0].type, 'text');
  const envelope = JSON.parse(parts[0].text);
  assert.equal(envelope.command, 'show');
  assert.equal(envelope.status, 'ok');
  assert.equal(envelope.details?.entry?.id, id);
  exactBytes(envelope.details?.entry?.content, note.content);
  console.log('VERIFIED PLUGIN UTF-8: OpenAPI JSON and MCP HTTP returned exact content; request bytes=' + bytes);
}
