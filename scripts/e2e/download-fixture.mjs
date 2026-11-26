import assert from 'node:assert/strict';
import {once} from 'node:events';
import {readdir} from 'node:fs/promises';
import {createServer} from 'node:http';
import {join} from 'node:path';
import {runScenario} from '../../referee/test-cases/runner.mjs';
import {withRuntime} from '../../referee/test-cases/targets.mjs';

const catalogPath = '/Xenova/bge-small-zh-v1.5/resolve/75c43b069aac4d136ba6bc1122f995fedcfd2781/tokenizer.json';
// The test cancels before completion. No synthetic bytes can pass catalog integrity verification.
const chunk = Buffer.alloc(16 * 1024, 0x61);
const limit = 128 * 1024;

export async function runDownload(binary, scenario) {
  assert(binary, 'missing feature-enabled Runtime');
  const transfers = [];
  let unexpected = 0;
  const server = createServer((req, res) => {
    if (req.method !== 'GET' || req.url !== catalogPath || transfers.length >= 2 ||
        req.headers.authorization || req.headers.cookie || req.headers['proxy-authorization']) {
      unexpected++;
      res.writeHead(404).end();
      return;
    }
    const record = {bytes: 0};
    transfers.push(record);
    res.writeHead(200, {'Content-Type': 'application/octet-stream', 'Content-Length': '439125'});
    res.flushHeaders();
    const timer = setInterval(() => {
      if (record.bytes >= limit || res.writableNeedDrain) return;
      res.write(chunk);
      record.bytes += chunk.length;
    }, 120);
    res.once('close', () => clearInterval(timer));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const {port} = server.address();
  try {
    await withRuntime(binary, {syntheticReady: true}, async runtime => {
      const request = spec => runtime.target.request(spec);
      const origin = 'http://127.0.0.1:' + port;
      const config = await request({method:'PATCH', path:'/v1/host/config', body:{model_mirror:origin}});
      assert.equal(config.status, 200, 'fixture mirror configuration');
      assert.equal(config.data.model_mirror, origin);
      assert.equal(config.data.proxy_url, null);
      const operations = [];
      await runScenario(scenario, {...runtime.target, async request(spec) {
        const result = await request(spec);
        if (spec.method === 'POST' && spec.path === '/v1/models/downloads' && result.status === 202) {
          operations.push(result.data.operation_id);
        }
        return result;
      }}, {downloadModel:'bge-small-zh-v1.5'});
      assert.equal(operations.length, 2);
      assert.equal(new Set(operations).size, 2);
      for (const id of operations) {
        const response = await request({method:'GET', path:'/v1/operations/' + encodeURIComponent(id)});
        assert.equal(response.status, 200);
        assert.equal(response.data.state, 'cancelled');
      }
      const files = await readdir(join(runtime.env.RSRS_DATA_DIR,'models'), {recursive:true, withFileTypes:true})
        .catch(error => { if (error.code === 'ENOENT') return []; throw error; });
      assert(files.every(file => file.isDirectory()), 'cancelled fixture download left model files');
    }, {deviceLocal:true, downloadMirrorPort:port});
    assert.equal(unexpected, 0, 'unexpected mirror request');
    assert.equal(transfers.length, 2);
    assert(transfers.every(({bytes}) => bytes > 0 && bytes <= limit));
  } finally {
    const closed = new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
    server.closeAllConnections();
    await closed;
  }
}
