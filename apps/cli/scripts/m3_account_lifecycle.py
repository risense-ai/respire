"""Seed disposable accounts and verify account switching and Runtime lifecycle."""
import json
import pathlib
import secrets
import subprocess
import sys
import tempfile
import time
import urllib.request

from cryptography.hazmat.primitives.ciphers.aead import AESGCM
import legacy_vault_fixture as vault_crypto

from m3_account_library import (
    assert_database_checkpoint_preserved,
    assert_historical_database_seed,
    database_checkpoint_state,
    legacy_custody_alias,
    library_state,
    verify_fixture_custody,
)
from native_fixture_process import captured_run, process_start_id, same_existing_path


def seed_accounts(args, main, alternate, binary, code, env, legacy_cache, keys,
                  custody_slots, run, select, snapshot, wait_index_ready):
    expected_urks={}
    recovery_factors={}
    seed_vault_versions=[]
    for profile in [main, alternate]:
        salt,nonce,urk=secrets.token_bytes(16),secrets.token_bytes(12),secrets.token_bytes(32)
        version=(1 if profile==main else 3) if args.seed_binary else 4
        factors={'super':code}
        session={'user':'fixture-'+str(time.time_ns()),'vault_version':version,'kdf_salt':salt.hex()}
        if version in (1,3):
            field='secret' if version==1 else 'secret_key'
            factors[field]=secrets.token_hex(32)
            if version==1: factors['pass']=secrets.token_urlsafe(32)
            session.update({field:factors[field], 'pass':factors['pass']} if version==1 else
                {field:factors[field], 'super':code})
            kek=vault_crypto.legacy_kek(version,factors['pass'] if version==1 else code,factors[field],salt.hex())
        else:
            kek=vault_crypto.v4_kek(code,salt.hex())
        session.update(wrapped_urk=AESGCM(kek).encrypt(nonce,urk,None).hex(),urk_nonce=nonce.hex())
        (profile/'session.json').write_text(json.dumps(session),encoding='utf-8')
        expected_urks[str(profile)]=urk
        recovery_factors[str(profile)]=factors
        seed_vault_versions.append(version)
    assert len(set(expected_urks.values()))==2, 'fixture accounts must have distinct URKs'
    seed_env={**env,'RSRS_M3_DIR':str(legacy_cache)}
    if args.seed_binary: seed_env['RSRS_RUNTIME_BIN']=str(pathlib.Path(args.seed_binary).resolve())
    # Historical artifacts predate RSRS_*; keep their inputs explicit and
    # isolated instead of accidentally testing the operator's default library.
    for key, value in list(seed_env.items()):
        if key.startswith(('RSRS_', 'ONEMEMORY_', 'RESPIRE_')):
            seed_env['ONEMEMORY_' + key[5:]] = value
    if not (args.seed_binary and args.runtime_binary):
        run('--direct','model','engine','cpu')
        stopped=captured_run([str(binary),'--runtime-internal','--stop'],env=env,timeout=50)
        assert stopped.returncode==0,'direct engine preparation runtime stop failed'
        global_engine=(main/'inference.json').read_bytes()
    for profile in [main,alternate]:
        select(profile)
        if args.seed_binary:
            def seed_command(*command):
                seeded=captured_run([str(pathlib.Path(args.seed_binary).resolve()),'--json','--direct',*command],env=seed_env,timeout=180)
                assert seeded.returncode==0,seeded.stdout.decode(errors='replace')+seeded.stderr.decode(errors='replace')
                return seeded
            seed_version = json.loads(seed_command('v').stdout)['summary']['version']
            if args.seed_source:
                assert 'v' + seed_version == args.seed_source, 'historical seed version differs from the shard source'
            # These published versions select M3 without activating an empty index.
            if seed_version in {'1.0.11-dev.1', '1.0.11', '1.0.12', '1.0.15', '1.0.16',
                                '1.0.17-dev.6', '1.0.18', '1.0.19', '1.0.20'}:
                assert not any((profile/name).exists() for name in ('rsrs.db','onememory.db')), 'historical seed profile is not empty'
            else:
                seed_command('model','activate','m3')
            seed_command('remember','Real account return fixture '+profile.name,'--title',profile.name,'--importance','important','--force')
            if args.database_dev_checkpoint == 'v1.0.11-dev.1':
                first_ids = {row[0] for row in snapshot(profile)[0][0]}
                assert len(first_ids) == 1, 'relation checkpoint requires exactly one original entry'
                seed_command('remember','Independent checkpoint companion for network routing policy',
                    '--title','checkpoint-companion','--importance','important','--force')
                peer_ids = {row[0] for row in snapshot(profile)[0][0]} - first_ids
                assert len(peer_ids) == 1, 'historical CLI did not create the relation companion'
                seed_command('remember','Replacement checkpoint conclusion preserving encrypted relationship fields',
                    '--title','checkpoint-replacement','--importance','important','--force',
                    '--supersedes',next(iter(first_ids)),'--see-also',next(iter(peer_ids)))
            elif args.database_dev_checkpoint == 'v1.0.17-dev.6':
                recalled = json.loads(seed_command('recall','Real account return fixture '+profile.name,'--limit','3').stdout)
                request = recalled['summary']['query_log_id']
                assert isinstance(request, str) and request, 'historical recall did not expose its request identity'
                memory_ids = [row[0] for row in snapshot(profile)[0][0]]
                assert len(memory_ids) == 1, 'feedback checkpoint requires its original candidate'
                seed_command('query-log','mark',memory_ids[0],'--good','--query-log-id',request,'--source','human')
            tombstone = json.loads(seed_command('remember','Historical encrypted deletion '+profile.name,
                '--title','upgrade-tombstone','--importance','important','--force').stdout)['summary']['id']
            assert isinstance(tombstone, str) and tombstone, 'historical tombstone seed has no identity'
            seed_command('forget',tombstone)
        else:
            run('--direct','remember','Real account return fixture '+profile.name,'--title',profile.name,'--importance','important','--force')
            if args.runtime_binary:
                # Durable remember may return before background embeddings finish.
                # Wait on this selected profile's live owner before stopping it.
                wait_index_ready()
                index_files=list((profile/'core-index').iterdir())
                assert index_files and all(path.is_file() and path.stat().st_size>0 for path in index_files), 'seed index artifacts missing or empty'
                # --direct forwards to a resident owner; drain it before raw fixture profile selection.
                stopped=captured_run([str(binary),'--runtime-internal','--stop'],env=env,timeout=50)
                assert stopped.returncode==0,'direct seed runtime stop failed'
    if not args.seed_binary:
        stopped=captured_run([str(binary),'--runtime-internal','--stop'],env=env,timeout=50)
        assert stopped.returncode==0,'direct seed runtime stop failed'
    original = {str(profile):snapshot(profile) for profile in [main,alternate]}
    assert [json.loads(original[str(profile)][2]).get('vault_version',1) for profile in [main,alternate]]==seed_vault_versions, 'historical preparation replaced the selected session formats'
    original_state={name:library_state(value,recovery_factors[name]) for name,value in original.items()}
    if args.seed_binary:
        for profile in [main, alternate]:
            assert_historical_database_seed(original[str(profile)])
    checkpoint_before = {}
    if args.database_dev_checkpoint:
        for profile in [main, alternate]:
            state = database_checkpoint_state(profile, args.database_dev_checkpoint,
                original[str(profile)], original_state[str(profile)][0])
            if state['kind'] == 'legacy_relation_payload':
                assert state['legacy_domain'] and any(row[1] for row in state['links']) \
                    and any(row[2] for row in state['links']) and any(row[3] for row in state['links']), \
                    'checkpoint lacks old-domain encrypted association state'
            else:
                assert state['witness'] and state['requests'] and state['feedback'] \
                    and not state['recovery_table'], 'checkpoint lacks its intermediate witness/feedback state'
                assert original_state[str(profile)][1], 'checkpoint has no pending synchronization work'
            checkpoint_before[str(profile)] = state
    assert all(state[2]==expected_urks[name] for name,state in original_state.items()), 'seed changed the fixture URK'
    if args.seed_binary:
        for profile in [main,alternate]:
            session_bytes=original[str(profile)][2]
            session=json.loads(session_bytes)
            assert session.get('keyring_account') is None, 'historical seed already selected a credential alias'
            version=session.get('vault_version',1)
            alias=legacy_custody_alias(profile,session_bytes)
            primary='pass' if version==1 else 'super'
            slots={primary:session[primary], 'legacy-secret':session['secret' if version==1 else 'secret_key']}
            if version==3 and 'pass' in session: slots['pass']=session['pass']
            for slot in slots: keys.reserve('rsrs',slot+':'+alias)
            custody_slots[str(profile)]=(alias,version,slots)
    return (expected_urks, recovery_factors, seed_vault_versions, seed_env,
            seed_version if args.seed_binary else None, original, original_state,
            checkpoint_before, global_engine if not (args.seed_binary and args.runtime_binary) else None)


