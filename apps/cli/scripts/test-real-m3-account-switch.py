"""Read-only real M3 cache; disposable profiles and owned runtime port only."""
import argparse, faulthandler, hashlib, json, os, pathlib, secrets, socket, sqlite3, subprocess, sys, tempfile, time, urllib.request
from native_fixture_process import (install_official_runtime, install_published_runtime, stage_fixture_cli, FixtureCancelled, ForegroundOverlapProbe, WorkerInspector, captured_run,
    add_runtime_arguments, runtime_inputs, runtime_report, verify_runtime_health, process_start_id, canonical_scope_path, same_existing_path)
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from contextlib import closing
from types import SimpleNamespace
from m3_account_library import (
    assert_database_checkpoint_preserved,
    assert_historical_database_seed,
    custody_consumer_error,
    database_checkpoint_state,
    legacy_custody_alias,
    library_state,
    migrate_fixture_custody,
    recover_fixture_urk,
    verify_fixture_custody,
)
from m3_account_lifecycle import seed_accounts, verify_account_switches, verify_reopen_and_restart
from m3_account_resident_stress import verify_resident_stress

parser=argparse.ArgumentParser()
parser.add_argument('--binary',action='append',required=True)
parser.add_argument('--model-dir',required=True)
parser.add_argument('--model-file',choices=['model_quantized.onnx','model_fp16.onnx'],default='model_quantized.onnx')
parser.add_argument('--legacy-model-file',choices=['model_fp16.onnx','model_quantized.onnx'],default='model_fp16.onnx')
parser.add_argument('--legacy-model-dir',help='Read-only baseline cache containing real FP16 and quantized files')
parser.add_argument('--output-dir',required=True)
architecture=parser.add_mutually_exclusive_group()
architecture.add_argument('--expect-in-process',action='store_true',help='Historical binaries without an inference worker')
architecture.add_argument('--expect-supervised-worker',action='store_true',help='Verify one owned worker and its lifetime/SQLite boundary')
parser.add_argument('--resident-stress',action='store_true',help='Exercise concurrent recall, short writes, lightweight reads and background indexing')
parser.add_argument('--expect-background-threads',type=int,choices=range(1,17),help='Verify the actual loaded worker foreground/background policy')
parser.add_argument('--seed-binary',help='Create both libraries with this published older binary, then verify the database upgrade')
parser.add_argument('--seed-source',help='Verify and record the exact historical release tag used for a shard')
parser.add_argument('--client2-binary', type=pathlib.Path, help='Install and start Runtime using the verified desktop executable')
parser.add_argument('--client2-sha256')
parser.add_argument('--database-dev-checkpoint', choices=['v1.0.11-dev.1', 'v1.0.17-dev.6'])
add_runtime_arguments(parser)
args=parser.parse_args()
if args.seed_source and not args.seed_binary:
    parser.error('--seed-source requires --seed-binary')
if args.database_dev_checkpoint and not args.seed_binary:
    parser.error('--database-dev-checkpoint requires an authenticated historical --seed-binary')
database_upgrade = bool(args.seed_binary and args.runtime_binary)
if args.client2_binary or args.client2_sha256:
    if not (database_upgrade and args.client2_binary and args.client2_sha256 and os.name == 'nt'):
        parser.error('Client2 requires verified Windows desktop bytes and a historical database source')
    args.client2_binary = args.client2_binary.resolve(strict=True)
    assert hashlib.sha256(args.client2_binary.read_bytes()).hexdigest() == args.client2_sha256, 'desktop digest differs'
if database_upgrade and (args.expect_in_process or args.expect_supervised_worker
        or args.resident_stress or args.expect_background_threads is not None):
    parser.error('historical database upgrades leave worker and lifecycle fault checks to the resident fixture')
runtime_config=args
if sys.platform.startswith('linux') and args.seed_binary and os.environ.get('RSRS_SS_FIXTURE_BUS')!='private':
    os.execvpe('dbus-run-session', ['dbus-run-session','--',sys.executable,*sys.argv],
        dict(os.environ,RSRS_SS_FIXTURE_BUS='private'))
