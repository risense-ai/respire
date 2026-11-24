"""Bound native-test process waits, including Windows handle inspection.

The inspector preserves boundary assertions in a disposable process: a hung
psutil native call must fail acceptance rather than hang its Python controller.
"""
import json
import hashlib
import os
import pathlib
import subprocess
import sys
import tempfile
import threading
import time
import secrets
import shutil
import runpy

from native_fixture_workers import (
    FixtureCancelled, ForegroundOverlapProbe, WorkerInspector,
    captured_run, inspect_worker, process_start_id, serve,
)


def add_runtime_arguments(parser):
    parser.add_argument('--runtime-binary', default=os.environ.get('RSRS_RUNTIME_BIN'))
    parser.add_argument('--runtime-sha256', default=os.environ.get('RSRS_PUBLISHED_RUNTIME_SHA256'))
    parser.add_argument('--runtime-version', default=os.environ.get('RSRS_PUBLISHED_RUNTIME_VERSION'))


def runtime_inputs(args, cli_binaries):
    values = (args.runtime_binary, args.runtime_sha256, args.runtime_version)
    published = os.environ.get('RSRS_ARTIFACT_SOURCE') == 'published-assets'
    if not published and not any(values[1:]):
        args.runtime_binary = None
        return
    if not all(values):
        raise ValueError('complete standalone Runtime path, digest and version required')
    if published and (os.environ.get('GITHUB_ACTIONS') != 'true'
                      or os.environ.get('RUNNER_ENVIRONMENT') != 'github-hosted'):
        raise ValueError('published Runtime fixtures require a dedicated GitHub-hosted runner')
    args.runtime_binary = pathlib.Path(args.runtime_binary).resolve(strict=True)
    if any(args.runtime_binary.samefile(path) for path in cli_binaries):
        raise ValueError('CLI shim cannot substitute for standalone Runtime')
    args.runtime_sha256 = args.runtime_sha256.lower()
    if len(args.runtime_sha256) != 64 or any(c not in '0123456789abcdef' for c in args.runtime_sha256):
        raise ValueError('invalid Runtime image digest')
    with args.runtime_binary.open('rb') as stream:
        if hashlib.file_digest(stream, 'sha256').hexdigest() != args.runtime_sha256:
            raise ValueError('Runtime image digest mismatch')
    for key in ('RSRS_PUBLISHED_ASSETS_SHA256', 'RSRS_PUBLISHED_BUNDLE_SHA256'):
        value = os.environ.get(key, '')
        if published and (len(value) != 64 or any(c not in '0123456789abcdef' for c in value)):
            raise ValueError('published Runtime provenance missing: ' + key)
    args.runtime_identity_receipts = []


def runtime_install_diagnostic(result):
    """Retain fixed installer failure fields without raw errors, paths or stderr."""
    diagnostic = {'exit_code': result.returncode, 'structured_reason': 'unknown',
                  'category': 'unclassified', 'envelope': 'invalid_json'}
    try:
        value = json.loads(result.stdout)
    except (ValueError, TypeError):
        return diagnostic
    if not isinstance(value, dict):
        diagnostic['envelope'] = 'non_object'
        return diagnostic
    diagnostic['envelope'] = 'object'
    summary = value.get('summary')
    reason = summary.get('reason') if isinstance(summary, dict) else None
    if type(reason) is str and reason in ('invalid_input', 'runtime_error', 'request_outcome_unknown'):
        diagnostic['structured_reason'] = reason
    errors = value.get('errors')
    if isinstance(errors, list) and 'runtime restart blocked: owner roots do not match this installation' in errors:
        diagnostic['category'] = 'owner_roots_mismatch'
    return diagnostic


