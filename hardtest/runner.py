#!/usr/bin/env python3
"""Host-side HardTest runner. Every case runs in a separate limited container."""
import argparse
import base64
from collections import Counter, defaultdict
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tomllib
import time
import uuid

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))
from hardtest.core import score
from hardtest.workloads import cases

IMAGE = 'nsh-hardtest:local'


def docker(args, **kwargs):
    return subprocess.run(['docker', *args], check=False, text=True, **kwargs)


def run_case(case, image=IMAGE):
    name = 'nsh-hardtest-' + uuid.uuid4().hex
    cmd = ['run', '--name', name, '--label', 'nsh-hardtest=true', '--init', '--network=none',
           '--read-only', '--cap-drop=ALL', '--security-opt=no-new-privileges',
           '--user=65534:65534', '--memory=512m', '--memory-swap=512m', '--cpus=1',
           '--pids-limit=64', '--tmpfs', '/tmp:rw,nosuid,nodev,exec,size=64m',
           '--ulimit', 'core=0', '--ulimit', 'nofile=512:512', '-i', image]
    try:
        try:
            proc = docker(cmd, input=json.dumps(case), capture_output=True,
                          timeout=case.get('timeout', 12) + 15)
        except subprocess.TimeoutExpired:
            docker(['kill', name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
            return {'outcome': 'timeout', 'detail': 'container exceeded outer timeout'}
        info = docker(['inspect', '--format', '{{.State.OOMKilled}}', name], capture_output=True, timeout=10)
        if info.returncode == 0 and info.stdout.strip() == 'true':
            return {'outcome': 'oom', 'detail': 'container memory limit reached'}
        try:
            result = json.loads(proc.stdout)
        except (ValueError, TypeError):
            return {'outcome': 'infrastructure_failure', 'detail': f'docker exit {proc.returncode}: {proc.stderr[-2000:]}'}
        return result
    except (OSError, subprocess.SubprocessError) as exc:
        return {'outcome': 'infrastructure_failure', 'detail': str(exc)}
    finally:
        try:
            docker(['rm', '-f', name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
        except (OSError, subprocess.SubprocessError):
            pass


def text_data(value):
    return base64.b64decode(value).decode('utf-8', errors='backslashreplace') if value else ''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', choices=['QUICK', 'STRESS', 'EXTREME'], default='QUICK')
    parser.add_argument('--output', type=Path, default=ROOT / '.build/hardtest')
    parser.add_argument('--image', default=IMAGE)
    parser.add_argument('--selftest', action='store_true')
    parser.add_argument('--case', type=Path, help='run one saved case in the same isolation')
    args = parser.parse_args()
    if os.geteuid() == 0:
        parser.error('refusing to launch HardTest as root')
    args.output.mkdir(parents=True, exist_ok=True)
    failures = args.output / 'failures'; failures.mkdir(exist_ok=True)
    if args.selftest:
        checks = [
            (dict(name='timeout', argv=['/usr/local/bin/python3', '-c', 'import os,time; os.fork(); time.sleep(30)'], suite=True, timeout=1), 'timeout'),
            (dict(name='crash', argv=['/usr/local/bin/python3', '-c', 'import os,signal; os.kill(os.getpid(),signal.SIGSEGV)'], suite=True), 'crash'),
        ]
        good = True
        for case, expected in checks:
            result = run_case(case, args.image); print(case['name'], result.get('outcome'), flush=True)
            good &= result.get('outcome') == expected
        return 0 if good else 1
    generated = [json.loads(args.case.read_text())] if args.case else cases(args.profile)
    results = []
    stop_adaptive = False
    stop_nesting = False
    for case in generated:
        if (stop_adaptive and case['name'].startswith('commands-')) or (stop_nesting and case['name'].startswith('nesting-')):
            continue
        print(f"{case['category']}/{case['name']} ...", flush=True)
        result = run_case(case, args.image)
        result.update({key: case.get(key) for key in ('name', 'category', 'load')})
        result['seed'] = None  # Workloads are deterministic; no random seed is used.
        outcome = result['outcome']
        if case['name'].startswith('commands-') and outcome != 'pass':
            stop_adaptive = True
        if case['name'].startswith('nesting-') and outcome != 'pass':
            stop_nesting = True
        if outcome not in ('pass', 'resource_limit', 'oom'):
            retry = run_case(case, args.image)
            result['retry_outcome'] = retry['outcome']
            # Minimize the generated repeated-command workload only when the
            # same failure reproduces; preserve the original independently.
            if case['name'].startswith('commands-') and retry['outcome'] == outcome:
                low, high = 1, case['load']
                for _ in range(12):
                    if high - low <= 1:
                        break
                    mid = (low + high) // 2
                    candidate = dict(case, load=mid, script=':\n' * mid + "printf 'ok\\n'\n")
                    if run_case(candidate, args.image)['outcome'] == outcome:
                        high = mid
                    else:
                        low = mid
                candidate = dict(case, load=high, script=':\n' * high + "printf 'ok\\n'\n")
                (failures / (case['name'] + '-min.json')).write_text(json.dumps(candidate, indent=2) + '\n')
                result['minimized_load'] = high
            (failures / (case['name'] + '.json')).write_text(json.dumps(case, indent=2) + '\n')
            (failures / (case['name'] + '.txt')).write_text(
                'stdout:\n' + text_data(result.get('stdout_b64')) + '\nstderr:\n' + text_data(result.get('stderr_b64')))
            result['reproduce'] = f"python3 hardtest/runner.py --case {failures / (case['name'] + '.json')}"
        result.pop('stdout_b64', None); result.pop('stderr_b64', None)
        results.append(result)
        print(' ', outcome, flush=True)
    write_report(args, results)
    return 1 if any(r['outcome'] not in ('pass', 'resource_limit', 'oom') for r in results) else 0


def write_report(args, results):
    counts = Counter(r['outcome'] for r in results)
    categories = defaultdict(list)
    for result in results:
        categories[result['category']].append(result)
    ratios = {name: sum(r['outcome'] == 'pass' for r in group) / len(group) for name, group in categories.items()}
    def mean(names):
        return sum(ratios.get(name, 0) for name in names) / len(names)
    successful_loads = [r['load'] for r in results if r.get('load') and r['outcome'] == 'pass']
    max_load = max(successful_loads, default=0)
    components = {'correctness': mean(['parser', 'execution', 'posix', 'recovery']),
                  'stability': counts['pass'] / len(results), 'memory': ratios.get('memory', 0),
                  'performance': min(max_load / 100000, 1), 'interactive': mean(['lua', 'signals'])}
    critical = any(r['outcome'] in ('crash', 'timeout', 'wrong_output', 'wrong_exit_status', 'unexpected_stderr', 'infrastructure_failure') for r in results)
    commit = subprocess.run(['git', 'rev-parse', 'HEAD'], cwd=ROOT, capture_output=True, text=True).stdout.strip() or 'unknown'
    with (ROOT / 'Cargo.toml').open('rb') as source:
        version = tomllib.load(source)['package']['version']
    dirty = bool(subprocess.run(['git', 'status', '--porcelain'], cwd=ROOT, capture_output=True, text=True).stdout.strip())
    try:
        cpu = next(line.split(':', 1)[1].strip() for line in Path('/proc/cpuinfo').read_text().splitlines() if line.startswith('model name'))
    except (OSError, StopIteration):
        cpu = platform.processor() or 'unknown'
    metadata = {'profile': args.profile, 'commit': commit, 'working_tree_dirty': dirty, 'nsh_version': version,
                'platform': platform.platform(), 'architecture': platform.machine(),
                'cpu': cpu, 'created_utc': datetime.now(timezone.utc).isoformat(),
                'score_experimental': score(components, critical), 'score_components': components,
                'counts': dict(counts), 'scenarios': len(results), 'max_successful_load': max_load,
                'peak_rss_kib': max((r.get('peak_rss_kib', 0) for r in results), default=0)}
    (args.output / 'report.json').write_text(json.dumps({'metadata': metadata, 'results': results}, indent=2) + '\n')
    (args.output / 'metrics.json').write_text(json.dumps({'metadata': metadata, 'cases': [
        {k: r.get(k) for k in ('name', 'duration_s', 'peak_rss_kib', 'rss_samples_kib', 'rss_monotonic_growth_suspected', 'load')} for r in results]}, indent=2) + '\n')
    lines = ['# NSH HardTest', '', f"Profile: {args.profile}; commit: {commit}; score: {metadata['score_experimental']}/10000 (experimental)",
             '', f"NSH: {version}; OS: {metadata['platform']}; CPU: {cpu}. Random seeds: not applicable (all cases deterministic).",
             '', f"Scenarios: {len(results)}; outcomes: {dict(counts)}; peak observed RSS: {metadata['peak_rss_kib']} KiB; max successful load: {max_load}.",
             '', '| Category | Passed | Total |', '|---|---:|---:|']
    for name, group in sorted(categories.items()):
        lines.append(f"| {name} | {sum(r['outcome'] == 'pass' for r in group)} | {len(group)} |")
    for r in results:
        if r['outcome'] != 'pass':
            lines += ['', f"- {r['name']}: {r['outcome']} (retry: {r.get('retry_outcome', 'not retried')})", f"  {r.get('reproduce', '')}"]
    (args.output / 'report.md').write_text('\n'.join(lines) + '\n')


if __name__ == '__main__':
    raise SystemExit(main())