faulthandler.enable()
# A cloud container's PID 1 may not reap orphans. Adopt workers after a forced
# host termination so this fixture can verify and reap their actual exit.
if sys.platform.startswith('linux'):
    import ctypes
    libc=ctypes.CDLL(None,use_errno=True)
    if libc.prctl(36,1,0,0,0)!=0:  # PR_SET_CHILD_SUBREAPER
        raise OSError(ctypes.get_errno(),'cannot enable fixture child subreaper')
# Asynchronous stack dumps can race live frames (CPython #116008); fixture calls retain hard deadlines.
BASE=pathlib.Path(args.output_dir).resolve()
BASE.mkdir(parents=True,exist_ok=True)
import cryptography, importlib.metadata, importlib.util, psutil
vault_spec=importlib.util.spec_from_file_location('upgrade_vault_crypto', pathlib.Path(__file__).with_name('dev-legacy-vault-smoke.py'))
vault_crypto=importlib.util.module_from_spec(vault_spec)
vault_spec.loader.exec_module(vault_crypto)
(BASE/'fixture-dependencies.json').write_text(json.dumps({
    'python':sys.version,'cryptography':cryptography.__version__,'psutil':psutil.__version__,
    'psutil_source':json.loads(importlib.metadata.distribution('psutil').read_text('direct_url.json') or 'null'),
},indent=2),encoding='utf-8')
CACHE=pathlib.Path(args.model_dir).resolve()
BINARIES=[pathlib.Path(path).resolve() for path in args.binary]
runtime_inputs(args,BINARIES)
MODEL_HASHES={'model_quantized.onnx':'0826f8c1ab9edf1801db86c61919d4d108e8bfc0b809ec823ad366882ff0b77d',
    'model_fp16.onnx':'4f1a646a3d4f39985589e9991a717044ede8278617fe55e3d246838bc05055e9'}
def verify_model(cache,model):
    for name,expected in [('tokenizer.json','6710678b12670bc442b99edc952c4d996ae309a7020c1fa0096dd245c2faf790'),('onnx/'+model,MODEL_HASHES[model])]:
        with (cache/name).open('rb') as stream: assert hashlib.file_digest(stream,'sha256').hexdigest()==expected
verify_model(CACHE,args.model_file)
LEGACY_CACHE=pathlib.Path(args.legacy_model_dir).resolve() if args.legacy_model_dir else CACHE
if args.seed_binary:
    verify_model(LEGACY_CACHE,args.legacy_model_file)
    if args.model_file=='model_quantized.onnx': verify_model(LEGACY_CACHE,'model_quantized.onnx')