def install_official_runtime(binary, env, version, binary_hash):
    """Install the pinned Runtime into a fresh source-build fixture, then bind its identity."""
    if (os.environ.get('GITHUB_ACTIONS') != 'true'
            or os.environ.get('RUNNER_ENVIRONMENT') != 'github-hosted'):
        raise ValueError('official installation requires a dedicated GitHub-hosted runner')
    root = pathlib.Path(env['RSRS_DATA_DIR'])
    if root.is_symlink():
        raise ValueError('official installation root must not be linked')
    root = root.resolve()
    if not any(root != pathlib.Path(os.environ[key]).resolve()
               and root.is_relative_to(pathlib.Path(os.environ[key]).resolve())
               for key in ('RUNNER_TEMP', 'GITHUB_WORKSPACE') if os.environ.get(key)):
        raise ValueError('official installation must remain inside the owned runner workspace')
    source_full = (os.environ.get('ARTIFACT_SOURCE') == 'build'
                   and os.environ.get('VALIDATION_PROFILE') == 'full')
    if (root / 'bin/runtime.json').exists():
        if not source_full:
            raise ValueError('source-build fixture must start without a Runtime receipt')
    else:
        install_env = {key: value for key, value in env.items()
                       if key not in ('RSRS_RUNTIME_BIN', 'RSRS_CORE_TEST_MODE', 'GH_TOKEN', 'GITHUB_TOKEN')
                       and not key.endswith(('_TOKEN', '_API_KEY'))}
        result = subprocess.run([str(binary), '--json', 'runtime', 'install', '--version', version],
                                env=install_env, capture_output=True, timeout=180)
        if result.returncode:
            raise RuntimeError('official Runtime fixture install failed: exit ' + str(result.returncode)
                               + '; diagnostic=' + json.dumps(runtime_install_diagnostic(result)))
    receipt = json.loads((root / 'bin/runtime.json').read_text(encoding='utf-8'))
    proof = receipt.get('release') or {}
    metadata_bytes = proof.get('metadata_json', '').encode('utf-8')
    metadata = json.loads(metadata_bytes)
    installed = root / 'bin/runtime' / version / ('rsrs-runtime.exe' if os.name == 'nt' else 'rsrs-runtime')
    if (receipt.get('version') != version or receipt.get('sha256') != binary_hash
            or proof.get('metadata_sha256') != hashlib.sha256(metadata_bytes).hexdigest()
            or metadata.get('schema_version') != 2 or metadata.get('runtime_version') != version
            or (metadata.get('target_triple') or metadata.get('triple')) != env['RSRS_RUNTIME_TRIPLE']
            or metadata.get('binary_sha256') != binary_hash
            or installed.is_symlink() or not installed.resolve().is_relative_to(root)):
        raise ValueError('fixture installation differs from the selected official Runtime')
    with installed.open('rb') as stream:
        if hashlib.file_digest(stream, 'sha256').hexdigest() != binary_hash:
            raise ValueError('fixture Runtime image differs from the selected asset')
    env['RSRS_RUNTIME_BIN'] = str(installed.resolve(strict=True))
    return installed.resolve(strict=True)


def stage_fixture_cli(binary, env, root):
    bin_dir = pathlib.Path(env.get('RSRS_BIN_DIR') or root / 'bin')
    if bin_dir.is_symlink():
        raise ValueError('fixture bin must not be linked')
    bin_dir = bin_dir.resolve()
    if not any(bin_dir != pathlib.Path(os.environ[key]).resolve()
               and bin_dir.is_relative_to(pathlib.Path(os.environ[key]).resolve())
               for key in ('RUNNER_TEMP', 'GITHUB_WORKSPACE') if os.environ.get(key)):
        raise ValueError('fixture bin must remain inside the owned runner workspace')
    bin_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
    staged = bin_dir / ('rsrs.exe' if os.name == 'nt' else 'rsrs')
    if staged.is_symlink():
        raise ValueError('fixture CLI must not be linked')
    if not staged.exists():
        shutil.copy2(binary, staged)
    if staged.read_bytes() != pathlib.Path(binary).read_bytes():
        raise ValueError('fixture CLI differs from the selected build')
    env['RSRS_BIN_DIR'] = str(bin_dir)