def verify_account_switches(args, main, alternate, binary, env, port, output_dir,
                            database_upgrade, global_engine, public_config, old_runtime,
                            keys, custody_slots, custody_receipts, expected_urks,
                            original, original_state, recovery_factors, worker_identities,
                            run, snapshot, wait_index_ready, checked_worker, confirm_workers_exited):
    statuses=[]
    visited=set()
    generation_changes={}
    for account, profile in [('main',main),('alternate',alternate),('main',main)]:
        previous_workers=list(worker_identities)
        run('account','use',account)
        if args.expect_supervised_worker and previous_workers:
            confirm_workers_exited(previous_workers)
        if old_runtime is not None:
            assert old_runtime.wait(timeout=20)==0,'old runtime did not exit gracefully'
        assert run('model','engine')['summary']['engine']=='cpu'
        if not database_upgrade:
            assert (main/'inference.json').read_bytes()==global_engine
            assert not (alternate/'inference.json').exists()
        # A real model change may rebuild derived indexes once; never source data.
        rows = wait_index_ready()
        if args.seed_binary:
            custody_receipts[str(profile)]={'account':account, **verify_fixture_custody(
                profile,keys,custody_slots[str(profile)],expected_urks[str(profile)],binary,env)}
        if args.seed_binary and str(profile) not in visited:
            current=snapshot(profile)
            baseline=original[str(profile)]
            assert library_state(current,recovery_factors[str(profile)])==original_state[str(profile)], 'migration changed original records, sync work or URK'
            generation_changes[str(profile)]=current[0][2]!=baseline[0][2] or current[1]!=baseline[1]
            original[str(profile)]=current
        visited.add(str(profile))
        if args.expect_in_process or args.expect_supervised_worker:
            import psutil
            probe_stdout=tempfile.TemporaryFile()
            probe_stderr=tempfile.TemporaryFile()
            pending=subprocess.Popen([str(binary),'--json','model','probe','--model','m3'],env=env,stdout=probe_stdout,stderr=probe_stderr)
            checks=0
            deadline=time.monotonic()+120
            try:
                while pending.poll() is None:
                    assert time.monotonic()<deadline,'native probe exceeded the acceptance deadline'
                    start=time.monotonic()
                    with urllib.request.urlopen(urllib.request.Request(f'http://127.0.0.1:{port}/api/health',headers={'X-Respire-Health':'runtime-v1'}),timeout=2) as response:
                        health=json.load(response)
                    assert time.monotonic()-start < 2,'health blocked behind native inference'
                    if args.expect_supervised_worker:
                        identity=checked_worker(health)
                        if checks: assert identity==probe_worker,'warm native probe replaced its worker'
                        probe_worker=identity
                    else:
                        children=psutil.Process(health['pid']).children(recursive=True)
                        children=[child for child in children if not (sys.platform=='win32' and child.name().lower()=='conhost.exe')]
                        assert not children, 'runtime spawned an inference child: '+str([child.name() for child in children])
                    checks+=1
                    time.sleep(.05)
                pending.wait(timeout=5)
                probe_stdout.seek(0); probe_stderr.seek(0)
                stdout,stderr=probe_stdout.read(),probe_stderr.read()
                assert pending.returncode==0,stdout.decode(errors='replace')+stderr.decode(errors='replace')
                assert checks>0,'native inference responsiveness was not exercised'
            finally:
                if pending.poll() is None:
                    pending.kill(); pending.wait(timeout=5)
                probe_stdout.close(); probe_stderr.close()
        else:
            run('model','probe','--model','m3')
        if args.expect_background_threads is not None:
            # Model/index metadata may be ready before the worker heartbeat.
            deadline=time.monotonic()+10
            policy=None
            while policy is None and time.monotonic()<deadline:
                with urllib.request.urlopen(urllib.request.Request(f'http://127.0.0.1:{port}/api/health',headers={'X-Respire-Health':'runtime-v1'}),timeout=2) as response:
                    thread_health=json.load(response)
                policy=thread_health.get('inference',{}).get('intra_threads')
                if policy is None: time.sleep(.05)
            assert policy=={'foreground':1,'background':args.expect_background_threads}, 'loaded worker thread policy differs: '+str(policy)
            (output_dir/('inference-threads-'+account+'.json')).write_text(json.dumps(policy),encoding='utf-8')
        recalled=run('recall','Real account return fixture','--titles')
        assert recalled.get('items'),'real recall returned no seeded entry'
        if args.expect_supervised_worker:
            with urllib.request.urlopen(urllib.request.Request(f'http://127.0.0.1:{port}/api/health',headers={'X-Respire-Health':'runtime-v1'}),timeout=2) as response:
                assert checked_worker(json.load(response))==probe_worker,'warm recall replaced its worker'
        doctor=run('doctor',allow_failure=True)
        rows={item['name']:item for item in doctor['items'] if item['name'] in ['embedder','model index']}
        assert len(rows)==2 and all(item['status']=='ok' for item in rows.values()),rows
        assert same_existing_path(json.loads((main/'client.json').read_text())['data_dir'],profile)
        config=json.loads((main/'client.json').read_text())
        if not database_upgrade:
            assert all(config.get(key)==value for key,value in public_config.items()),'upgrade/switch changed public API settings'
        assert library_state(snapshot(profile),recovery_factors[str(profile)])==original_state[str(profile)], 'switch/probe changed original records, sync work or URK'
        # Independent important entries avoid the intentional daily-trivia append behavior.
        written=run('remember','Post-upgrade real inference '+account+' '+str(time.time_ns()),'--title','post-upgrade-'+account,'--importance','important','--force')
        assert written['summary'].get('id'),'real post-upgrade remember did not commit'
        # The durable write precedes derived indexing; compare stable snapshots after background completion.
        wait_index_ready()
        updated=snapshot(profile)
        updated_state=library_state(updated,recovery_factors[str(profile)])
        assert all(row in updated_state[0] for row in original_state[str(profile)][0]), 'remember changed an existing record'
        assert all(row in updated_state[1] for row in original_state[str(profile)][1]), 'remember changed existing sync work'
        assert updated_state[2]==expected_urks[str(profile)], 'remember changed the URK'
        original_state[str(profile)]=updated_state
        original[str(profile)]=updated
        assert run('recall','Post-upgrade real inference','--titles').get('items'),'post-upgrade remember could not be recalled'
        statuses.append({'account':account,'model_rows':rows})
    return statuses, generation_changes


