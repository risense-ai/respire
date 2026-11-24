"""Additional cases for FixtureInstallationTests."""
import ast
import json
import re
import subprocess
from pathlib import Path
import types
from native_fixture_process import runtime_install_diagnostic


class FixtureInstallationCases:
    def test_fixture_receives_its_own_verified_installation(self):
        self.run_fixture()
        startup = Path(__file__).with_name('test-windows-host-startup.ps1').read_text(encoding='utf-8-sig')
        self.assertIn('--installation-root $profileRoot', startup)
        self.assertLess(startup.index('Copy-Item -LiteralPath $nativeExecutable -Destination $stableCli'),
                        startup.index('Start-ScheduledTask -TaskName $taskName'))
        self.assertIn('Get-FileHash -LiteralPath $stableCli -Algorithm SHA256', startup)
        self.assertIn("('bin/runtime/' + $managedReceipt.version + '/rsrs-runtime.exe')", startup)
        self.assertEqual(startup.count("-Headers @{'X-Respire-Health' = 'runtime-v1'}"), 3)
        self.assertIn('task_result_parsed = ($null -ne $taskEnvelope); task_error_code = $taskErrorCode;', startup)
        bundle = ast.parse(Path(__file__).with_name('runtime-acceptance-bundle.py').read_text(encoding='utf-8'))
        entrypoint = next(node for node in bundle.body if isinstance(node, ast.If))
        call = next(node for node in ast.walk(entrypoint) if isinstance(node, ast.Call)
                    and isinstance(node.func, ast.Name) and node.func.id == 'stage_runtime')
        self.assertTrue(any(arg.arg == 'installation_root' and isinstance(arg.value, ast.Attribute)
                            and arg.value.attr == 'installation_root' for arg in call.keywords))
        workflows = Path(__file__).resolve().parents[1] / '.github/workflows'
        programs = []
        for name, product_job in [('runtime-autostart-validation.yml', 'user-runtime'),
                                  ('windows-startup-validation.yml', 'startup')]:
            text = (workflows / name).read_text(encoding='utf-8')
            source, product = text.split('\n  ' + product_job + ':\n')
            self.assertIn('  source-checks:\n', source)
            self.assertNotIn('installPinnedRuntime', source)
            self.assertNotIn('test-runtime-autostart.py --binary', source)
            self.assertNotIn('./scripts/test-windows-host-startup.ps1', source)
            self.assertIn('Source and fixture checks only.', source)
            self.assertIn("if: github.event_name == 'workflow_dispatch'", product)
            self.assertIn('needs: source-checks', product)
            self.assertIn('EXPECTED_RUNTIME_SOURCE_SHA: ${{ inputs.runtime_source_sha }}', product)
            self.assertIn('      runtime_source_sha:\n', source)
            self.assertIn('        required: true\n', source)
            self.assertNotIn('continue-on-error', text)
            body = product.split("node --input-type=module <<'NODE'\n")[1].split('          NODE')[0]
            body = re.sub(r'^\s*import .+;\s*$', '', body, flags=re.MULTILINE)
            body = body.replace("const {installPinnedRuntime} = await import('./scripts/ci-install-pinned-runtime.mjs');", '')
            matrix = None
            if name == 'runtime-autostart-validation.yml':
                self.assertNotIn('previous', product)
                self.assertNotIn('npm install', product)
                self.assertNotIn('model_maintenance', product)
                self.assertIn('--register\n', product)
                self.assertIn('macos-15-intel', product)
                preparation = product.split('      - name: Prepare hosted Linux credentials and user systemd\n')[1].split('      - uses:')[0]
                for proof in ('test "$GITHUB_ACTIONS" = true', 'test "$RUNNER_ENVIRONMENT" = github-hosted',
                              'dbus-user-session dbus-x11 gnome-keyring', 'SecretStorage==3.3.3 jeepney==0.9.0 cryptography==46.0.3',
                              'sudo loginctl enable-linger "$(id -un)"', 'sudo systemctl restart "user@$uid.service"',
                              'sudo systemctl is-active --quiet "user@$uid.service"',
                              'systemctl --user start dbus.service',
                              'systemctl --user is-active --quiet dbus.socket dbus.service',
                              "stat -c '%u:%a'", 'test -S "$XDG_RUNTIME_DIR/bus"',
                              "NameHasOwner s org.freedesktop.systemd1)",
                              "GetConnectionUnixUser s org.freedesktop.systemd1)",
                              "= 'b true'", '= "u $uid"', '"DBUS_SESSION_BUS_ADDRESS=$DBUS_SESSION_BUS_ADDRESS"'):
                    self.assertIn(proof, preparation)
                self.assertNotIn('dbus-run-session', preparation)
                self.assertLess(preparation.index('systemctl --user start dbus.service'),
                                preparation.index('NameHasOwner s org.freedesktop.systemd1)'))
                self.assertLess(product.index('Prepare hosted Linux credentials and user systemd'), product.index('Build the current native CLI'))
                matrix = re.search(r'        include: \$\{\{ (.+) \}\}', product)[1]
            programs.append({'target': 'x86_64-pc-windows-msvc' if product_job == 'startup'
                             else 'x86_64-unknown-linux-gnu', 'program': body, 'matrix': matrix})
        # Execute only the workflow verifier with synthetic bytes; never install a product.
        check = r'''
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {readFileSync} from 'node:fs';
import {posix} from 'node:path';
import {runInNewContext} from 'node:vm';
const programs=JSON.parse(readFileSync(0,'utf8'));
for(const {matrix} of programs) if(matrix) {
 for(const platform of ['all','windows-latest','macos-15-intel','ubuntu-latest']) {
  const selected=JSON.parse(JSON.stringify(runInNewContext(matrix,{inputs:{platform},fromJSON:JSON.parse})));
  assert.deepEqual(selected.map(row=>row.os),platform==='all'?['windows-latest','macos-15-intel','ubuntu-latest']:[platform]);
  for(const row of selected) assert.equal(row.target,{'windows-latest':'x86_64-pc-windows-msvc','macos-15-intel':'x86_64-apple-darwin','ubuntu-latest':'x86_64-unknown-linux-gnu'}[row.os]);
 }
}
const hash=bytes=>createHash('sha256').update(bytes).digest('hex');
for(const {target,program} of programs)for(const fault of ['', 'input', 'source', 'proof', 'image', 'core', 'target', 'version', 'installer']){
 const version='3.4.0-dev.1',suffix=target.includes('windows')?'.exe':'';
 const runtime=`/fixture/bin/runtime/${version}/rsrs-runtime${suffix}`;
 const core=Buffer.from('synthetic core inventory'),image=Buffer.from('synthetic Runtime; never executed');
 const metadata={schema_version:2,runtime_version:version,target_triple:target,source_commit:(fault==='source'?'b':'a').repeat(40),
  binary_sha256:hash(image),files:[{path:'core-runtime.json',sha256:hash(core)}]};
 if(fault==='target')metadata.target_triple='wrong-target';
 const encoded=JSON.stringify(metadata);
 const receipt={version:fault==='version'?'3.4.0-dev.2':version,sha256:hash(image),release:{metadata_json:encoded,metadata_sha256:fault==='proof'?'0'.repeat(64):hash(encoded)}};
 const files=new Map([['test-support/runtime-test-fixture.json',JSON.stringify({required:version})],['/fixture/bin/runtime.json',JSON.stringify(receipt)],
  [runtime,fault==='image'?Buffer.from('changed'):image],[`/fixture/bin/runtime/${version}/core-runtime.json`,fault==='core'?Buffer.from('changed'):core]]);
 const writes=[];
 const sandbox={assert,createHash,join:posix.join,resolve:()=>'/cli',spawnSync:()=>assert.fail('No product execution in self-check'),
  mkdtempSync:()=>'/fixture',realpathSync:value=>value,statSync:()=>({isFile:()=>true}),
  readFileSync:file=>{assert(files.has(file),`Unexpected file ${file}`);return files.get(file);},appendFileSync:(file,value)=>writes.push(value),
  installPinnedRuntime:async()=>({status:fault==='installer'?1:0}),
  process:{env:{GITHUB_ACTIONS:'true',RUNTIME_FIXTURE_HOSTED:'true',EXPECTED_RUNTIME_SOURCE_SHA:fault==='input'?'':'a'.repeat(40),
   SDK_TARGET:target,EXE_SUFFIX:suffix,RUNNER_TEMP:'/temp',GITHUB_ENV:'/output'}}};
 const run=()=>runInNewContext('(async()=>{'+program+'})()',sandbox);
 if(fault){await assert.rejects(run);assert.equal(writes.length,0);}
 else {await run();assert.deepEqual(writes,[`RSRS_RUNTIME_BIN=${runtime}\n`]);}
}
console.log('PASS: both workflow verifiers bind explicit source and original receipt/image/core hashes; no product executed');
'''
        result = subprocess.run(['node', '--input-type=module', '-e', check], input=json.dumps(programs),
                                capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_invalid_installation_never_rebinds_runtime(self):
        secret = 'private-installer-sentinel'
        for stdout, envelope in ((secret, 'invalid_json'), (b'\xff', 'invalid_json'),
                                 ('null', 'non_object'), ('[]', 'non_object')):
            diagnostic = runtime_install_diagnostic(types.SimpleNamespace(returncode=7, stdout=stdout, stderr=secret))
            self.assertEqual(diagnostic, {'exit_code': 7, 'structured_reason': 'unknown',
                'category': 'unclassified', 'envelope': envelope})
        for reason in (secret, {'token': secret}, [secret], None, 'invalid_input', 'runtime_error', 'request_outcome_unknown'):
            stdout = json.dumps({'summary': {'reason': reason}, 'errors': [secret], 'details': {'path': secret}})
            diagnostic = runtime_install_diagnostic(types.SimpleNamespace(returncode=1, stdout=stdout, stderr=secret))
            self.assertEqual(diagnostic['structured_reason'], reason if type(reason) is str and reason in
                ('invalid_input', 'runtime_error', 'request_outcome_unknown') else 'unknown')
            self.assertEqual(diagnostic['category'], 'unclassified')
            self.assertNotIn(secret, json.dumps(diagnostic))
        for fault in ('exit', 'image', 'target', 'proof', 'version', 'existing', 'host', 'outside'):
            with self.subTest(fault=fault):
                self.run_fixture(fault)
