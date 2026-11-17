#!/usr/bin/env python3
"""Execute one generated case in a resource-limited, disposable container."""
import base64
import json
import os
from pathlib import Path
import resource
import signal
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, '/opt/nsh')
from hardtest.core import classify, parser_limit


def bounded_environment():
    if os.geteuid() == 0:
        raise RuntimeError('refusing to run test as root')
    cg = Path('/sys/fs/cgroup')
    try:
        memory = (cg / 'memory.max').read_text().strip()
        pids = (cg / 'pids.max').read_text().strip()
        cpu = (cg / 'cpu.max').read_text().split()
        if memory == 'max' or int(memory) > 768 * 1024 * 1024:
            raise ValueError('memory limit missing or too high')
        if pids == 'max' or int(pids) > 64:
            raise ValueError('process limit missing or too high')
        if cpu[0] == 'max' or int(cpu[0]) > int(cpu[1]):
            raise ValueError('CPU quota missing or above one CPU')
    except (OSError, ValueError, IndexError) as exc:
        raise RuntimeError(f'refusing to run without verified cgroup v2 limits: {exc}') from exc


def set_limits():
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    resource.setrlimit(resource.RLIMIT_FSIZE, (2 * 1024 * 1024, 2 * 1024 * 1024))
    resource.setrlimit(resource.RLIMIT_NOFILE, (512, 512))
    resource.setrlimit(resource.RLIMIT_CPU, (20, 21))


def run_case(case):
    bounded_environment()
    start = time.monotonic()
    def oom_kills():
        try:
            return int(dict(line.split() for line in Path('/sys/fs/cgroup/memory.events').read_text().splitlines()).get('oom_kill', 0))
        except (OSError, ValueError):
            return 0
    oom_before = oom_kills()
    peak = 0
    samples = []
    with tempfile.TemporaryDirectory(prefix='hardtest-', dir='/tmp') as directory:
        home = Path(directory)
        env = {'PATH': '/usr/bin:/bin', 'HOME': directory, 'TMPDIR': directory,
               'LC_ALL': 'C.UTF-8', 'TERM': 'xterm-256color',
               'XDG_CONFIG_HOME': directory + '/config', 'XDG_STATE_HOME': directory + '/state'}
        if 'script' in case:
            script = home / 'case.sh'
            script.write_text(case['script'], encoding='utf-8')
            args = ['/opt/nsh/target/release/nsh', '--vanilla', str(script)]
        else:
            args = case['argv']
        with tempfile.TemporaryFile(dir=directory) as output, tempfile.TemporaryFile(dir=directory) as error:
            process = subprocess.Popen(args, cwd=directory, env=env, stdin=subprocess.DEVNULL,
                                       stdout=output, stderr=error, start_new_session=True,
                                       preexec_fn=set_limits)
            timed_out = False
            while process.poll() is None:
                if time.monotonic() - start > case.get('timeout', 12):
                    timed_out = True
                    break
                try:
                    for line in Path(f'/proc/{process.pid}/status').read_text().splitlines():
                        if line.startswith('VmRSS:'):
                            rss = int(line.split()[1]); peak = max(peak, rss)
                            if len(samples) < 200:
                                samples.append(rss)
                            break
                except (OSError, ValueError):
                    pass
                time.sleep(0.02)
            # Kill the process group even after the leader exits, so descendants
            # cannot hold resources until the enclosing container exits.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
            output.seek(0); error.seek(0)
            stdout = output.read(2 * 1024 * 1024 + 1)
            stderr = error.read(2 * 1024 * 1024 + 1)
        if oom_kills() > oom_before:
            result = 'oom'
        elif timed_out:
            result = 'timeout'
        elif process.returncode in (-signal.SIGXCPU, -signal.SIGXFSZ):
            result = 'resource_limit'
        elif case.get('parser_limit') and parser_limit(process.returncode, stdout, stderr):
            result = 'resource_limit'
        elif case.get('suite'):
            result = 'pass' if process.returncode == 0 else ('crash' if process.returncode < 0 else 'wrong_exit_status')
        else:
            result = classify(process.returncode, stdout, stderr,
                              case.get('status', 0), case.get('stdout', '').encode(),
                              None if case.get('stderr') is None else case['stderr'].encode())
        suspicious = len(samples) >= 8 and all(b >= a for a, b in zip(samples, samples[1:])) and samples[-1] - samples[0] >= 1024
        return {'rss_monotonic_growth_suspected': suspicious, 'outcome': result, 'status': process.returncode, 'duration_s': round(time.monotonic() - start, 4),
                'peak_rss_kib': peak, 'rss_samples_kib': samples,
                'stdout_b64': base64.b64encode(stdout).decode(), 'stderr_b64': base64.b64encode(stderr).decode()}


if __name__ == '__main__':
    try:
        print(json.dumps(run_case(json.load(sys.stdin))))
    except Exception as exc:
        print(json.dumps({'outcome': 'infrastructure_failure', 'detail': str(exc)}))
        sys.exit(2)