reports = []
for binary in BINARIES:
    if args.runtime_binary: args.runtime_identity_receipts=[]
    root = BASE / ('account-return-real-' + str(time.time_ns()))
    if os.name == 'posix' and sys.platform != 'darwin':
        # Keep fixture paths short and isolated from the CI report
        # directory for cross-platform tools; keep the owned
        # fixture in a short workspace directory and retain reports in BASE.
        root = pathlib.Path(os.environ['RUNNER_TEMP'] if args.seed_binary else pathlib.Path.cwd()) / ('rsrs-up-' + secrets.token_hex(6))
    main = root / '.rsrs'
    alternate = main / 'accounts/alternate'
    root.mkdir(mode=0o700)
    main.mkdir(mode=0o700)
    (main / 'accounts').mkdir(mode=0o700)
    alternate.mkdir(mode=0o700)
    entropy = secrets.token_bytes(18)
    code = 'A3-' + '-'.join(entropy.hex().upper()[i:i+6] for i in range(0,36,6))
    with socket.socket() as listener:
        listener.bind(('127.0.0.1',0)); port = listener.getsockname()[1]
    env = {key: value for key, value in os.environ.items()
           if key not in ('GH_TOKEN', 'GITHUB_TOKEN')}
    for suffix in ['CLIENT_ONLY','NO_AUTOSTART','RUNTIME_WORKER','CORE_TEST_MODE','RPC_TOKEN','ENGINE','ADDR','TOKEN','JSON','ORT_DEBUG',
                   'SUPER','PASS','SECRET','KDF_SALT','WRAPPED_URK','URK_NONCE']:
        for prefix in ['RSRS_', 'ONEMEMORY_', 'RESPIRE_']:
            env.pop(prefix + suffix,None)
    env.update(HOME=str(root), USERPROFILE=str(root), RSRS_DATA_DIR=str(main), RSRS_RPC_PORT=str(port),
        RSRS_SUPER=code,RSRS_NO_AUTOSYNC='1',RSRS_UPDATE_CHECK='0',RSRS_M3_DIR=str(CACHE))
    if args.seed_binary: env.pop('RSRS_SUPER')
    env['RSRS_RUNTIME_BIN']=str(args.runtime_binary or binary)
    if args.runtime_binary: env['LC_ALL']='C'
    runtime_installed_after_legacy_data = False
    runtime_artifact_reused = False
    stale_endpoint_archived = False
    keys=None
    custody_slots={}
    custody_receipts={}
    components_started=False
    network_smoke=None
    network_evidence=None
    def run(*args, allow_failure=False, timeout=120, cancel=None):
        result = captured_run([str(binary),'--json',*args],env=env,timeout=timeout,cancel=cancel)
        if result.returncode and not allow_failure: raise RuntimeError(result.stdout.decode(errors='replace')+result.stderr.decode(errors='replace'))
        envelope=json.loads(result.stdout)
        envelope['_fixture_timing']=result.fixture_timing
        command=next((word for word in args if not word.startswith('-')),None)
        if runtime_config.runtime_binary and result.returncode==0 and command in ('model','remember','recall','show','doctor','status'):
            observe_runtime()
        return envelope
    def run_client2(action):
        desktop_env = {key: value for key, value in env.items() if key != 'RSRS_RUNTIME_BIN'}
        result = captured_run([str(args.client2_binary),'--headless',action],env=desktop_env,timeout=960)
        assert result.returncode==0, f'Client2 {action} failed with exit {result.returncode}'
        assert json.loads(result.stdout).get('ok') is True, f'Client2 {action} did not confirm success'
    def wait_index_ready(timeout=180):
        deadline = time.monotonic() + timeout
        while True:
            doctor = run('doctor', allow_failure=True)
            rows = {item['name']: item for item in doctor['items'] if item['name'] in ['embedder', 'model index']}
            if len(rows) == 2 and all(item['status'] == 'ok' for item in rows.values()):
                return rows
            assert time.monotonic() < deadline, rows
            time.sleep(.25)

    def snapshot(profile):
        database = profile/'rsrs.db'
        if not database.exists(): database = profile/'onememory.db'
        with closing(sqlite3.connect(database)) as db:
            assert db.execute('PRAGMA integrity_check').fetchall() == [('ok',)], 'database integrity check failed'
            rows = [db.execute(query).fetchall() for query in ['SELECT id,ciphertext,nonce,dirty,deleted FROM memories ORDER BY id',
                'SELECT * FROM sync_outbox ORDER BY seq','SELECT * FROM core_artifacts ORDER BY memory_id,model']]
        artifacts = sorted((path.name,hashlib.sha256(path.read_bytes()).hexdigest()) for path in (profile/'core-index').iterdir())
        return (rows,artifacts,(profile/'session.json').read_bytes())
    def observe_runtime(health=None):
        if health is None:
            with urllib.request.urlopen(urllib.request.Request(f'http://127.0.0.1:{port}/api/health',headers={'X-Respire-Health':'runtime-v1'}),timeout=2) as response:
                health=json.load(response)
        profile=pathlib.Path(json.loads((main/'client.json').read_text())['data_dir']) if (main/'client.json').exists() else main
        verify_runtime_health(runtime_config,health,main,profile)
        return health
    old_runtime=None
    old_rpc_checked=False
    old_rpc_authenticated=False
    legacy_client_rejected=False
    source_rpc_checked=False
    legacy_handoff_actor=None
    live_owner_restart_rejected=False
    public_config={'addr':'https://fixture.invalid','autosync':False,'custom':'preserve-upgrade-fixture'}
    def select(profile):
        (main/'client.json').write_text(json.dumps({**public_config,'data_dir':str(profile)}),encoding='utf-8')
    worker_identities=[]
    inspector=None
    stop_failure=None
    def checked_worker(health):
        global inspector
        if args.runtime_binary: observe_runtime(health)
        if sys.platform=='win32':
            if inspector is None:
                inspector=WorkerInspector(root/'boundary-inspector')
            identity=inspector.inspect({'host_pid':health['pid'],
                'worker_pid':health['inference']['worker_pid'],'binary':str(binary),
                'runtime_binary':str(args.runtime_binary) if args.runtime_binary else None,
                'process_start_id':health.get('process_start_id')})
            if identity not in worker_identities: worker_identities.append(identity)
            return identity
        import psutil
        host=psutil.Process(health['pid'])
        children=[child for child in host.children(recursive=True)
                  if not (sys.platform=='win32' and child.name().lower()=='conhost.exe')]
        assert len(children)==1, 'expected exactly one supervised Core worker'
        worker=children[0]
        assert worker.pid==health['inference']['worker_pid'] and worker.ppid()==host.pid
        assert '--core-worker-internal' in worker.cmdline()
        assert same_existing_path(worker.exe(),host.exe())
        assert not any(pathlib.Path(file.path).name in ('rsrs.db','lock.db') for file in worker.open_files())
        assert not {'GH_TOKEN','GITHUB_TOKEN','OPENAI_API_KEY','RSRS_SUPER'} & worker.environ().keys()
        identity=(worker.pid,worker.create_time())
        if identity not in worker_identities: worker_identities.append(identity)
        return identity
    def confirm_workers_exited(identities):
        import psutil
        deadline=time.monotonic()+10
        for pid,created in identities:
            while True:
                try: alive=psutil.Process(pid).create_time()==created
                except psutil.NoSuchProcess: alive=False
                if not alive: break
                if sys.platform.startswith('linux'):
                    try: reaped,_=os.waitpid(pid,os.WNOHANG)
                    except ChildProcessError: reaped=0
                    if reaped==pid: break
                assert time.monotonic()<deadline, 'owned Core worker survived its host: '+str(pid)
                time.sleep(.02)
    try:
        if sys.platform == 'darwin' or args.seed_binary:
            keyring_script='dev-migration-secret-service.py' if sys.platform.startswith('linux') else 'dev-migration-keyring.py'
            keyring_spec=importlib.util.spec_from_file_location('upgrade_native_keyring',
                pathlib.Path(__file__).with_name(keyring_script))
            keyring_backend=importlib.util.module_from_spec(keyring_spec)
            keyring_spec.loader.exec_module(keyring_backend)
            keys=keyring_backend.create(root/'keyring-home')
            if sys.platform.startswith('linux'): keys.configure_business_env(env)
            else: keys.configure_env(env)
        components_started=True
        if not (args.seed_binary and args.runtime_binary):
            installed = install_published_runtime(binary, env)
            if installed is None and args.runtime_binary:
                installed = install_official_runtime(binary, env, args.runtime_version, args.runtime_sha256)
            if installed is not None: args.runtime_binary = installed
        (expected_urks, recovery_factors, seed_vault_versions, seed_env, seed_version,
         original, original_state, checkpoint_before, global_engine) = seed_accounts(
            args, main, alternate, binary, code, env, LEGACY_CACHE, keys, custody_slots,
            run, select, snapshot, wait_index_ready)
        select(main)
        if args.seed_binary and not database_upgrade:
            log=(root/'old-runtime.log').open('wb')
            web_help=subprocess.check_output([args.seed_binary,'web','--help'],env=seed_env,text=True,encoding='utf-8',timeout=30)
            runtime_args=['web','--internal','--no-open','--port',str(port)] if '--port' in web_help else ['--runtime-internal']
            old_runtime=subprocess.Popen([str(pathlib.Path(args.seed_binary).resolve()),*runtime_args],env=seed_env,stdout=log,stderr=log)
            log.close()
            for attempt in range(200):
                assert old_runtime.poll() is None,('published old runtime exited before upgrade (exit '+str(old_runtime.returncode)+'): '+
                    (root/'old-runtime.log').read_text(encoding='utf-8',errors='replace')[-4000:])
                try:
                    headers={}
                    token=main/'runtime/token'
                    if token.exists(): headers['Authorization']='Bearer '+token.read_text().strip()
                    request=urllib.request.Request(f'http://127.0.0.1:{port}/api/health',headers=headers)
                    with urllib.request.urlopen(request,timeout=1) as response: health=json.load(response)
                    assert health['pid']==old_runtime.pid
                    break
                except OSError: time.sleep(.1)
            else: raise RuntimeError('published old runtime did not become ready')
            # Historical components prepare data; only the upgraded stack is tested next.
            original_token=token.read_bytes() if token.is_file() else None
            legacy_records={path:(path.read_bytes() if path.is_file() else None) for path in
                [main/'runtime/endpoint.json',token,main/'client.json',main/'inference.json',
                 main/'runtime/autostart/registration.json']}
            legacy_data={str(profile):snapshot(profile) for profile in [main,alternate]}
            old_owner=psutil.Process(old_runtime.pid)
            old_birth=old_owner.create_time()
            old_start=process_start_id(old_runtime.pid)
            assert same_existing_path(old_owner.exe(),args.seed_binary)
            if not args.runtime_binary:
                old_status=run('--client-only','status')
                assert old_status.get('status')=='ok','new client could not read the old runtime through HTTP RPC'
                assert old_runtime.poll() is None,'client-only RPC replaced or stopped the old runtime'
                request=urllib.request.Request(f'http://127.0.0.1:{port}/api/health',headers=headers)
                with urllib.request.urlopen(request,timeout=2) as response: after_rpc=json.load(response)
                assert after_rpc['pid']==old_runtime.pid,'client-only RPC changed runtime ownership'
                assert (token.read_bytes() if token.is_file() else None)==original_token,'client-only RPC changed the old token file'
                old_rpc_checked=True
                old_rpc_authenticated=bool(original_token and original_token.strip())
            if args.runtime_binary:
                # Test setup owns this historical child. This is not product takeover evidence.
                old_children=[(child.pid,child.create_time()) for child in old_owner.children(recursive=True)]
                assert process_start_id(old_runtime.pid)==old_start
                old_runtime.terminate()
                old_runtime.wait(timeout=15)
                confirm_workers_exited([(old_runtime.pid,old_birth),*old_children])
                for path in [main/'runtime/endpoint.json',token]:
                    before=legacy_records[path]
                    if path.exists():
                        assert before is not None and path.read_bytes()==before
                        path.unlink()
                old_runtime=None
                legacy_handoff_actor='fixture'
        if args.seed_binary and args.runtime_binary:
            # Historical writers have exited. Reuse the independently verified
            # artifact; installation/download behavior is a per-target host check.
            legacy_data={str(profile):snapshot(profile) for profile in [main,alternate]}
            endpoint = main/'runtime/endpoint.json'
            endpoint.parent.mkdir(exist_ok=True)
            # The recorded PID is live but belongs to this test driver, while the old port is free.
            stale_endpoint = json.dumps({'v':1,'pid':os.getpid(),'url':f'http://127.0.0.1:{port}'}).encode()
            with endpoint.open('xb') as stream:
                stream.write(stale_endpoint)
            if args.client2_binary:
                obsolete_receipt = main/'runtime.json'
                with obsolete_receipt.open('xb') as stream:
                    stream.write(b'obsolete Runtime selection')
                cli_path = main/'bin/rsrs.exe'
                assert not cli_path.exists(), 'desktop Runtime preparation must start without a CLI'
                run_client2('install-runtime')
                assert not cli_path.exists(), 'desktop Runtime preparation unexpectedly installed the CLI'
                assert obsolete_receipt.read_bytes()==b'obsolete Runtime selection', 'desktop changed the obsolete receipt'
                installed = main/'bin/runtime'/args.runtime_version/'rsrs-runtime.exe'
                assert hashlib.sha256(installed.read_bytes()).hexdigest()==args.runtime_sha256, 'desktop installed different Runtime bytes'
                env['RSRS_RUNTIME_BIN'] = str(installed)
                stage_fixture_cli(binary, env, main)
            else:
                installed = install_published_runtime(binary, env)
            assert installed is not None, 'database upgrade requires the verified managed or published Runtime artifact'
            args.runtime_binary = installed
            assert endpoint.read_bytes()==stale_endpoint, 'Runtime preparation changed the old endpoint'
            assert {str(profile):snapshot(profile) for profile in [main,alternate]} == legacy_data, 'Runtime installation changed old account data'
            runtime_installed_after_legacy_data = True
            runtime_artifact_reused = True
        if args.seed_binary:
            # The desktop case leaves the main session for the actual startup host to migrate.
            for profile in [main,alternate]:
                if args.client2_binary and profile == main:
                    continue
                before=snapshot(profile)
                migrate_fixture_custody(profile,keys,custody_slots[str(profile)],expected_urks[str(profile)])
                after=snapshot(profile)
                assert after[:2]==before[:2], 'fixture custody changed historical database or indexes'
                assert library_state(after,recovery_factors[str(profile)])==original_state[str(profile)], 'fixture custody changed original records, sync work or URK'
        if args.seed_binary and args.runtime_binary:
            if args.client2_binary:
                run_client2('start')
                desktop_owner = observe_runtime()
                assert desktop_owner['store_ready'], 'Client2 did not unlock the historical database'
                run('--client-only','status')
                run_client2('rpc-status')
                assert observe_runtime()['instance_id']==desktop_owner['instance_id'], 'the two hosts changed the serving Runtime'
            else:
                run('runtime','start')
            archived = list(endpoint.parent.glob('endpoint.stopped-*.json'))
            assert len(archived)==1 and archived[0].read_bytes()==stale_endpoint, 'startup did not archive the old endpoint'
            assert endpoint.read_bytes()!=stale_endpoint, 'new Runtime still uses the old endpoint record'
            stale_endpoint_archived = True
            run('--direct','model','engine','cpu')
            stopped=captured_run([str(binary),'--runtime-internal','--stop'],env=env,timeout=50)
            if stopped.returncode:
                stop_failure={'phase':'direct_engine_preparation','returncode':stopped.returncode,
                    'timing':dict(stopped.fixture_timing),
                    'runtime_identity_receipt_index':len(args.runtime_identity_receipts)-1,
                    'stdout':'redaction_unavailable','stderr':'redaction_unavailable'}
                try:
                    # Reuse the existing bounded credential redactor; never attach raw output or fixture files.
                    support_spec=importlib.util.spec_from_file_location('stop_diagnostic_support',
                        pathlib.Path(__file__).with_name('dev-legacy-vault-smoke.py'))
                    support=importlib.util.module_from_spec(support_spec)
                    support_spec.loader.exec_module(support)
                    redactor=object.__new__(support.Smoke)
                    redactor.diagnostic_secrets={code,entropy.hex()}
                    for factors in recovery_factors.values():
                        redactor.diagnostic_secrets.update(factors.values())
                    for profile in (main,alternate):
                        redactor.diagnostic_secrets.update(value for value in
                            json.loads((profile/'session.json').read_bytes()).values()
                            if isinstance(value,str) and value)
                    for stream in ('stdout','stderr'):
                        body=getattr(stopped,stream)
                        output=subprocess.CompletedProcess(stopped.args,stopped.returncode,
                            body if stream=='stdout' else b'',body)
                        stop_failure[stream]=redactor.redacted_cli_error(env,output)
                except Exception:
                    # Redaction failure must neither publish raw bytes nor hide the original stop failure.
                    pass
            assert stopped.returncode==0,'direct engine preparation runtime stop failed'
            global_engine=(main/'inference.json').read_bytes()
        statuses, generation_changes = verify_account_switches(
            args, main, alternate, binary, env, port, BASE, database_upgrade, global_engine,
            public_config, old_runtime, keys, custody_slots, custody_receipts, expected_urks,
            original, original_state, recovery_factors, worker_identities, run, snapshot,
            wait_index_ready, checked_worker, confirm_workers_exited)
        stress = None
        if args.resident_stress:
            stress = verify_resident_stress(args, main, port, BASE, run, observe_runtime,
                                            checked_worker, wait_index_ready)
        (normal_reopen_checked, restart_checked, live_owner_restart_rejected,
         checkpoint_evidence) = verify_reopen_and_restart(
            args, main, alternate, binary, env, port, database_upgrade, recovery_factors, keys,
            custody_slots, custody_receipts, expected_urks, checkpoint_before, run, run_client2,
            snapshot, wait_index_ready, observe_runtime, checked_worker, confirm_workers_exited)
        if database_upgrade:
            # All offline evidence above is complete before any account/network changes.
            network_smoke=vault_crypto.Smoke(SimpleNamespace(root=root/'historical-sync', binary=binary,
                binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                source_sha=os.environ['CLI_SHA'], version=os.environ['CLI_VERSION']))
            network_smoke.keys=keys
            network_smoke.model=CACHE
            network_evidence=network_smoke.historical_database_sync(env, expected_urks[str(main)], code,
                original_state[str(main)][0])
            assert library_state(snapshot(alternate),recovery_factors[str(alternate)])==original_state[str(alternate)], 'network stage changed the offline alternate library'
            if args.database_dev_checkpoint:
                current=snapshot(main)
                active=[(row[0],vault_crypto.decrypt_content(expected_urks[str(main)],row[1],row[2]),row[3],row[4])
                    for row in current[0][0] if row[1]]
                state=database_checkpoint_state(main,args.database_dev_checkpoint,current,active)
                before=checkpoint_before[str(main)]
                if before['kind']=='legacy_relation_payload':
                    live_ids={row[0] for row in original_state[str(main)][0] if not row[3]}
                    before={**before,'links':[row for row in before['links'] if row[0] in live_ids]}
                assert_database_checkpoint_preserved(before,state)
                checkpoint_evidence['post_sync_special_state_preserved']=True
                checkpoint_evidence['sync_verified_accounts']=['main']
            assert run('recall','Historical reverse synchronization','--titles').get('items'), 'verified source owner lost reverse marker'
        seed_identity = ({'seed_source': 'v' + seed_version,
                          'seed_sha256': hashlib.sha256(pathlib.Path(args.seed_binary).read_bytes()).hexdigest()}
                         if args.seed_source else {})
        reports.append({'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'binary':str(binary),'passed':True,
            'real_model':True,'model_file':args.model_file,'engine':'cpu','in_process_checked':args.expect_in_process,
            'supervised_worker_checked':args.expect_supervised_worker,'owned_worker_identities':worker_identities,
            'global_engine_preserved':not database_upgrade,'distinct_vault_keys':True,'sequence':statuses,
            'coverage_scope':'historical-database' if database_upgrade else 'current-host',
            'host_checks_delegated_to':'resident-concurrency' if database_upgrade else None,
            'normal_reopen_checked':normal_reopen_checked,'runtime_artifact_reused':runtime_artifact_reused,
            'stale_endpoint_archived':stale_endpoint_archived,
            'client2_sha256':args.client2_sha256,
            'client2_native_credential_migration':bool(args.client2_binary),
            'original_records_preserved':True,'original_sync_work_preserved':True,'urk_preserved':True,
            'old_records_decrypted':True,'post_upgrade_recall_checked':True,
            'encrypted_tombstones_preserved':bool(args.seed_binary),
            'post_upgrade_sync_checked':False,
            'post_upgrade_sync':network_evidence,
            'seed_vault_versions':seed_vault_versions,
            'database_dev_checkpoint':checkpoint_evidence,
            'host_credential_custody':{'required':bool(args.seed_binary),'verified_accounts':list(custody_receipts.values())},
            'historical_credential_migration_actor':'fixture' if args.seed_binary else None,
            'resident_stress':stress,
            'upgrade_contract':'standalone-v1' if args.runtime_binary else 'legacy-shim-v1',
            'source_rpc_checked':source_rpc_checked,'legacy_handoff_actor':legacy_handoff_actor,
            'runtime_installed_after_legacy_data':runtime_installed_after_legacy_data,
            'live_owner_restart_rejected':live_owner_restart_rejected,
            'forced_restart_checked':restart_checked and not bool(args.runtime_binary),
            'safe_restart_checked':restart_checked and bool(args.runtime_binary),
            'legacy_client_rejected':legacy_client_rejected,'old_rpc_checked':old_rpc_checked,
            'old_rpc_authenticated':old_rpc_authenticated,'initial_generation_changes':generation_changes,'fixture':str(root),
            **seed_identity,**runtime_report(args,True)})
    except Exception as error:
        reports.append({'binary':str(binary),'passed':False,'error':str(error)[:2000],
            **({'runtime_stop_failure':stop_failure} if stop_failure is not None else {})})
        (BASE/'real-account-return-verification.json').write_text(json.dumps(reports,indent=2),encoding='utf-8')
        raise
    finally:
        cleanup_errors=[]
        try:
            if inspector is not None: inspector.close()
        except Exception as error: cleanup_errors.append('inspector: '+str(error))
        try:
            if components_started:
                stopped=captured_run([str(binary),'--runtime-internal','--stop'],env=env,timeout=50)
                assert stopped.returncode==0,'owned runtime stop failed'
        except Exception as error: cleanup_errors.append('runtime: '+str(error))
        try:
            if args.expect_supervised_worker: confirm_workers_exited(worker_identities)
        except Exception as error: cleanup_errors.append('worker reap: '+str(error))
        try:
            if old_runtime is not None and old_runtime.poll() is None:
                old_runtime.terminate(); old_runtime.wait(timeout=15)
        except Exception as error: cleanup_errors.append('old runtime: '+str(error))
        if network_smoke is not None:
            try:
                network_smoke.cleanup()
                reports[-1]['cloud_cleanup']=network_smoke.report['cloud_cleanup']
                assert reports[-1]['cloud_cleanup']['passed'] is True, 'historical sync cloud cleanup failed'
            except Exception as error:
                cleanup_errors.append('cloud: '+str(error))
            reports[-1]['post_upgrade_sync_diagnostic']={field:network_smoke.report[field]
                for field in ('stage','server_sha','cli_failure','provider_observation',
                    'identity_preparation') if field in network_smoke.report}
        if keys is not None:
            try:
                expected_slots=len(keys.entries) if network_smoke is not None else None
                reports[-1]['native_credential_cleanup']=keys.cleanup()
                provider=reports[-1]['native_credential_cleanup'].get('provider_daemon')
                if network_smoke is not None and provider is not None and (provider.get('started') is not True
                        or provider.get('exit_code_at_cleanup_start') is not None):
                    reports[-1]['native_credential_cleanup'].update(passed=False,
                        failure_code='original_provider_not_running_at_cleanup')
                assert reports[-1]['native_credential_cleanup'].get('passed') is True \
                    and not reports[-1]['native_credential_cleanup'].get('remaining_entries'), 'native credential cleanup failed'
                custody=reports[-1].get('host_credential_custody')
                if custody is not None and custody['required']:
                    if expected_slots is None:
                        expected_slots=sum(len(plan[2]) for plan in custody_slots.values())
                    assert len(reports[-1]['native_credential_cleanup']['events'])==expected_slots, 'custody cleanup omitted an owned slot'
                    custody['cleanup_passed']=True
            except Exception as error:
                reports[-1].setdefault('native_credential_cleanup',
                    {'passed':False,'failure_code':type(error).__name__})
                cleanup_errors.append('native credentials: '+str(error))
        if cleanup_errors:
            reports[-1]['passed']=False
            reports[-1]['cleanup_errors']=cleanup_errors
        if network_evidence and reports[-1]['passed']:
            reports[-1]['post_upgrade_sync_checked']=(same_existing_path(network_evidence.get('profile'),main)
                and all(network_evidence.get(field) is True for field in ('original_urk_preserved',
                    'existing_keys_registered','source_push','independent_pull_decrypted','reverse_marker_source_pull',
                    'original_active_preserved','tombstones_propagated','pending_work_acknowledged'))
                and reports[-1]['cloud_cleanup']['passed'] is True
                and reports[-1]['native_credential_cleanup']['passed'] is True)
        reports[-1].update(runtime_report(args,reports[-1]['passed']))
        if args.runtime_binary and not reports[-1]['standalone_runtime_evidence']:
            reports[-1]['passed']=False
        (BASE/'real-account-return-verification.json').write_text(json.dumps(reports,indent=2),encoding='utf-8')
        if cleanup_errors and sys.exc_info()[0] is None:
            raise RuntimeError('fixture cleanup failed: '+str(cleanup_errors))
    assert reports[-1]['passed'], 'fixture evidence incomplete'
    print('PASS '+str(binary)+': main -> alternate -> main; real CPU inference; original records decrypted and URKs preserved',flush=True)
(BASE/'real-account-return-verification.json').write_text(json.dumps(reports,indent=2),encoding='utf-8')
