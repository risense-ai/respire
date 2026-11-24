"""Inspect original library, database checkpoints, and native credential custody."""
from contextlib import closing
import hashlib
import json
import pathlib
import sqlite3
import sys

from cryptography.hazmat.primitives.ciphers.aead import AESGCM
import legacy_vault_fixture as vault_crypto
from native_fixture_process import canonical_scope_path, captured_run, same_existing_path


def recover_fixture_urk(session, factors):
    version = session.get('vault_version', 1)
    assert version in (1, 2, 3, 4), 'unsupported fixture vault version'
    wrapped = session['wrapped_urk']
    if version == 4:
        kek = vault_crypto.v4_kek(factors['super'], session['kdf_salt'], wrapped)
    else:
        kek = vault_crypto.legacy_kek(version, factors['pass'] if version == 1 else factors['super'],
            factors['secret'] if version == 1 else factors['secret_key'] if version == 3 else '',
            session['kdf_salt'], current=wrapped.startswith(vault_crypto.PREFIX))
    return AESGCM(kek).decrypt(bytes.fromhex(session['urk_nonce']), vault_crypto.encrypted_bytes(wrapped), None)


def legacy_custody_alias(profile, session_bytes):
    return 'legacy-'+hashlib.sha256(canonical_scope_path(profile).encode('utf-8')+session_bytes).hexdigest()


def migrate_fixture_custody(profile, keys, plan, expected_urk):
    alias,version,slots=plan
    original=(profile/'session.json').read_bytes()
    session=json.loads(original)
    assert legacy_custody_alias(profile,original)==alias, 'historical session changed before fixture custody'
    backend=keys.credential_backend
    assert backend in ('native','secret_service'), 'fixture custody requires a persistent provider'
    factors={('secret' if version==1 else 'secret_key') if slot=='legacy-secret' else slot:value
        for slot,value in slots.items()}
    assert recover_fixture_urk(session,factors)==expected_urk, 'fixture custody original URK differs'
    for slot,value in slots.items():
        assert ('rsrs',slot+':'+alias) in keys.entries, 'fixture custody slot was not reserved'
        keys.put('rsrs',slot+':'+alias,value)
    migrated={key:value for key,value in session.items() if key not in ('pass','super','secret','secret_key')}
    migrated.update(keyring_account=alias,keyring_backend=backend,legacy_secret_backend=backend)
    assert (profile/'session.json').read_bytes()==original, 'historical session changed during fixture custody'
    (profile/'session.json').write_text(json.dumps(migrated),encoding='utf-8')


def custody_consumer_error(output):
    # Emit only known source messages; arbitrary output may contain decrypted data or credentials.
    body=(output.stdout+output.stderr).decode('utf-8',errors='replace')
    markers=('runtime request belongs to another installation',
        'runtime RPC response scope mismatch', 'scope_mismatch',
        'unlock Runtime does not own the selected profile',
        'runtime owner roots or installation identity do not match maintenance',
        'unlock Runtime process identity changed', 'unlock Runtime image changed',
        'host session credential read failed', 'legacy secret credential is missing or empty',
        'Runtime session credentials are unavailable', 'failed to decrypt',
        'NoStorageAccess', 'PlatformFailure', 'NoEntry')
    matched=sorted((body.index(marker),marker) for marker in markers if marker in body)
    return 'CLI host custody consumer failed: '+json.dumps({'returncode':output.returncode,
        'source_chain':[marker for _,marker in matched] or ['unclassified'],
        'stdout_bytes':len(output.stdout),'stderr_bytes':len(output.stderr)})