def install_published_runtime(binary, env):
    """Use the real offline installer with the selected release's unchanged bytes."""
    if (os.environ.get('ARTIFACT_SOURCE') == 'build'
            and os.environ.get('VALIDATION_PROFILE') == 'full'
            and os.environ.get('GITHUB_ACTIONS') == 'true'
            and os.environ.get('RUNNER_ENVIRONMENT') == 'github-hosted'
            and os.environ.get('RSRS_ARTIFACT_SOURCE') != 'published-assets'):
        root = pathlib.Path(env.get('RSRS_DATA_DIR') or pathlib.Path(env['HOME']) / '.rsrs')
        if root.is_symlink():
            raise ValueError('source installation root must not be linked')
        root = root.resolve()
        if not any(root != pathlib.Path(os.environ[key]).resolve()
                   and root.is_relative_to(pathlib.Path(os.environ[key]).resolve())
                   for key in ('RUNNER_TEMP', 'GITHUB_WORKSPACE') if os.environ.get(key)):
            raise ValueError('source installation must remain inside the owned runner workspace')
        root.mkdir(parents=True, exist_ok=True, mode=0o700)
        if os.name == 'posix':
            root.chmod(0o700)
        env['RSRS_RUNTIME_TRIPLE'] = os.environ['RSRS_RUNTIME_TRIPLE']
        repository = pathlib.Path(__file__).resolve().parent.parent
        version = os.environ['NATIVE_RUNTIME_VERSION']
        binary_hash = os.environ['NATIVE_RUNTIME_SHA256']
        if (version != json.loads((repository / 'test-support/runtime-test-fixture.json').read_text(encoding='utf-8'))['required']
                or len(binary_hash) != 64 or any(c not in '0123456789abcdef' for c in binary_hash)):
            raise ValueError('source fixture differs from the independently pinned Runtime identity')
        if not (root / 'bin/runtime.json').exists():
            source = pathlib.Path(os.environ['NATIVE_RUNTIME_BIN']).resolve(strict=True)
            if source.parent.name != version or source.parent.parent.name != 'runtime':
                raise ValueError('official managed Runtime source required')
            source_root = source.parent.parent.parent
            if source_root.name != 'bin' or source_root.parent == root or not (source_root / 'runtime.json').is_file():
                raise ValueError('independent official Runtime source receipt required')
            source_env = dict(env, RSRS_DATA_DIR=str(source_root.parent))
            verified = install_official_runtime(binary, source_env, version, binary_hash)
            if not source.samefile(verified):
                raise ValueError('official Runtime source differs from the selected image')
            bundle = runpy.run_path(str(repository / 'scripts/runtime-acceptance-bundle.py'))
            bundle['copy_managed_installation'](source_root, root)
        install_env = dict(env, RSRS_DATA_DIR=str(root))
        installed = install_official_runtime(binary, install_env, version, binary_hash)
        env['RSRS_RUNTIME_BIN'] = install_env['RSRS_RUNTIME_BIN']
        stage_fixture_cli(binary, env, root)
        return installed
    if os.environ.get('RSRS_ARTIFACT_SOURCE') != 'published-assets':
        return None
    if (os.environ.get('GITHUB_ACTIONS') != 'true'
            or os.environ.get('RUNNER_ENVIRONMENT') != 'github-hosted'):
        raise ValueError('published installation requires a dedicated GitHub-hosted runner')
    root = pathlib.Path(env.get('RSRS_DATA_DIR') or pathlib.Path(env['HOME']) / '.rsrs')
    if root.is_symlink():
        raise ValueError('published installation root must not be linked')
    root = root.resolve()
    if not any(root != pathlib.Path(os.environ[key]).resolve()
               and root.is_relative_to(pathlib.Path(os.environ[key]).resolve())
               for key in ('RUNNER_TEMP', 'GITHUB_WORKSPACE') if os.environ.get(key)):
        raise ValueError('published installation must remain inside the owned runner workspace')
    archive = pathlib.Path(os.environ['RSRS_PUBLISHED_RUNTIME_ARCHIVE']).resolve(strict=True)
    metadata_path = pathlib.Path(os.environ['RSRS_PUBLISHED_RUNTIME_METADATA']).resolve(strict=True)
    version = os.environ['RSRS_PUBLISHED_RUNTIME_VERSION']
    triple = os.environ['RSRS_RUNTIME_TRIPLE']
    if metadata_path != archive.with_name('runtime-build-' + triple + '.json'):
        raise ValueError('offline installation requires the selected adjacent metadata')
    metadata_bytes = metadata_path.read_bytes()
    metadata_hash = hashlib.sha256(metadata_bytes).hexdigest()
    with archive.open('rb') as stream:
        archive_hash = hashlib.file_digest(stream, 'sha256').hexdigest()
    metadata = json.loads(metadata_bytes)
    binary_hash = os.environ['RSRS_PUBLISHED_RUNTIME_SHA256']
    if (metadata_hash != os.environ['RSRS_PUBLISHED_RUNTIME_METADATA_SHA256']
            or archive_hash != os.environ['RSRS_PUBLISHED_BUNDLE_SHA256']
            or metadata.get('schema_version') != 2 or metadata.get('runtime_version') != version
            or metadata.get('target_triple') != triple or metadata.get('archive_sha256') != archive_hash
            or metadata.get('binary_sha256') != binary_hash):
        raise ValueError('selected published Runtime bytes or identity changed')
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    if os.name == 'posix':
        root.chmod(0o700)
    receipt_path = root / 'bin/runtime.json'
    if not receipt_path.exists():
        install_env = {key: value for key, value in env.items()
                       if key not in ('RSRS_RUNTIME_BIN', 'RSRS_CORE_TEST_MODE', 'GH_TOKEN', 'GITHUB_TOKEN')
                       and not key.endswith(('_TOKEN', '_API_KEY'))}
        install_env.update(RSRS_DATA_DIR=str(root), RSRS_RUNTIME_TRIPLE=triple,
                           RSRS_UPDATE_CHECK='0', RSRS_NO_AUTOSYNC='1')
        result = subprocess.run([str(binary), '--json', 'runtime', 'install', '--file', str(archive),
                                 '--version', version], env=install_env, capture_output=True, timeout=180)
        if result.returncode:
            raise RuntimeError('selected published Runtime offline install failed: exit ' + str(result.returncode)
                               + '; diagnostic=' + json.dumps(runtime_install_diagnostic(result)))
    receipt = json.loads(receipt_path.read_text(encoding='utf-8'))
    proof = receipt.get('release', {})
    installed = root / 'bin/runtime' / version / ('rsrs-runtime.exe' if os.name == 'nt' else 'rsrs-runtime')
    if (receipt.get('version') != version or receipt.get('sha256') != binary_hash
            or proof.get('metadata_sha256') != metadata_hash
            or proof.get('metadata_json', '').encode('utf-8') != metadata_bytes
            or installed.is_symlink() or not installed.resolve().is_relative_to(root)):
        raise ValueError('managed installation does not preserve the selected release proof')
    with installed.open('rb') as stream:
        if hashlib.file_digest(stream, 'sha256').hexdigest() != binary_hash:
            raise ValueError('managed installation image differs from the selected asset')
    env['RSRS_RUNTIME_BIN'] = str(installed.resolve(strict=True))
    env['RSRS_RUNTIME_TRIPLE'] = triple
    stage_fixture_cli(binary, env, root)
    return installed.resolve(strict=True)


