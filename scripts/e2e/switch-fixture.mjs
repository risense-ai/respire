// First-party ONNX opset 13 Identity and tokenizer from model_switch_tests.rs at
// 65ad0ffcfee97b15e828a0be4a5b65291d0d1800. Distributed under the root LICENSE.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {mkdir, readFile, writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {probeReadiness} from '../../referee/test-cases/targets.mjs';
import {setTimeout as delay} from 'node:timers/promises';

const graph = Buffer.from(
  '08093a8a010a1c0a0566697865641209656d62656464696e6722084964656e74697479120973796e7468657469632a1f0801' +
  '0804100122100000803f000000000000000000000000420566697865645a210a09696e7075745f69647312140a120807120e' +
  '0a0208010a081206746f6b656e73621b0a09656d62656464696e67120e0a0c080112080a0208010a0208044202100d', 'hex');
const vocabulary = Buffer.from('{"version":"1.0","model":{"type":"WordLevel","vocab":{"[UNK]":0},"unk_token":"[UNK]"}}');

export async function prepareSwitch(runtime, rollback) {
  const root = runtime.env.RSRS_DATA_DIR;
  const assets = join(root, 'qualification-models');
  await mkdir(assets, {mode: 0o700});
  async function resource(name, bytes) {
    const path = join(assets, name);
    await writeFile(path, bytes, {flag:'wx', mode:0o600});
    return {path, size:bytes.length, sha256:createHash('sha256').update(bytes).digest('hex')};
  }
  const good = await resource('identity.onnx', graph);
  const tokenizer = await resource('tokenizer.json', vocabulary);
  const incumbent = 'fixture-incumbent';
  const candidate = rollback ? 'fixture-invalid' : 'fixture-candidate';
  const models = {};
  for (const id of [incumbent, candidate]) {
    models[id] = {id, tokenizer, weights: (rollback && id === candidate) ?
      await resource('invalid.onnx', Buffer.from('invalid synthetic ONNX')) : good,
      embedding:{model_id:id, revision:'fixture-r1', dimensions:4, max_tokens:32,
        pooling:'sentence_embedding', normalize:true, query_prefix:'', document_prefix:'',
        pad_token_id:0, pad_to_max_tokens:false}, onnx:{input_ids:'input_ids', output:'embedding'}};
  }
  await writeFile(join(root,'embedding-profiles-v1.json'), JSON.stringify({models,backends:{}}), {flag:'wx',mode:0o600});
  const request = spec => runtime.target.request(spec);
  if (!rollback) {
    // Populate the disposable library before starting either switch worker.
    for (let n=0;n<128;n++) {
      const stored=await request({method:'POST',path:'/v1/memories',body:{content:'Synthetic background record '+n}});
      assert.equal(stored.status,201,'fixture seed '+(n+1)+'/128: expected HTTP 201, received '+stored.status);
    }
  }
  const admission = rollback ? runtime.target : settledSwitchTarget(runtime);
  const started = await admission.request({method:'POST', path:'/v1/models/switch', body:{model_id:incumbent,engine:'cpu'}});
  assert.equal(started.status,202,'incumbent switch admission');
  assert.equal(typeof started.data.operation_id,'string');
  const deadline = Date.now() + 30_000;
  for (;;) {
    const op = await request({method:'GET',path:'/v1/operations/'+encodeURIComponent(started.data.operation_id)});
    assert.equal(op.status,200);
    if (op.data.state === 'completed') break;
    assert(['pending','running'].includes(op.data.state) && Date.now()<deadline,'incumbent did not complete');
    await delay(50);
  }
  const active = await request({method:'GET',path:'/v1/models/active-assets'});
  assert.equal(active.data.active_model_id,incumbent);
  return rollback ? {rollbackModel:candidate} : {switchModel:candidate};
}


// Before a switch, check that the owned Runtime is idle without retrying the POST.
export function settledSwitchTarget(runtime) {
  const target = runtime.target;
  return {...target, async request(spec) {
    if (spec.method === 'POST' && spec.path === '/v1/models/switch') {
      const profile = runtime.env.RSRS_DATA_DIR;
      const endpoint = JSON.parse(await readFile(join(profile,'runtime','endpoint.json'),'utf8'));
      const base = 'http://127.0.0.1:' + runtime.env.RSRS_RPC_PORT;
      assert.equal(endpoint.url,base);
      const modern = await probeReadiness(base,endpoint.pid,false,profile);
      assert.equal(modern?.instance_id,endpoint.instance_id);
      const deadline = performance.now() + 30_000;
      for (;;) {
        const health = await probeReadiness(base,endpoint.pid,true);
        assert.equal(health?.instance_id,modern.instance_id);
        assert(Array.isArray(health.control?.background) && Array.isArray(health.control?.admitted));
        const rpc = health.rpc;
        assert(Number.isSafeInteger(rpc?.ordinary_running) && Number.isSafeInteger(rpc.lightweight_running));
        if (!health.control.background.length && !health.control.admitted.length &&
            rpc.ordinary_running === 0 && rpc.lightweight_running === 0) {
          const reply = await target.request({method:'POST',path:'/rpc',body:{
            v:1,id:'card019-model-progress',method:'model.progress',args:[]}});
          assert.equal(reply.status,200);
          assert.equal(reply.data?.ok,true);
          assert.equal(reply.data.envelope?.command,'model.progress');
          const active = reply.data.envelope.summary?.operation?.active;
          assert.equal(typeof active,'boolean');
          if (!active && performance.now() < deadline) break;
        }
        assert(performance.now() < deadline,'owned Runtime did not report idle before switch admission');
        await delay(100);
      }
    }
    return target.request(spec);
  }};
}
