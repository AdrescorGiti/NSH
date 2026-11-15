#!/usr/bin/env python3
"""Profile shell-only fixture time and allocator requests on Linux/glibc."""
import argparse
import os
from pathlib import Path
import re
import statistics
import subprocess
import tempfile
import time


PROJECT = Path(__file__).resolve().parents[1]
FIXTURES = (
    ("plain", b": item item item item item item item item item item\n" * 20_000, []),
    ("quoted", b"v='word with spaces'\n" + b': "$v" "$v" "$v" "$v" "$v"\n' * 20_000, []),
    ("split", b"v='x y z q r t'\n" + b": $v $v $v $v $v\n" * 20_000, []),
    (
        "parameters",
        b"v=prefix/suffix; u=''; unset absent\n"
        + b': "${v#prefix/}" "${v%/suffix}" "${u:-alternative}" "${absent:-word}" "$v"\n'
        * 20_000,
        [],
    ),
    ("arithmetic", b"n=0\n" + b"n=$((n + 1))\n" * 20_000, []),
    ("case", b"value=gamma\n" + b'case "$value" in alpha|beta) : ;; gamma) : ;; esac\n' * 20_000, []),
    (
        "patterns",
        b"value='long-prefix/partial/file.txt'\n"
        + b': "${value#*/}" "${value%%/*}" "${value%.*}"\n' * 20_000,
        [],
    ),
    (
        "parse",
        b'if :; then score=1; : argument longer_argument "longer quoted value"; else : nope; fi\n'
        * 20_000,
        ["-n"],
    ),
)

INTERPOSER = r"""
#define _GNU_SOURCE
#include <errno.h>
#include <stdatomic.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <unistd.h>

extern void *__libc_malloc(size_t);
extern void *__libc_calloc(size_t, size_t);
extern void *__libc_realloc(void *, size_t);
extern void *__libc_memalign(size_t, size_t);
static _Atomic size_t malloc_calls, calloc_calls, realloc_calls, aligned_calls, bytes;
static _Atomic int printed;

void *malloc(size_t length) {
    malloc_calls++;
    bytes += length;
    return __libc_malloc(length);
}
void *calloc(size_t count, size_t size) {
    calloc_calls++;
    if (size == 0 || count <= (size_t)-1 / size) bytes += count * size;
    return __libc_calloc(count, size);
}
void *realloc(void *old, size_t length) {
    realloc_calls++;
    bytes += length;
    return __libc_realloc(old, length);
}
void *aligned_alloc(size_t alignment, size_t length) {
    aligned_calls++;
    bytes += length;
    return __libc_memalign(alignment, length);
}
int posix_memalign(void **output, size_t alignment, size_t length) {
    aligned_calls++;
    bytes += length;
    if (alignment < sizeof(void *) || alignment & (alignment - 1)) return EINVAL;
    void *data = __libc_memalign(alignment, length);
    if (!data) return ENOMEM;
    *output = data;
    return 0;
}
__attribute__((destructor)) static void counters(void) {
    if (atomic_exchange(&printed, 1)) return;
    char line[256];
    int size = snprintf(line, sizeof(line),
        "malloc=%zu calloc=%zu realloc=%zu aligned=%zu requested=%zu\n",
        (size_t)malloc_calls, (size_t)calloc_calls, (size_t)realloc_calls,
        (size_t)aligned_calls, (size_t)bytes);
    if (size > 0 && (size_t)size < sizeof(line)) write(STDERR_FILENO, line, size);
}
/* Shells using _exit() skip normal ELF destructors. */
void _exit(int code) { counters(); syscall(SYS_exit_group, code); __builtin_unreachable(); }
void _Exit(int code) { _exit(code); }
"""
REPORT = re.compile(rb"malloc=(\d+) calloc=(\d+) realloc=(\d+) aligned=(\d+) requested=(\d+)\n")


def executable(path):
    path = Path(path).resolve()
    if not os.access(path, os.X_OK) or not path.is_file():
        raise argparse.ArgumentTypeError(f"missing executable: {path}")
    return str(path)


def run(argv, env, cwd, instrumented=False):
    start = time.perf_counter_ns()
    completed = subprocess.run(
        argv, env=env, cwd=cwd, capture_output=True, stdin=subprocess.DEVNULL, timeout=15,
    )
    elapsed = (time.perf_counter_ns() - start) / 1_000_000
    if completed.returncode != 0 or completed.stdout:
        raise RuntimeError((argv, completed.returncode, completed.stdout[:500], completed.stderr[:500]))
    if instrumented:
        count = REPORT.fullmatch(completed.stderr)
        if count is None:
            raise RuntimeError((argv, "expected one allocator report", completed.stderr))
        return tuple(map(int, count.groups()))
    if completed.stderr:
        raise RuntimeError((argv, completed.stderr))
    return elapsed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("nsh", type=executable)
    parser.add_argument("--cpu", type=int)
    parser.add_argument("--rounds", type=int, default=11)
    args = parser.parse_args()
    if args.rounds < 1:
        parser.error("--rounds must be positive")
    if args.cpu is not None:
        os.sched_setaffinity(0, {args.cpu})
    build = PROJECT / ".build"
    build.mkdir(exist_ok=True)
    print(f"rounds={args.rounds} cpus={','.join(map(str, sorted(os.sched_getaffinity(0))))} locale=C")
    print(f"NSH: {args.nsh}")
    print("Allocation totals are requests observed through glibc symbols, not peak or live bytes.")
    print("Timing runs exclude LD_PRELOAD. Each workload uses 20,000 shell-only lines.")
    with tempfile.TemporaryDirectory(prefix="allocation-profile-", dir=build) as temporary:
        scratch = Path(temporary)
        source = scratch / "alloc.c"
        helper = scratch / "alloc.so"
        source.write_text(INTERPOSER)
        subprocess.run(["cc", "-O2", "-shared", "-fPIC", "-o", helper, source], check=True)
        baseline_env = {"LC_ALL": "C", "PATH": "/usr/bin:/bin", "HOME": temporary}
        profiler_env = {**baseline_env, "LD_PRELOAD": str(helper)}
        for name, content, switches in FIXTURES:
            script = scratch / f"{name}.sh"
            script.write_bytes(content)
            argv = [args.nsh, "--vanilla", *switches, str(script)]
            samples = [run(argv, baseline_env, scratch) for _ in range(args.rounds)]
            malloc, calloc, realloc, aligned, requested = run(argv, profiler_env, scratch, True)
            print(
                f"{name:12s} {statistics.median(samples):8.3f} ms  "
                f"malloc={malloc} calloc={calloc} realloc={realloc} aligned={aligned} "
                f"requested={requested}",
                flush=True,
            )


if __name__ == "__main__":
    main()