def same_existing_path(left, right):
    """Compare OS file identities, including Windows verbatim-path aliases."""
    if not left or not right:
        return False
    try:
        return pathlib.Path(left).samefile(right)
    except (OSError, ValueError, TypeError):
        return False


def canonical_scope_path(value):
    """Match Rust canonicalize().display(), retaining Windows verbatim prefixes."""
    path = pathlib.Path(value).resolve(strict=True)
    if os.name == 'nt':
        from nt import _getfinalpathname
        return _getfinalpathname(str(path))
    return str(path)


def runtime_scope_ids(root, profile, session):
    user = session.get('user')
    if user is not None and not isinstance(user, str):
        raise ValueError('fixture session user must be a string')
    installation = hashlib.sha256(canonical_scope_path(root).encode()).hexdigest()
    account = hashlib.sha256((user or canonical_scope_path(profile)).encode()).hexdigest()
    return installation, account


HEALTH_FAILURE_CODES = frozenset({
    'runtime_health_verification_failed', 'runtime_health_profile_escape',
    'runtime_health_installation_not_private_owned', 'runtime_health_fields_mismatch',
    'runtime_health_os_disk_image_mismatch', 'runtime_health_disk_image_changed',
    'runtime_health_image_changed_during_inspection', 'runtime_health_digest_mismatch',
    'runtime_health_os_identity_changed',
})
HEALTH_FIELDS = (
    'protocol', 'pid', 'selected_image', 'exe', 'image_path', 'process_start_id',
    'runtime_version', 'bin_version', 'installation_root', 'installation_id',
    'profile', 'account_id', 'instance_id', 'generation',
)


class RuntimeHealthFailure(AssertionError):
    """Only fixed identifiers, never health values or exception text, leave the verifier."""
    def __init__(self, code, fields=()):
        self.code = code if type(code) is str and code in HEALTH_FAILURE_CODES else 'runtime_health_verification_failed'
        self.fields = tuple(name for name in HEALTH_FIELDS if name in fields)
        super().__init__(self.code)

    def diagnostic(self):
        return {'failure_code': self.code, 'failed_fields': list(self.fields)}


