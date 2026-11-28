import assert from 'node:assert/strict';
import {channel} from 'node:diagnostics_channel';
import {loadScenarios, runScenario} from '../../test-cases/runner.mjs';
import {withRuntime} from '../../test-cases/targets.mjs';
import {mcpRoundTrip} from '../../test-cases/plugin-rpc/mcp.mjs';
import {cliTarget} from '../../test-cases/cli-e2e/command.mjs';
import {prepareRecall, prepareSwitch, settledSwitchTarget} from './switch-fixture.mjs';
import {runDownload} from './download-fixture.mjs';
import {verifyPluginTransport} from './plugin-transport.mjs';

// The scenario runner reports "HTTP status mismatch" without the received status.
channel('undici:request:headers').subscribe(({request, response}) => {
  if (request.method === 'POST' && request.path === '/v1/models/switch') {
    console.log('Model switch admission: expected HTTP 202, received ' + response.statusCode);
  }
});

const [binary, cli, downloadBinary] = process.argv.slice(2);
const compiledIds = ['E2E-001', 'E2E-002', 'E2E-003', 'E2E-004', 'E2E-005', 'E2E-006', 'E2E-007'];
const scenarios = (await loadScenarios()).filter(scenario => compiledIds.includes(scenario.id));
assert.deepEqual(scenarios.map(scenario => scenario.id), compiledIds);
let passed = 0;
let skipped = 0;
for (const scenario of scenarios) {
  const target = scenario.target === 'cli' ? 'CLI' : scenario.target ? 'LEGACY RPC' : 'RUNTIME';
  const label = 'COMPILED '+target+' '+scenario.id+' '+scenario.title;
  try {
    if (scenario.id === 'E2E-002') {
      await runDownload(downloadBinary, scenario);
      console.log('PASS '+label);
      passed++;
      continue;
    }
    const models = ['E2E-003','E2E-004'].includes(scenario.id);
    // Only advertise inputs with an actual offline staging implementation.
    const provided = models ? [scenario.id === 'E2E-004' ? 'rollbackModel' : 'switchModel'] : [];
    const missing = (scenario.inputs ?? []).filter(key => !provided.includes(key));
    if (missing.length) {
      console.error('SKIP '+label+' — missing staged offline input: '+missing.join(', '));
      skipped++;
      continue;
    }
    await withRuntime(binary,{syntheticReady:true},async runtime => {
      const inputs = models ? await prepareSwitch(runtime,scenario.id === 'E2E-004') : {};
      const target = models ? settledSwitchTarget(runtime) :
        scenario.target === 'cli' ? cliTarget(cli,runtime) : runtime.target;
      await runScenario(scenario,target,inputs);
      if (scenario.id === 'E2E-007') await verifyPluginTransport(runtime);
    },{deviceLocal:models || scenario.id === 'E2E-005'});
    if (process.platform === 'win32' && scenario.id === 'E2E-007') {
      for (const locale of ['en', 'zh']) {
        const bytes = await withRuntime(binary, {syntheticReady:true}, async runtime => {
          await prepareRecall(runtime);
          return mcpRoundTrip(cli, runtime, locale);
        }, {junction:true, deviceLocal:true});
        console.log('VERIFIED WINDOWS JUNCTION MCP STDIO locale=' + locale + ' request bytes=' + bytes);
      }
    }
    console.log('PASS '+label);
    passed++;
  } catch (error) {
    if (error.cmd?.includes('Set-Acl')) {
      const code = Number.isInteger(error.code) ? error.code : 'unreported';
      const signal = ['SIGTERM','SIGKILL'].includes(error.signal) ? error.signal : 'unreported';
      console.error('FAIL '+label+' — synthetic ACL setup expected exit 0; exit='+code+' signal='+signal+' killed='+(error.killed===true));
    } else {
      console.error('FAIL '+label+' — '+error.message);
    }
  }
}
const failures=scenarios.length-passed;
console.log(`SUMMARY passed=${passed} skipped=${skipped} failures=${failures} preflight=false`);
process.exitCode=failures ? 1 : 0;
