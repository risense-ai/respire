"""Pinned desktop inputs, source coverage and Linux process checks."""
import os
from pathlib import Path
import sys
from client2_launcher_baselines import DESKTOP_BASELINES
from client2_launcher_baselines import sha256


def coverage_profile(value):
    if value is None:
        return None
    if (os.environ.get('GITHUB_ACTIONS') != 'true'
            or os.environ.get('RUNNER_ENVIRONMENT') != 'github-hosted'):
        raise RuntimeError('Coverage forwarding requires an isolated hosted runner')
    expected = Path(__file__).resolve().parents[1] / 'target' / 'llvm-cov-target'
    if expected.resolve(strict=True) != expected:
        raise RuntimeError('Coverage directory must not redirect outside the source workspace')
    template = Path(value)
    if not template.is_absolute() or template.parent.resolve(strict=True) != expected:
        raise RuntimeError('Coverage profile parent must be the current source coverage directory')
    plain_name = template.name.replace('%m', '').replace('%p', '')
    if ('%m' not in template.name or '%p' not in template.name
            or not template.name.endswith('.profraw')
            or not plain_name or any(not (char.isascii() and (char.isalnum() or char in '_.-'))
                                     for char in plain_name)):
        raise RuntimeError('Coverage profile must retain the process and module placeholders')
    return str(expected / template.name)


def desktop_executable(value, baseline='v2.0.0-dev.5'):
    if value is None:
        return None
    if (os.environ.get('GITHUB_ACTIONS') != 'true'
            or os.environ.get('RUNNER_ENVIRONMENT') != 'github-hosted'
            or sys.platform != 'linux' or os.uname().machine != 'x86_64'):
        raise RuntimeError('Pinned desktop acceptance requires an isolated Linux x64 hosted runner')
    path = value.resolve(strict=True)
    if not path.is_file() or sha256(path) != DESKTOP_BASELINES[baseline]['binary']:
        raise RuntimeError(f'Require the exact released Client2 {baseline} Linux x64 desktop bytes')
    return path


def linux_runtime_record(pid):
    if type(pid) is not int or pid <= 0 or pid == os.getpid():
        raise RuntimeError('Invalid desktop Runtime PID')
    return Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()


def verify_desktop_runtime(health, root, image, port):
    pid = health['pid']
    before = linux_runtime_record(pid)
    assert before[0] != 'Z'
    assert health['process_start_id'] == 'linux-starttime-' + before[19]
    assert health['server'] == 'respire' and health['v'] == 1
    assert Path(health['installation_root']).resolve(strict=True) == root
    assert Path(health['data_dir']).resolve(strict=True).is_relative_to(root)
    assert Path(health['exe']).resolve(strict=True) == image
    assert Path(health['image_path']).resolve(strict=True) == image
    assert Path(f'/proc/{pid}/exe').resolve(strict=True) == image
    assert health['image_sha256'] == sha256(image)
    # Bind the reported PID to the actual IPv4 loopback listening socket.
    sockets = set()
    for fd in Path(f'/proc/{pid}/fd').iterdir():
        try:
            sockets.add(os.readlink(fd))
        except FileNotFoundError:
            continue  # Other Runtime workers may close unrelated descriptors during inspection.
    listeners = [line.split() for line in Path('/proc/net/tcp').read_text().splitlines()[1:]]
    assert any(row[1] == f'0100007F:{port:04X}' and row[3] == '0A'
               and f'socket:[{row[9]}]' in sockets for row in listeners)
    after = linux_runtime_record(pid)
    assert after[0] != 'Z' and after[19] == before[19]
    return {key: health[key] for key in ['pid', 'installation_id', 'instance_id',
            'process_start_id', 'image_sha256', 'runtime_version', 'account_id', 'generation']}