def verify_runtime_health(args, health, root, profile, pid=None):
    try:
        return _verify_runtime_health(args, health, root, profile, pid)
    except RuntimeHealthFailure:
        raise
    except Exception:
        # Malformed health/session data and OS errors still fail closed. Never
        # propagate an arbitrary message, path, account, or dynamically named type.
        raise RuntimeHealthFailure('runtime_health_verification_failed') from None


def _verify_runtime_health(args, health, root, profile, pid=None):
    import psutil
    root, profile = pathlib.Path(canonical_scope_path(root)), pathlib.Path(canonical_scope_path(profile))
    if not profile.is_relative_to(root):
        raise RuntimeHealthFailure('runtime_health_profile_escape')
    if os.name == 'posix' and (root.stat().st_uid != os.getuid() or root.stat().st_mode & 0o077):
        raise RuntimeHealthFailure('runtime_health_installation_not_private_owned')
    owner = psutil.Process(health['pid'])
    birth, created = process_start_id(owner.pid), owner.create_time()
    image = pathlib.Path(owner.exe()).resolve(strict=True)
    session_path = profile / 'session.json'
    session = json.loads(session_path.read_text(encoding='utf-8')) if session_path.exists() else {}
    installation_id, account_id = runtime_scope_ids(root, profile, session)
    selected_image = ((root / 'bin/runtime' / args.runtime_version /
                       ('rsrs-runtime.exe' if os.name == 'nt' else 'rsrs-runtime')).resolve()
                      if (os.environ.get('RSRS_ARTIFACT_SOURCE') == 'published-assets'
                          or (os.environ.get('ARTIFACT_SOURCE') == 'build'
                              and os.environ.get('VALIDATION_PROFILE') == 'full'
                              and os.environ.get('GITHUB_ACTIONS') == 'true'
                              and os.environ.get('RUNNER_ENVIRONMENT') == 'github-hosted')) else args.runtime_binary)
    checks = {
        'protocol': type(health.get('v')) is int and health['v'] == 1,
        'pid': pid is None or owner.pid == pid,
        'selected_image': same_existing_path(image, selected_image),
        'exe': same_existing_path(health.get('exe'), image),
        'image_path': same_existing_path(health.get('image_path'), image),
        'process_start_id': health.get('process_start_id') == birth,
        'runtime_version': health.get('runtime_version') == args.runtime_version,
        'bin_version': health.get('bin') == args.runtime_version,
        'installation_root': same_existing_path(health.get('installation_root'), root),
        'installation_id': health.get('installation_id') == installation_id,
        'profile': same_existing_path(health.get('data_dir'), profile),
        'account_id': health.get('account_id') == account_id,
        'instance_id': isinstance(health.get('instance_id'), str) and bool(health['instance_id']),
        'generation': type(health.get('generation')) is int and health['generation'] > 0,
    }
    failed = [name for name, passed in checks.items() if not passed]
    if failed:
        raise RuntimeHealthFailure('runtime_health_fields_mismatch', failed)
    identity = (owner.pid, birth, health['instance_id'], health['account_id'], health['generation'])
    previous = next((item for item in args.runtime_identity_receipts
                     if item['_identity'] == identity), None)
    def file_identity(stat):
        # Windows stat/fstat expose different legacy ctime semantics in Python 3.12.
        timestamp = stat.st_birthtime_ns if sys.platform == 'win32' else stat.st_ctime_ns
        return (stat.st_dev, stat.st_ino, stat.st_size, stat.st_mtime_ns, timestamp)
    # procfs binds the open file to the live Linux process image. Other platforms
    # prove a stable OS path/birth and the opened disk file, not loaded memory bytes.
    source = pathlib.Path('/proc', str(owner.pid), 'exe') if sys.platform.startswith('linux') else image
    with source.open('rb') as stream:
        opened = os.fstat(stream.fileno())
        opened_identity = file_identity(opened)
        if opened_identity != file_identity(image.stat()):
            raise RuntimeHealthFailure('runtime_health_os_disk_image_mismatch')
        if previous is None:
            digest = hashlib.file_digest(stream, 'sha256').hexdigest()
        else:
            if previous['_file_identity'] != opened_identity:
                raise RuntimeHealthFailure('runtime_health_disk_image_changed')
            digest = previous['image_sha256']
        if (file_identity(os.fstat(stream.fileno())) != opened_identity
                or file_identity(image.stat()) != opened_identity):
            raise RuntimeHealthFailure('runtime_health_image_changed_during_inspection')
        if digest != args.runtime_sha256 or health.get('image_sha256') != digest:
            raise RuntimeHealthFailure('runtime_health_digest_mismatch')
        # New psutil objects force independent post-inspection OS queries.
        if (process_start_id(owner.pid) != birth or psutil.Process(owner.pid).create_time() != created
                or pathlib.Path(psutil.Process(owner.pid).exe()).resolve(strict=True) != image):
            raise RuntimeHealthFailure('runtime_health_os_identity_changed')
    if previous is None:
        receipt = {key: health[key] for key in ('installation_root', 'data_dir', 'account_id',
                   'instance_id', 'generation', 'process_start_id')}
        receipt.update(exe=str(image), os_image_path=str(image), image_sha256=digest,
                       disk_file_digest=digest, file_identity=list(opened_identity),
                       file_identity_fields=['device', 'inode', 'size', 'mtime_ns',
                                             'birthtime_ns' if sys.platform == 'win32' else 'ctime_ns'],
                       image_evidence=('linux_proc_exe_handle' if sys.platform.startswith('linux') else
                                       'stable_os_path_birth_and_open_disk_file'),
                       loaded_memory_digest=False, pid=owner.pid, create_time=created,
                       protocol_v=health['v'], runtime_version=args.runtime_version,
                       manifest_sha256=os.environ.get('RSRS_PUBLISHED_ASSETS_SHA256'),
                       bundle_sha256=os.environ.get('RSRS_PUBLISHED_BUNDLE_SHA256'),
                       provenance_evidence='upstream_verified_digest_references',
                       _identity=identity, _file_identity=opened_identity)
    else:
        receipt = previous
    if previous is None:
        args.runtime_identity_receipts.append(receipt)
    return {key: value for key, value in receipt.items() if not key.startswith('_')}


