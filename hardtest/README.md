# NSH HardTest

[Русская версия](README.ru.md)

HardTest runs a finite set of deterministic scenarios; it does not certify POSIX conformance or prove the absence of bugs or leaks. Workloads and their expected results are in `workloads.py`; the POSIX, Lua and PTY scenarios also use existing project tests. `tests/differential.py` compares status, output and filesystem state against recorded expectations, accounting for known reference-shell differences. A difference from a single shell is not automatically an NSH bug.

<a id="запуск"></a>

## Running

Linux with Docker, cgroup v2 and Python 3.12+ is required. Run from the repository root as an ordinary user with Docker access:

```sh
docker build -f hardtest/Dockerfile -t nsh-hardtest:local .
python3 -m unittest hardtest.test_hardtest
python3 hardtest/runner.py --selftest
python3 hardtest/runner.py --profile QUICK --output .build/hardtest-quick
python3 hardtest/runner.py --profile STRESS --output .build/hardtest-stress
python3 hardtest/runner.py --profile EXTREME --output .build/hardtest-extreme
```

In GitHub Actions select **Actions → HardTest → Run workflow → EXTREME**. EXTREME is manual-only; pull requests and pushes to main run QUICK, and the Sunday schedule runs STRESS. The job has a 45-minute timeout. Reports are uploaded even if tests fail. The `verify_failure_artifact` input deliberately fails a manual run after testing to verify artifact upload. The image is built from the current tree and compiles NSH and pinned Dash/Yash versions; image building requires network access, but test containers do not get network access.

Each scenario runs in its own container without root, networking, privileges, capabilities or a mount of the source working directory; the root filesystem is read-only. Limits: 512 MiB memory without swap, one CPU quota, 64 processes per cgroup, a 64 MiB temporary filesystem, 2 MiB per output file, 512 file descriptors, 20 CPU seconds per process and a per-test timeout. `/tmp` permits execution, as required by the POSIX tests; its size is limited and it disappears with the container. A check inside the container refuses to run unless cgroup v2 confirms the memory, CPU and process limits. On timeout, the process group is killed and the container and all its processes are removed; the workflow also removes leftover containers. The Docker daemon itself is privileged: run HardTest only on a trusted, isolated runner.

`report.json`, `report.md`, `metrics.json` and the `failures/` directory are written to the selected output directory. For each failure the scenario and bounded stdout/stderr are saved and the scenario is retried; a reproducible failure in the repeated-command workload is also minimized. Reproduce with `python3 hardtest/runner.py --case PATH/TO/SCENARIO.json`. Use a separate output directory for each run so old `failures/` files do not mix with new ones. Scenarios are fixed and no random seeds are used. RSS is sampled only for the leading process, so short peaks may be missed and descendant memory is not included. Monotonic growth with enough samples is flagged as suspicious, not as a proven leak. OOM and reaching a configured limit are reported separately and are not in themselves classified as NSH bugs. Docker launch failures are infrastructure failures.

<a id="экспериментальный-hardtest-score"></a>

## Experimental HardTest Score

The score depends on the profile, available resources, OS and current test suite. It is not a ranking for comparing computers. Elapsed time is not included. Each fraction below is the fraction of scenarios passed; an unavailable category scores 0. The final result is rounded to an integer:

- Correctness: 3000 × the average pass fraction of parser, execution, posix and recovery categories.
- Stability: 2500 × the fraction of all scenarios passed.
- Memory: 1500 × the fraction of memory scenarios passed.
- Load capacity: 1500 × `min(maximum successful workload / 100000, 1)`; this measures completed workload size, not speed.
- Lua and interactivity: 1500 × the average pass fraction of lua and signals categories.

If a crash, timeout, wrong status/output, unexpected stderr or infrastructure failure is detected, the total is capped at 3000. A high completed workload cannot offset a critical failure. Repeatability assumes the same versions, limits and results, including no incidental runner timeouts.