def verify_fixture_custody(profile, keys, plan, expected_urk, binary, env):
    alias,version,slots=plan
    session=json.loads((profile/'session.json').read_bytes())
    backend='secret_service' if sys.platform.startswith('linux') else 'native'
    assert session.get('keyring_account')==alias, 'custody alias differs from the reserved profile/session identity'
    assert session.get('vault_version',1)==version, 'custody changed the historical vault version'
    assert session.get('keyring_backend')==session.get('legacy_secret_backend')==backend, 'custody selected an unverified provider'
    assert not {'pass','super','secret','secret_key'} & session.keys(), 'custody retained inline sensitive factors'
    if sys.platform.startswith('linux'): keys.verify_owner()
    recovered={}
    for slot,expected in slots.items():
        assert ('rsrs',slot+':'+alias) in keys.entries, 'custody slot is not reserved by this fixture'
        if sys.platform=='darwin':
            # These synthetic fixture entries are readable without changing product ACLs.
            metadata=keys._security(keys.root,['find-generic-password','-s','rsrs','-a',slot+':'+alias,str(keys.keychain)])
            assert metadata.returncode==0, 'custody exact slot metadata missing'
        else:
            value=keys.read('rsrs',slot+':'+alias)
            assert value==expected, 'custody slot readback differs from the original factor'
            name=('secret' if version==1 else 'secret_key') if slot=='legacy-secret' else slot
            recovered[name]=value
    if sys.platform=='darwin':
        # Check the wrap with original factors, then require the CLI to unlock the same data via host slots.
        original_factors={(('secret' if version==1 else 'secret_key') if slot=='legacy-secret' else slot): value
            for slot,value in slots.items()}
        assert recover_fixture_urk(session,original_factors)==expected_urk, 'host custody changed the URK'
        assert set(slots)=={'pass' if version==1 else 'super','legacy-secret'}, 'CLI custody proof has an unconsumed slot'
        # RSRS_DATA_DIR identifies the installation; client.json selects its account.
        clean_env=dict(env)
        installation=pathlib.Path(env['RSRS_DATA_DIR'])
        config_bytes=(installation/'client.json').read_bytes()
        assert same_existing_path(json.loads(config_bytes)['data_dir'],profile), 'custody consumer account is not selected'
        for prefix in ('RSRS_','ONEMEMORY_','RESPIRE_'):
            for suffix in ('SUPER','PASS','SECRET','KDF_SALT','WRAPPED_URK','URK_NONCE'):
                assert prefix+suffix not in env, 'resident custody consumer has a factor override'
        database = profile/'rsrs.db'
        if not database.exists(): database = profile/'onememory.db'
        with closing(sqlite3.connect(database.as_uri()+'?mode=ro',uri=True)) as db:
            row=db.execute('SELECT id,ciphertext,nonce FROM memories WHERE deleted=0 ORDER BY id LIMIT 1').fetchone()
        assert row is not None, 'custody consumer has no original ciphertext'
        expected=json.loads(vault_crypto.decrypt_content(expected_urk,*row[1:]))['content']
        shown=captured_run([str(binary),'--json','show',row[0]],env=clean_env,timeout=120)
        assert shown.returncode==0, custody_consumer_error(shown)
        result=json.loads(shown.stdout)
        assert not result.get('errors') and result.get('status') not in ('error','failed','fail'), custody_consumer_error(shown)
        assert result.get('details',{}).get('entry',{}).get('content')==expected, 'CLI host custody could not decrypt original data'
        assert (installation/'client.json').read_bytes()==config_bytes, 'CLI custody consumer changed account routing'
        assert json.loads((profile/'session.json').read_bytes())==session, 'CLI custody consumer changed the session'
    else:
        assert recover_fixture_urk(session,recovered)==expected_urk, 'host custody changed the URK'
    return {'vault_version':version,'primary_slot_readback':sys.platform!='darwin',
        'legacy_secret_readback':sys.platform!='darwin','urk_preserved':True,
        'consumer':'cli_authenticated_decryption' if sys.platform=='darwin' else 'exact_slot_readback'}


def library_state(snapshot, factors):
    rows, _, session_bytes = snapshot
    session = json.loads(session_bytes)
    urk = recover_fixture_urk(session, factors)
    records = []
    for memory_id, ciphertext, nonce, dirty, deleted in rows[0]:
        plaintext = vault_crypto.decrypt_content(urk, ciphertext, nonce)
        records.append((memory_id, plaintext, dirty, deleted))
    return records, rows[1], urk


def assert_historical_database_seed(snapshot_value):
    memories, outbox, _ = snapshot_value[0]
    assert any(row[1] and row[2] and row[4] == 0 for row in memories), 'historical seed has no encrypted active record'
    assert any(row[1] and row[2] and row[3] == 1 and row[4] == 1 for row in memories), 'historical seed has no encrypted pending tombstone'
    assert outbox, 'historical seed has no pending synchronization work'


def database_checkpoint_state(profile, checkpoint, snapshot_value, records):
    if checkpoint == 'v1.0.11-dev.1':
        links = []
        for memory_id, plaintext, _, _ in records:
            payload = json.loads(plaintext)
            links.append((memory_id, payload.get('supersedes', ''),
                          payload.get('superseded_by', ''), tuple(sorted(payload.get('see_also', [])))))
        return {'kind': 'legacy_relation_payload', 'links': sorted(links),
                'legacy_domain': all(not row[1].startswith(vault_crypto.PREFIX) for row in snapshot_value[0][0])}
    if checkpoint != 'v1.0.17-dev.6':
        raise ValueError('unsupported database checkpoint: ' + checkpoint)
    database = profile / 'rsrs.db'
    if not database.exists():
        database = profile / 'onememory.db'
    with closing(sqlite3.connect(database.as_uri() + '?mode=ro', uri=True)) as db:
        witness = db.execute('SELECT schema_version,library_id,seq,ts,action,entry_id,previous_entry_id,'
            'before_digest,after_digest,prev,hash FROM witness_log ORDER BY seq').fetchall()
        requests = db.execute('SELECT request_id FROM query_log WHERE request_id IS NOT NULL ORDER BY request_id').fetchall()
        feedback = db.execute('SELECT q.request_id,f.memory_id,f.good,f.source,f.marked_at,f.revision '
            'FROM query_feedback f JOIN query_log q ON q.id=f.query_log_id '
            'ORDER BY q.request_id,f.memory_id').fetchall()
        recovery_table = db.execute("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='sync_recovery_intents')").fetchone()[0]
    return {'kind': 'witness_feedback', 'witness': witness, 'requests': requests,
            'feedback': feedback, 'recovery_table': bool(recovery_table)}


def assert_database_checkpoint_preserved(before, after):
    assert before['kind'] == after['kind'], 'database checkpoint kind changed'
    if before['kind'] == 'legacy_relation_payload':
        current = {row[0]: row for row in after['links']}
        assert all(current.get(row[0]) == row for row in before['links']), 'encrypted historical relation fields changed'
    else:
        assert after['witness'][:len(before['witness'])] == before['witness'], 'historical witness prefix changed'
        assert set(before['requests']) <= set(after['requests']), 'historical query request identity lost'
        assert set(before['feedback']) <= set(after['feedback']), 'historical explicit feedback changed'
