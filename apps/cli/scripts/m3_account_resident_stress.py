"""Measure resident foreground and background work on the disposable M3 library."""
import json
import sqlite3
import sys
import time
import urllib.request

from native_fixture_process import FixtureCancelled, ForegroundOverlapProbe


def verify_resident_stress(args, main, port, output_dir, run, observe_runtime,
                           checked_worker, wait_index_ready):
    global resource_members, resource_members_checked

    import concurrent.futures, contextlib, psutil, threading
    with urllib.request.urlopen(urllib.request.Request(f'http://127.0.0.1:{port}/api/health',headers={'X-Respire-Health':'runtime-v1'}), timeout=2) as response:
        health = json.load(response)
    owned = psutil.Process(health['pid'])
    if args.runtime_binary:
        observe_runtime(health)
    else:
        assert '--runtime-internal' in owned.cmdline(), 'stress must target its owned runtime'
    resource_members=None
    resource_members_checked=None
    def total_memory(refresh_members=True):
        global resource_members, resource_members_checked
        if refresh_members:
            # Keep the original all-descendant audit. Continuous RSS
            # sampling must not repeatedly scan the entire OS PID map.
            resource_members=[owned,*owned.children(recursive=True)]
            resource_members_checked=time.monotonic()
        members=resource_members
        memories=[member.memory_info() for member in members]
        return sum(memory.rss for memory in memories),sum(getattr(memory,'private',0) or 0 for memory in memories)
    baseline_rss,baseline_private=total_memory()
    samples = []
    observer_errors=[]
    timings = []
    body = 'resident stress query ' + 'background indexing and foreground retrieval '.join(['sample'] * 110)
    def timed(command, timeout=120, cancel=None):
        started = time.monotonic()
        try:
            envelope = run(*command, timeout=timeout,cancel=cancel)
        except FixtureCancelled:
            raise
        except Exception as error:
            raise RuntimeError('stress '+command[0]+' failed after '+str(round(time.monotonic()-started,3))+'s: '+str(error)) from error
        assert envelope['status'] in ['ok', 'warn'], command[0]
        return command[0], time.monotonic() - started, envelope
    def probe_recall(timeout, cancel):
        command, elapsed, envelope = timed(['recall','Real account return fixture','--titles','--no-related'], timeout=timeout,cancel=cancel)
        assert envelope.get('items'), 'foreground overlap probe lost the indexed seed'
        return {'command':command,'seconds':elapsed,**envelope['_fixture_timing']}
    overlap = ForegroundOverlapProbe(probe_recall)
    fixed_workload_completed = None
    def sample_health(check_boundary=True):
        overlap.check()
        if check_boundary and observer_errors:
            raise RuntimeError('continuous health observer failed: '+str(observer_errors))
        sample_started=time.monotonic()
        sample_unix=time.time()
        with urllib.request.urlopen(urllib.request.Request(f'http://127.0.0.1:{port}/api/health',headers={'X-Respire-Health':'runtime-v1'}), timeout=2) as response:
            current = json.load(response)
        health_completed=time.monotonic()
        assert current['pid'] == owned.pid, 'stress changed runtime ownership'
        inference = current.get('inference', {})
        overlap.observe(inference)
        rss,private=total_memory(refresh_members=check_boundary)
        memory_completed=time.monotonic()
        if check_boundary and args.expect_supervised_worker: checked_worker(current)
        samples.append({'rss':rss,'private':private,
            'started_unix':sample_unix,'started_monotonic':sample_started,
            'health_completed_monotonic':health_completed,
            'health_seconds':health_completed-sample_started,
            'memory_seconds':memory_completed-health_completed,
            'boundary_seconds':time.monotonic()-memory_completed,
            'membership_age_seconds':time.monotonic()-resource_members_checked,
            'boundary_checked':check_boundary,
            'retrieval_index':current.get('retrieval_index',{}),'inference':inference,
            'foreground':inference.get('foreground',{}),'background':inference.get('background',{})})
    @contextlib.contextmanager
    def observed_pool():
        stopped=threading.Event()
        def observe():
            try:
                while not stopped.is_set():
                    # Retain every original boundary check in the main
                    # loop. Handle inspection must not starve concurrent
                    # HTTP responsiveness/lane/resource observations.
                    sample_health(check_boundary=False)
                    stopped.wait(.02)
            except Exception as error: observer_errors.append(str(error))
        observer=threading.Thread(target=observe,name='resident-health-observer',daemon=True)
        observer.start()
        # Core lane telemetry is a 250 ms heartbeat, not a fresh native
        # sample on each HTTP poll. Model checksums/preparation can also
        # outlast the fixed CLI burst. Keep bounded real foreground work
        # available throughout preparation; never substitute queued work
        # or separate foreground/background observations for overlap.
        overlap.start()
        try:
            with concurrent.futures.ThreadPoolExecutor(max_workers=18) as pool:
                yield pool
        finally:
            try:
                overlap.close(cancel_pending=sys.exc_info()[0] is not None)
            finally:
                stopped.set()
                observer.join(timeout=3)
                # Persist evidence even when a deadline, client, or
                # boundary assertion fails before final metrics exist.
                (output_dir/'resident-stress-metrics.json').write_text(json.dumps({
                    'passed':False,'requests':len(timings),'timings':timings,'samples':samples,
                    'overlap_probe':{'deadline_seconds':120,'max_in_flight':2,'timings':overlap.calls,
                        'errors':[str(error) for error in overlap.errors]},
                    'fixed_workload_completed_monotonic':fixed_workload_completed,
                    'observer_errors':observer_errors},indent=2),encoding='utf-8')
                assert not observer.is_alive(),'continuous health observer did not stop'
                assert not observer_errors,'continuous health observer failed: '+str(observer_errors)
    with observed_pool() as pool:
        print('resident phase: concurrent writes',flush=True)
        batch_deadline=time.monotonic()+120*2+10
        writes = [pool.submit(timed, ['remember', body, '--title', 'stress-'+str(i),
            '--importance', 'important', '--force']) for i in range(28)]
        while any(not future.done() for future in writes):
            assert time.monotonic()<batch_deadline,'resident write batch exceeded command deadlines'
            sample_health()
            time.sleep(.02)
        created = []
        for future in writes:
            command, elapsed, envelope = future.result()
            assert envelope['summary'].get('index_state') == 'pending', 'remember waited for derived indexing'
            created.append(envelope['summary']['id'])
            timings.append({'command':command,'seconds':elapsed,**envelope['_fixture_timing']})
        # Retrieval quality evaluation must use the same pending-aware
        # resident path as recall, without requiring derived artifacts.
        eval_file = output_dir / 'resident-stress-eval.jsonl'
        bench_file = output_dir / 'resident-stress-bench.json'
        eval_file.write_text(json.dumps({'query':'resident stress query','expect':created})+'\n',encoding='utf-8')
        run('bench','run',str(eval_file),'--save',str(bench_file))
        bench_report = json.loads(bench_file.read_text(encoding='utf-8'))
        # The worker can finish first; the controlled CLI unit fixture
        # separately proves the all-pending retrieval contract.
        assert 0 <= int(bench_report['params']['index_pending']) <= bench_report['n_entries'], 'invalid pending count'
        assert bench_report['metrics']['hitk'] == 1, 'pending-aware benchmark lost the expected source'
        run('bench','run',str(eval_file),'--baseline',str(bench_file))
        # A healthy owner must serve reads even while another process
        # holds the lifecycle gate for startup/replacement decisions.
        lifecycle = sqlite3.connect(main/'runtime/takeover/lock.db', timeout=2)
        try:
            lifecycle.execute('BEGIN EXCLUSIVE')
            command, elapsed, envelope = timed(['show', created[0]])
            assert elapsed < 5, 'warm request acquired the lifecycle gate'
        finally:
            lifecycle.rollback()
            lifecycle.close()
        work = []
        print('resident phase: concurrent recall/show/update',flush=True)
        batch_deadline=time.monotonic()+120*5+10
        for i in range(28):
            work.append(pool.submit(timed, ['recall','resident stress query','--titles','--no-related']))
            work.append(pool.submit(timed, ['show',created[i]]))
            if i < 16:
                work.append(pool.submit(timed, ['update',created[i],'--title','updated-stress-'+str(i)]))
        work.append(pool.submit(timed, ['remember','concurrent trivial write','--importance','trivial']))
        while any(not future.done() for future in work):
            assert time.monotonic()<batch_deadline,'resident mixed batch exceeded command deadlines'
            sample_health()
            time.sleep(.02)
        for future in work:
            command, elapsed, envelope = future.result()
            if command == 'recall':
                assert envelope.get('items'), 'pending or indexed stress data was not recalled'
            timings.append({'command':command,'seconds':elapsed,**envelope['_fixture_timing']})
        fixed_workload_completed = time.monotonic()
        while not overlap.observed.is_set():
            sample_health()
            time.sleep(.02)
    overlap_details = {'deadline_seconds':120,'max_in_flight':2,'timings':overlap.calls,
        'started_monotonic':overlap.started,
        'fixed_workload_completed_monotonic':fixed_workload_completed,
        'first_background_active_monotonic':min((sample['health_completed_monotonic'] for sample in samples if sample['background'].get('active')),default=None),
        'both_active_during_fixed_workload':any(sample['health_completed_monotonic'] <= fixed_workload_completed
            and sample['foreground'].get('active') and sample['background'].get('active') for sample in samples)}
    metrics = {'passed':False,'requests':len(timings),'timings':timings,
        'overlap_probe':overlap_details,
        'rss_before':baseline_rss,'rss_peak':max(sample['rss'] for sample in samples),
        'resource_scope':'owned host and all descendants',
        'private_before':baseline_private,
        'private_peak':max((sample['private'] or 0) for sample in samples),
        'both_lanes_observed':any(sample['foreground'].get('active') and sample['background'].get('active') for sample in samples),
        'samples':samples}
    (output_dir/'resident-stress-metrics.json').write_text(json.dumps(metrics,indent=2),encoding='utf-8')
    assert metrics['both_lanes_observed'],'concurrent foreground/background activity was not observed'
    # This fixture submits 28 long documents and 16 content-bearing edits;
    # its background batch is larger than the initial account preparation.
    print('resident phase: final index drain (900s)',flush=True)
    wait_index_ready(timeout=900)
    database = main / 'rsrs.db'
    if not database.exists(): database = main / 'onememory.db'
    with sqlite3.connect(database) as database_connection:
        missing = database_connection.execute(
            'SELECT COUNT(*) FROM memories m WHERE deleted=0 AND NOT EXISTS '
            '(SELECT 1 FROM core_artifacts a WHERE a.memory_id=m.id AND a.source=m.ciphertext)').fetchone()[0]
    assert missing == 0, 'background index did not catch up with edited sources'
    light = [entry['seconds'] for entry in timings if entry['command'] == 'show']
    assert max(light) < 5, 'lightweight reads were blocked by inference or background work'
    stress = {'passed':True,'requests':len(timings),'background_source_checks':True,
        'overlap_probe':overlap_details,
        'show_max_seconds':max(light),'timings':timings,'rss_before':baseline_rss,
        'resource_scope':'owned host and all descendants',
        'rss_peak':max(sample['rss'] for sample in samples),
        'private_before':baseline_private,
        'private_peak':max((sample['private'] or 0) for sample in samples),
        'both_lanes_observed':any(sample['foreground'].get('active') and sample['background'].get('active') for sample in samples),
        'samples':samples}
    (output_dir/'resident-stress-metrics.json').write_text(json.dumps(stress,indent=2),encoding='utf-8')
    return stress
