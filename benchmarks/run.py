#!/usr/bin/env python3
"""Compare five released shells on validated Linux workloads."""
import argparse
import ctypes
from dataclasses import dataclass
from datetime import datetime, timezone
import hashlib
import json
import mmap
import os
from pathlib import Path
import platform
import re
import shutil
import statistics
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
SHELLS = ("NSH", "Dash", "Zsh", "Fish", "Bash")
FLAGS = {
    "NSH": ("--vanilla",),
    "Dash": (),
    "Zsh": ("-f", "-d"),
    "Fish": ("--no-config",),
    "Bash": ("--noprofile", "--norc"),
}
STRACE = ROOT / ".build/references/strace/usr/bin/strace"
SYSTEM_PATH = "/usr/bin:/bin"
PAGE_SIZE = os.sysconf("SC_PAGE_SIZE")
LIBC = ctypes.CDLL(None, use_errno=True)
LIBC.mincore.argtypes = (ctypes.c_void_p, ctypes.c_size_t, ctypes.c_void_p)
LIBC.mincore.restype = ctypes.c_int


@dataclass(frozen=True)
class Workload:
    name: str
    expected: bytes
    source: bytes | None = None
    fish_source: bytes | None = None

WORDS = b" alpha beta" * 20
FUNCTION_CALLS = {
    "posix": (
        b"""visit() { printf '<%s>\\n' "$1"; }\n"""
        b"for outer in" + WORDS + b"; do\n"
        b"    for inner in" + WORDS + b'; do visit "$inner"; done\n'
        b"done\n"
    ),
    "fish": (
        b"""function visit; printf '<%s>\\n' "$argv[1]"; end\n"""
        b"for outer in" + WORDS + b"\n"
        b"    for inner in" + WORDS + b'\n        visit "$inner"\n    end\n'
        b"end\n"
    ),
}
COMPOUND_BODY = b"    true\n" * 1200
SUITES = {
    "startup_warm": Workload("Запуск, прогретый ELF", b""),
    "startup_cold": Workload("Запуск, холодные страницы ELF", b""),
    "builtins": Workload("2000 вызовов встроенной true", b"", b"true\n" * 2000),
    "printf": Workload(
        "800 вызовов встроенной printf", b"<entry> 0017\n" * 800,
        b"printf '<%s> %04d\\n' entry 17\n" * 800,
    ),
    "functions": Workload(
        "1600 вызовов функции в двух циклах", b"<alpha>\n<beta>\n" * 800,
        FUNCTION_CALLS["posix"], FUNCTION_CALLS["fish"],
    ),
    "compound": Workload(
        "Многострочный блок из 1200 команд", b"done\n",
        b"if true; then\n" + COMPOUND_BODY + b"fi\nprintf 'done\\n'\n",
        b"if true\n" + COMPOUND_BODY + b"end\nprintf 'done\\n'\n",
    ),
    "pipeline": Workload(
        "Пайплайн head и wc, 16 МиБ", b"16777216\n",
        b"/usr/bin/head -c 16777216 /dev/zero | /usr/bin/wc -c\n",
    ),
    "external": Workload(
        "100 запусков /usr/bin/true", b"", b"/usr/bin/true || exit 1\n" * 100,
    ),
    "external_path": Workload(
        "100 запусков cat через PATH", b"", b"cat /dev/null || exit 1\n" * 100,
    ),
}


def version_number(text):
    match = re.search(r"\b[0-9]+(?:\.[0-9]+)+\b", text)
    if match is None:
        raise ValueError(f"cannot identify a version in {text!r}")
    return match.group()


def describe(shell, binary, dash_version, directory, env):
    if shell == "Dash":
        reported = dash_version
    else:
        reported = subprocess.check_output(
            [binary, "--version"], cwd=directory, env=env,
            stdin=subprocess.DEVNULL, text=True, timeout=10,
        ).splitlines()[0]
    version = version_number(reported)
    public_name = (
        str(binary.relative_to(ROOT)) if binary.is_relative_to(ROOT)
        else str(binary) if binary.parent == Path("/usr/bin")
        else binary.name
    )
    with binary.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    return {
        "version": version,
        "version_number": version,
        "version_reported": reported,
        "binary": public_name,
        "binary_bytes": binary.stat().st_size,
        "sha256": digest,
        "options": FLAGS[shell],
    }