def verify_reopen_and_restart(args, main, alternate, binary, env, port, database_upgrade,
                              recovery_factors, keys, custody_slots, custody_receipts,
                              expected_urks, checkpoint_before, run, run_client2, snapshot,
                              wait_index_ready, observe_runtime, checked_worker, confirm_workers_exited):
    import psutil

    live_owner_restart_rejected = False
    normal_reopen_checked=False
    if database_upgrade:
        before_close={str(profile):library_state(snapshot(profile),recovery_factors[str(profile)]) for profile in [main,alternate]}
        closing_owner=observe_runtime()
        owned=psutil.Process(closing_owner['pid'])
        retired=[(owned.pid,owned.create_time()), *[(child.pid,child.create_time()) for child in owned.children(recursive=True)]]
        if args.client2_binary:
            run_client2('stop')
        else:
            stopped=captured_run([str(binary),'--runtime-internal','--stop'],env=env,timeout=50)
            assert stopped.returncode==0, 'normal database owner stop failed'
        confirm_workers_exited(retired)
        assert {str(profile):library_state(snapshot(profile),recovery_factors[str(profile)]) for profile in [main,alternate]}==before_close, 'normal close changed records, sync work or URK'
        if args.client2_binary:
            run_client2('start')
            assert observe_runtime()['store_ready'], 'Client2 restart did not unlock the historical database'
        for account,profile in [('main',main),('alternate',alternate),('main',main)]:
            run('account','use',account)
            wait_index_ready()
            reopened=observe_runtime()
            assert reopened['instance_id']!=closing_owner['instance_id'], 'database reopen reused the stopped owner'
            assert run('recall','Real account return fixture','--titles').get('items'), 'reopened database lost historical recall'
            assert run('recall','Post-upgrade real inference','--titles').get('items'), 'reopened database lost post-upgrade recall'
            assert library_state(snapshot(profile),recovery_factors[str(profile)])==before_close[str(profile)], 'normal reopen changed records, sync work or URK'
        normal_reopen_checked=True
    restart_checked=False
    if args.expect_in_process or args.expect_supervised_worker:
        with urllib.request.urlopen(urllib.request.Request(f'http://127.0.0.1:{port}/api/health',headers={'X-Respire-Health':'runtime-v1'}),timeout=2) as response:
            health=json.load(response)
        owned=psutil.Process(health['pid'])
        retired_workers=[checked_worker(health)] if args.expect_supervised_worker else []
        if args.runtime_binary:
            observe_runtime(health)
            assert same_existing_path(owned.exe(),args.runtime_binary)
        else:
            assert pathlib.Path(owned.exe()).name.lower() in ['rsrs','rsrs.exe']
            assert '--runtime-internal' in owned.cmdline(),'only the isolated runtime may be suspended'
        before_restart={str(profile):library_state(snapshot(profile),recovery_factors[str(profile)]) for profile in [main,alternate]}
        config_before=(main/'client.json').read_bytes()
        endpoint_before=(main/'runtime/endpoint.json').read_bytes()
        token_path=main/'runtime/token'
        token_before=token_path.read_bytes() if token_path.is_file() else None
        registration_path=main/'runtime/autostart/registration.json'
        registration_before=registration_path.read_bytes() if registration_path.is_file() else None
        owned_birth=owned.create_time()
        try:
            owned.suspend()
            restarted=run('restart','--timeout','1',allow_failure=bool(args.runtime_binary))
            if args.runtime_binary:
                assert restarted.get('status')=='fail','restart replaced a suspended live owner'
                assert psutil.Process(owned.pid).create_time()==owned_birth and owned.is_running()
                assert process_start_id(owned.pid)==health['process_start_id']
                assert (main/'client.json').read_bytes()==config_before
                assert (main/'runtime/endpoint.json').read_bytes()==endpoint_before
                assert (token_path.read_bytes() if token_path.is_file() else None)==token_before
                assert (registration_path.read_bytes() if registration_path.is_file() else None)==registration_before
        finally:
            if owned.is_running() and owned.create_time()==owned_birth:
                if args.runtime_binary: assert process_start_id(owned.pid)==health['process_start_id']
                owned.resume()
        if args.runtime_binary:
            assert {str(profile):library_state(snapshot(profile),recovery_factors[str(profile)]) for profile in [main,alternate]}==before_restart
            resumed=observe_runtime()
            assert (resumed['pid'],resumed['process_start_id'],resumed['instance_id'])==(
                health['pid'],health['process_start_id'],health['instance_id'])
            live_owner_restart_rejected=True
            restarted=run('restart','--timeout','10')
            assert restarted['summary']['forced'] is False,restarted
        else:
            assert restarted['summary']['forced'] is True,restarted
        assert restarted['summary']['stopped_pid']==health['pid'],restarted
        assert restarted['summary']['pid']!=health['pid'],restarted
        confirm_workers_exited(retired_workers)
        if args.runtime_binary:
            confirm_workers_exited([(owned.pid,owned_birth)])
            replacement=observe_runtime()
            assert replacement['pid']==restarted['summary']['pid']
            assert (replacement['pid'],replacement['process_start_id'])!=(health['pid'],health['process_start_id'])
            assert replacement['instance_id']!=health['instance_id']
            assert replacement['image_sha256']==health['image_sha256']==args.runtime_sha256
        assert same_existing_path(restarted['summary']['data_dir'],main),restarted
        assert (main/'client.json').read_bytes()==config_before,'restart changed the account or API configuration'
        assert {str(profile):library_state(snapshot(profile),recovery_factors[str(profile)]) for profile in [main,alternate]}==before_restart,'restart changed original records, sync work or URK'
        if args.seed_binary:
            for account,profile in [('alternate',alternate),('main',main)]:
                if sys.platform=='darwin':
                    run('account','use',account)
                    wait_index_ready()
                verify_fixture_custody(profile,keys,custody_slots[str(profile)],expected_urks[str(profile)],binary,env)
            assert (main/'client.json').read_bytes()==config_before, 'post-restart custody changed account routing'
            assert {str(profile):library_state(snapshot(profile),recovery_factors[str(profile)]) for profile in [main,alternate]}==before_restart, 'post-restart custody changed original library state'
            if sys.platform=='darwin' and args.runtime_binary:
                replacement=observe_runtime()
        run('model','probe','--model','m3')
        assert run('recall','Real account return fixture','--titles').get('items')
        if args.runtime_binary:
            ready=observe_runtime()
            assert (ready['pid'],ready['process_start_id'],ready['instance_id'])==(
                replacement['pid'],replacement['process_start_id'],replacement['instance_id'])
        if args.expect_supervised_worker:
            with urllib.request.urlopen(urllib.request.Request(f'http://127.0.0.1:{port}/api/health',headers={'X-Respire-Health':'runtime-v1'}),timeout=2) as response:
                checked_worker(json.load(response))
        restart_checked=True
    if args.seed_binary:
        assert set(custody_receipts)==set(custody_slots) and len(custody_receipts)==2, 'host custody evidence missing for a historical account'
    checkpoint_evidence = None
    if args.database_dev_checkpoint:
        for profile in [main, alternate]:
            current = snapshot(profile)
            state = database_checkpoint_state(profile, args.database_dev_checkpoint,
                current, library_state(current, recovery_factors[str(profile)])[0])
            assert_database_checkpoint_preserved(checkpoint_before[str(profile)], state)
        checkpoint_evidence = {'tag': args.database_dev_checkpoint, 'special_state_seeded': True,
            'special_state_preserved': True, 'verified_accounts': ['main', 'alternate']}
    return normal_reopen_checked, restart_checked, live_owner_restart_rejected, checkpoint_evidence