def runtime_report(args, passed):
    receipts = [{key: value for key, value in item.items() if not key.startswith('_')}
                for item in getattr(args, 'runtime_identity_receipts', [])]
    return {'runtime_mode': 'standalone_runtime' if args.runtime_binary else 'explicit_cli_shim',
            'standalone_runtime_evidence': bool(args.runtime_binary and passed and receipts),
            'runtime_binary_sha256': args.runtime_sha256 if args.runtime_binary else None,
            'runtime_version': args.runtime_version if args.runtime_binary else None,
            'runtime_identity_receipts': receipts}


def scoped_rpc_request(health, args, method='cli.exec'):
    scope = {key: health[key] for key in ('account_id', 'instance_id', 'generation')}
    if (type(health.get('v')) is not int or health['v'] != 1
            or not all(isinstance(scope[key], str) and scope[key]
            for key in ('account_id', 'instance_id'))
            or not isinstance(scope['generation'], int) or isinstance(scope['generation'], bool)
            or scope['generation'] <= 0):
        raise AssertionError('RPC requires complete health scope')
    operation = secrets.token_hex(16)
    return {'v': 1, 'id': secrets.token_hex(16), 'operation_id': operation,
            'idempotency_key': operation, 'scope': scope, 'method': method, 'args': args}


def verify_rpc_receipt(request, response):
    scope = response.get('scope')
    if (type(response.get('v')) is not int or not isinstance(scope, dict)
            or type(scope.get('generation')) is not int):
        raise AssertionError('RPC receipt protocol/scope type mismatch')
    for key in ('v', 'id', 'operation_id', 'idempotency_key', 'scope'):
        if response.get(key) != request[key]:
            raise AssertionError('RPC receipt mismatch: ' + key)
    if (not isinstance(response.get('exit'), int) or isinstance(response['exit'], bool)
            or response['exit'] < 0 or response.get('ok') is not (response['exit'] == 0)):
        raise AssertionError('RPC receipt ok/exit mismatch')


if __name__ == '__main__':
    if sys.argv[1] == '--install-published':
        print(install_published_runtime(sys.argv[2], dict(os.environ)))
    else:
        serve(sys.argv[1])
