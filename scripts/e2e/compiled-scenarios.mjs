// The pinned referee owns every request, assertion, executable and process lifetime.
import assert from 'node:assert/strict';
import {loadScenarios, runScenario} from '../../referee/test-cases/runner.mjs';
import {withRuntime} from '../../referee/test-cases/targets.mjs';
import {cliTarget} from '../../referee/test-cases/cli-e2e/command.mjs';
import {prepareSwitch} from './switch-fixture.mjs';

const [binary, cli] = process.argv.slice(2);
const scenarios = await loadScenarios();
assert.deepEqual(scenarios.map(s => s.id), Array.from({length:7}, (_,n) => 'E2E-00'+(n+1)));
let passed = 0;
let skipped = 0;
for (const scenario of scenarios) {
  const target = scenario.target === 'cli' ? 'CLI' : scenario.target ? 'LEGACY RPC' : 'RUNTIME';
  const label = 'COMPILED '+target+' '+scenario.id+' '+scenario.title;
  try {
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
      await runScenario(scenario, scenario.target === 'cli' ? cliTarget(cli,runtime) : runtime.target,inputs);
    },{deviceLocal:models || scenario.id === 'E2E-005'});
    console.log('PASS '+label);
    passed++;
  } catch (error) {
    console.error('FAIL '+label+' — '+error.message);
  }
}
const failures=scenarios.length-passed;
console.log(`SUMMARY passed=${passed} skipped=${skipped} failures=${failures} preflight=false`);
process.exitCode=failures ? 1 : 0;