def cold_copy(binary, directory):
    copy = directory / binary.name
    copy.unlink(missing_ok=True)
    with binary.open("rb") as source, copy.open("x+b") as target:
        shutil.copyfileobj(source, target)
        size = target.tell()
        target.flush()
        os.fchmod(target.fileno(), 0o700)
        os.fsync(target.fileno())
        os.posix_fadvise(target.fileno(), 0, 0, os.POSIX_FADV_DONTNEED)
        page_status = (ctypes.c_ubyte * ((size + PAGE_SIZE - 1) // PAGE_SIZE))()
        with mmap.mmap(target.fileno(), size, access=mmap.ACCESS_COPY) as mapping:
            address = ctypes.addressof(ctypes.c_char.from_buffer(mapping))
            if LIBC.mincore(address, size, page_status):
                code = ctypes.get_errno()
                raise OSError(code, os.strerror(code))
        if any(byte & 1 for byte in page_status):
            raise RuntimeError("mincore reported resident pages in the cold ELF copy")
    return copy


def checked_time(args, expected, directory, env):
    start = time.perf_counter_ns()
    result = subprocess.run(args, cwd=directory, env=env, stdin=subprocess.DEVNULL,
                            capture_output=True, timeout=15)
    elapsed = time.perf_counter_ns() - start
    if (result.returncode, result.stdout, result.stderr) != (0, expected, b""):
        raise ValueError(
            f"status={result.returncode}, stdout={result.stdout[:240]!r}, "
            f"stderr={result.stderr[:240]!r}, expected stdout={expected[:240]!r}",
        )
    return elapsed


def invocation(key, shell, binary, directory, fixtures, env):
    if key == "startup_cold":
        binary = cold_copy(binary, directory / "cold")
    args = [binary, *FLAGS[shell]]
    if key in ("startup_warm", "startup_cold"):
        args.extend(("-c", "true"))
    else:
        args.append(fixtures[key][shell])
    if key == "external_path":
        env = dict(env, PATH=f"{directory}/missing-one:{directory}/missing-two:{SYSTEM_PATH}")
    return args, env


def trace_processes(tracer, args, expected, directory, env):
    destination = directory / "trace"
    checked_time([tracer, "-f", "-qq", "-c", "-e", "trace=%process", "-o",
                  destination, *args], expected, directory, env)
    counts = {}
    for line in destination.read_text().splitlines():
        fields = line.split()
        if len(fields) not in (5, 6) or not fields[3].isdigit() or fields[-1] == "total":
            continue
        counts[fields[-1]] = {
            "calls": int(fields[3]), "errors": int(fields[4]) if len(fields) == 6 else 0,
        }
    if not {"execve", "execveat"} & counts.keys():
        raise ValueError("strace did not observe an exec")
    return counts


def execution_environment(directory):
    return {
        "PATH": SYSTEM_PATH,
        "HOME": str(directory),
        "LC_ALL": "C.UTF-8",
        "TZ": "UTC",
        "TERM": "dumb",
        "XDG_CONFIG_HOME": str(directory / "config"),
        "XDG_DATA_HOME": str(directory / "data"),
        "XDG_CACHE_HOME": str(directory / "cache"),
        "XDG_STATE_HOME": str(directory / "state"),
    }


def environment():
    cpu = re.search(r"^model name\s*:\s*(.*)$", Path("/proc/cpuinfo").read_text(), re.MULTILINE)
    cpus = sorted(os.sched_getaffinity(0))
    governors = set()
    for index in cpus:
        path = Path(f"/sys/devices/system/cpu/cpu{index}/cpufreq/scaling_governor")
        if path.exists():
            governors.add(path.read_text().strip())
    return {
        "date_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "distribution": platform.freedesktop_os_release().get("PRETTY_NAME", "Linux"),
        "kernel": platform.release(),
        "arch": platform.machine(),
        "cpu": cpu.group(1) if cpu else platform.processor(),
        "glibc": os.confstr("CS_GNU_LIBC_VERSION"),
        "affinity": cpus,
        "cpu_governors": sorted(governors),
        "python": platform.python_version(),
        "locale": "C.UTF-8",
        "path": SYSTEM_PATH,
        "timezone": "UTC",
        "term": "dumb",
        "strace": None,
    }


def write_fixtures(directory, keys):
    fixtures = {}
    for key in keys:
        workload = SUITES[key]
        if workload.source is None:
            continue
        shared = directory / f"{key}.sh"
        shared.write_bytes(workload.source)
        fixtures[key] = dict.fromkeys(SHELLS, shared)
        if workload.fish_source is not None:
            fish = directory / f"{key}.fish"
            fish.write_bytes(workload.fish_source)
            fixtures[key]["Fish"] = fish
    return fixtures


def collect(keys, binaries, tracer, directory, fixtures, env, samples, cold_samples):
    rows = {}
    for key in keys:
        total = cold_samples if key == "startup_cold" else samples
        expected = SUITES[key].expected
        measured = {shell: [] for shell in SHELLS}
        try:
            for index in range(-1, total):
                if key == "startup_cold" and index == -1:
                    continue
                # Each rotation changes which shell sees an earlier measurement.
                for shell in SHELLS[index % len(SHELLS):] + SHELLS[:index % len(SHELLS)]:
                    args, active = invocation(key, shell, binaries[shell], directory, fixtures, env)
                    ns = checked_time(args, expected, directory, active)
                    if index >= 0:
                        measured[shell].append({"ns": ns})
            engines = {}
            for shell, values in measured.items():
                times = [sample["ns"] for sample in values]
                bounds = statistics.quantiles(times, n=10, method="inclusive")
                row = {
                    "samples": values,
                    "median_ns": statistics.median(times),
                    "p10_ns": bounds[0],
                    "p90_ns": bounds[-1],
                }
                if tracer is not None and key != "startup_cold":
                    args, active = invocation(key, shell, binaries[shell], directory, fixtures, env)
                    row["syscalls"] = trace_processes(tracer, args, expected, directory, active)
                    execs = count_syscalls(row["syscalls"], ("execve", "execveat"), "calls")
                    if key in ("startup_warm", "builtins", "printf", "functions", "compound") and execs != 1:
                        raise ValueError(f"expected only the shell exec, observed {execs}")
                engines[shell] = row
            rows[key] = engines
        except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as cause:
            raise RuntimeError(f"{key}, {shell}: {cause}") from cause
        print(f"{key}: " + ", ".join(f"{name} {len(row['samples'])}/{total}"
                                     for name, row in rows[key].items()), flush=True)
    return rows


def count_syscalls(counts, names, field):
    return sum(counts.get(name, {}).get(field, 0) for name in names)


def timing(row):
    median, low, high = (row[key] / 1e6 for key in ("median_ns", "p10_ns", "p90_ns"))
    return f"{median:.3f} [{low:.3f}–{high:.3f}]"


def summary(rows, metadata, shells, stem):
    lines = [
        "# NSH, Dash, Zsh, Fish, Bash",
        "",
        f"{metadata['distribution']}, {metadata['arch']}, Linux {metadata['kernel']}, {metadata['glibc']}.",
        f"{metadata['cpu']}. Замер {metadata['date_utc']}.",
        f"CPU affinity {', '.join(map(str, metadata['affinity']))}. "
        f"Governor {', '.join(metadata['cpu_governors']) or 'не определён'}. "
        f"Python {metadata['python']}, LC_ALL={metadata['locale']}, TZ={metadata['timezone']}.",
        "",
    ]
    for name, shell in shells.items():
        lines.append(f"- {name} {shell['version']} · {shell['binary_bytes']} байт · SHA-256 `{shell['sha256']}`")
    lines += [
        "",
        "| Нагрузка, мс [p10–p90] | n | " + " | ".join(SHELLS) + " |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for key, engines in rows.items():
        cells = " | ".join(timing(row) for row in engines.values())
        lines.append(f"| {SUITES[key].name} | {len(engines['NSH']['samples'])} | {cells} |")
    lines += [
        "",
        "Время от запуска процесса до его завершения. Порядок оболочек циклически меняется. "
        "Перед прогретыми замерами выполнен один подготовительный запуск на оболочку и нагрузку. "
        "Каждый прогон проверяет код 0, точные байты stdout и пустой stderr. "
        f"Сырые выборки, хеши входов и настройки сохранены в [{stem}.json]({stem}.json).",
        "",
        "Оболочки работают в неинтерактивном режиме с одинаковыми временными HOME и XDG. "
        "Флаги запуска приведены в JSON. Ранний системный zshenv может читаться Zsh. "
        "Старт выполняет одну встроенную true. "
        "Все сценарии используют одинаковые команды. Fish использует собственный синтаксис функций, "
        "циклов и условного блока при том же числе операций и том же выводе. "
        "В тесте поиска PATH перед /usr/bin и /bin стоят две отсутствующие директории.",
        "",
        "Для холодного старта на каждом повторе создаётся отдельная копия ELF с исходным именем. "
        "После fsync страницы файла сбрасываются через POSIX_FADV_DONTNEED. "
        "Перед запуском отсутствие резидентных страниц проверяет mincore. "
        "Библиотеки, системные конфигурации, метаданные и кэши ядра могут оставаться прогретыми.",
        "",
    ]
    if metadata["strace"]:
        lines += [
            f"В отдельном проходе {metadata['strace']} собрал системные вызовы процессов. "
            "В число exec входит загрузка оболочки. Для встроенных сценариев проверен единственный exec.",
            "",
            "| Нагрузка | Оболочка | Fork/clone, успехи | Exec, попытки / ошибки | Wait, вызовы |",
            "| --- | --- | ---: | ---: | ---: |",
        ]
        for key, engines in rows.items():
            for name, row in engines.items():
                if "syscalls" not in row:
                    continue
                counts = row["syscalls"]
                created = ("fork", "vfork", "clone", "clone3")
                forks = count_syscalls(counts, created, "calls") - count_syscalls(counts, created, "errors")
                exec_calls = ("execve", "execveat")
                execs = count_syscalls(counts, exec_calls, "calls")
                errors = count_syscalls(counts, exec_calls, "errors")
                waits = count_syscalls(counts, ("wait4", "waitid"), "calls")
                lines.append(f"| {SUITES[key].name} | {name} | {forks} | {execs} / {errors} | {waits} |")
        lines.append("")
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--nsh", type=Path, required=True)
    parser.add_argument("--dash", type=Path, required=True)
    parser.add_argument("--dash-version", required=True, help="Dash has no --version option")
    for shell in ("zsh", "fish", "bash"):
        parser.add_argument(f"--{shell}", type=Path, default=Path(f"/usr/bin/{shell}"))
    parser.add_argument("--strace", type=Path,
                        default=STRACE if STRACE.is_file() else shutil.which("strace"))
    parser.add_argument("--skip-trace", action="store_true")
    parser.add_argument("--cpu", type=int, help="pin the harness and its children to this CPU")
    parser.add_argument("--samples", type=int, default=25)
    parser.add_argument("--cold-samples", type=int, default=20)
    parser.add_argument("--only", action="append", choices=SUITES, help="run selected workloads")
    parser.add_argument("--output-dir", type=Path, default=ROOT / "MANUALS/benchmarks")
    parser.add_argument("--report-stem", default="current")
    options = parser.parse_args()
    if min(options.samples, options.cold_samples) < 2:
        parser.error("collect at least two samples per workload")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+", options.report_stem):
        parser.error("report stem must be a filename stem")
    if options.cpu is not None:
        if options.cpu not in os.sched_getaffinity(0):
            parser.error(f"CPU {options.cpu} is outside the allowed affinity")
        os.sched_setaffinity(0, {options.cpu})
    tracer = None if options.skip_trace else options.strace
    if tracer is None and not options.skip_trace:
        parser.error("provide --strace or explicitly use --skip-trace")
    if tracer is not None:
        tracer = tracer.resolve()
    binaries = {name: getattr(options, name.lower()).resolve() for name in SHELLS}
    for name, binary in binaries.items():
        if not binary.is_file() or not os.access(binary, os.X_OK):
            parser.error(f"{name}: missing executable {binary}")
        with binary.open("rb") as data:
            if data.read(4) != b"\x7fELF":
                parser.error(f"{name}: expected a direct ELF executable ({binary})")
    keys = [key for key in SUITES if options.only is None or key in options.only]
    metadata = environment()
    scratch = ROOT / ".build"
    scratch.mkdir(exist_ok=True)
    try:
        with tempfile.TemporaryDirectory(prefix="current-bench-", dir=scratch) as temp:
            directory = Path(temp)
            (directory / "cold").mkdir()
            env = execution_environment(directory)
            if tracer:
                metadata["strace"] = subprocess.check_output(
                    [tracer, "--version"], env=env, text=True, timeout=10,
                ).splitlines()[0].rstrip(".")
            reports = {
                shell: describe(shell, path, options.dash_version, directory, env)
                for shell, path in binaries.items()
            }
            fixtures = write_fixtures(directory, keys)
            rows = collect(keys, binaries, tracer, directory, fixtures, env,
                           options.samples, options.cold_samples)
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as cause:
        parser.exit(1, f"benchmark failed: {cause}\n")
    hashes = {}
    validation = {}
    for key in keys:
        workload = SUITES[key]
        if workload.source is None:
            hashes[key] = {"shared_command": hashlib.sha256(b"true").hexdigest()}
        else:
            hashes[key] = {"shared": hashlib.sha256(workload.source).hexdigest()}
            if workload.fish_source is not None:
                hashes[key]["Fish"] = hashlib.sha256(workload.fish_source).hexdigest()
        validation[key] = {
            "status": 0,
            "stderr_bytes": 0,
            "stdout_bytes": len(workload.expected),
            "stdout_sha256": hashlib.sha256(workload.expected).hexdigest(),
        }
    output = {
        "environment": metadata, "shells": reports, "workloads": rows,
        "fixtures_sha256": hashes, "validation": validation,
    }
    options.output_dir.mkdir(parents=True, exist_ok=True)
    stem = options.report_stem
    (options.output_dir / f"{stem}.json").write_text(
        json.dumps(output, ensure_ascii=False, indent=2) + "\n", encoding="utf-8",
    )
    (options.output_dir / f"{stem}.md").write_text(
        summary(rows, metadata, reports, stem), encoding="utf-8",
    )


if __name__ == "__main__":
    main()
