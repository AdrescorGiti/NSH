#!/usr/bin/env python3
"""Compare NSH and reference shells against POSIX expectations and identical fixtures."""

import argparse
import dataclasses
import hashlib
import os
from pathlib import Path
import platform
import shutil
import signal
import stat
import subprocess
import tempfile


PROJECT = Path(__file__).resolve().parent.parent
REFERENCES = PROJECT / ".build" / "references"
COMMAND_PATH = "/usr/bin:/bin"
TIMEOUT = 8


@dataclasses.dataclass(frozen=True)
class File:
    name: bytes
    content: bytes
    mode: int = 0o644


@dataclasses.dataclass(frozen=True)
class Expected:
    stdout: bytes = b""
    stderr: bytes | None = b""
    status: int | None = 0
    files: tuple[File, ...] = ()

    def compare(self, actual):
        problems = []
        if self.status is None:
            if actual.status <= 0:
                problems.append(f"expected a positive error status, got {actual.status}")
        elif actual.status != self.status:
            problems.append(f"status {actual.status} != {self.status}")
        if self.stdout != actual.stdout:
            problems.append(f"stdout {actual.stdout!r} != {self.stdout!r}")
        if self.stderr is None:
            if not actual.stderr:
                problems.append("expected a stderr diagnostic")
        elif actual.stderr != self.stderr:
            problems.append(f"stderr {actual.stderr!r} != {self.stderr!r}")
        if actual.files != self.files:
            problems.append(f"files {actual.files!r} != {self.files!r}")
        return problems


@dataclasses.dataclass(frozen=True)
class Actual:
    stdout: bytes
    stderr: bytes
    status: int
    files: tuple[File, ...]


@dataclasses.dataclass(frozen=True)
class Fixture:
    name: str
    section: str
    source: bytes
    expected: Expected
    args: tuple[bytes, ...] = ()
    files: tuple[File, ...] = ()
    stdin_script: bool = False
    syntax_only: bool = False
    reference_alternatives: tuple[tuple[str, Expected, str], ...] = ()
    locale: str = "C"


CASES = (
    Fixture(
        "field-split", "2.6.5",
        br"""unset IFS
value='  a  b '
set -- $value
printf '%s|%s|%s\n' "$#" "$1" "$2"
IFS=
set -- $value
printf '%s|%s\n' "$#" "$1"
IFS=:
value='a::b:'
set -- $value
printf '%s|<%s>|<%s>|<%s>\n' "$#" "$1" "$2" "$3"
""",
        Expected(b"2|a|b\n1|  a  b \n3|<a>|<>|<b>\n"),
    ),
    Fixture(
        "quoted-at", "2.5.2",
        br"""set --
probe() { printf '%s\n' "$#"; for value; do printf '<%s>\n' "$value"; done; }
probe "$@"
set -- "" 'two words' ""
probe "start$@end"
""",
        Expected(b"0\n3\n<start>\n<two words>\n<end>\n"),
    ),
    Fixture(
        "parameter-trimming", "2.6.2",
        br"""value=abcabc
printf '%s\n' "${value#a*c}" "${value##a*c}" "${value%b*c}" "${value%%b*c}" "${value#'a*c'}"
""",
        Expected(b"abc\n\nabca\na\nabcabc\n"),
    ),
    Fixture(
        "arithmetic-write-and-short-circuit", "2.6.4",
        br"""n=3
printf '%s:%s:%s\n' "$((n = (2 + 3)*4))" "$n" "$((0 && 1/0))"
""",
        Expected(b"20:20:0\n"),
    ),
    Fixture(
        "native-command-substitution", "2.6.3",
        br"""printf '<%s>\n' "$(case x in x) printf '%s\n\n' "$(printf '%s' 'a b')";; esac)" "`printf '%s' old`"
""",
        Expected(b"<a b>\n<old>\n"),
    ),
    Fixture(
        "redirection-order-and-restore", "2.7",
        br"""{ printf OUT; printf ERR >&2; } 2>&1 >result
{ printf A; printf B >&2; } >both 2>&1
printf PARENT
""",
        Expected(b"ERRPARENT", files=(File(b"both", b"AB"), File(b"result", b"OUT"))),
    ),
    Fixture(
        "partial-redirection-error", "2.7, 2.8.1",
        br"""printf WRONG >created 4<no-such-input
rc=$?
printf GOOD
exit "$rc"
""",
        Expected(b"GOOD", stderr=None, status=None, files=(File(b"created", b""),)),
    ),
    Fixture(
        "ordered-heredocs", "2.7.4",
        b"""value=VALUE
{ cat <&3; cat <&4; } 3<<ONE 4<<-'TWO' >result
$value
ONE
\t$value
\tTWO
""",
        Expected(files=(File(b"result", b"VALUE\n$value\n"),)),
    ),
    Fixture(
        "heredoc-strip-after-join", "2.7.4",
        b"word=VALUE\ncat <<-END\n\tprefix\\\n\t$word\n\tEND\n",
        Expected(b"prefix\tVALUE\n"),
    ),
    Fixture(
        "heredoc-literal-delimiter", "2.7.4",
        b"value=VALUE\ncat <<$(printf 'END')\n$value\n$(printf 'END')\n",
        Expected(b"VALUE\n"),
        reference_alternatives=(
            ("dash", Expected(b"$value\n$(printf 'END')\n"), "delimiter quote removal"),
            ("yash", Expected(stderr=None, status=2), "delimiter quote removal"),
        ),
    ),
    Fixture(
        "sequential-aliases", "2.3.1",
        b"""alias p='printf "%s\\n" '
alias x="'two words'"
p x
unalias p x
""",
        Expected(b"two words\n"),
    ),
    Fixture(
        "continued-operator", "2.2.1, 2.3",
        b"false |\\\n| printf continued\n",
        Expected(b"continued"),
    ),
    Fixture(
        "case-fallthrough", "2.9.4.3",
        b"case x in x) printf A ;& y) printf B ;; *) printf WRONG ;; esac\n",
        Expected(b"AB"),
        reference_alternatives=(
            ("dash", Expected(stderr=None, status=2), "Issue 8 ;& is rejected"),
        ),
    ),
    Fixture(
        "dollar-single-quotes", "2.2.4",
        br"""printf '%s' $'A\n\101\x42\cC\c\\'
""",
        Expected(b"A\nAB\x03\x1c"),
    ),
    Fixture(
        "pipefail-status", "2.9.2",
        br"""set -o pipefail
( exit 4 ) | ( exit 6 ) | :
printf '%s\n' "$?"
set +o pipefail
false | :
printf '%s\n' "$?"
""",
        Expected(b"6\n0\n"),
    ),
    Fixture(
        "stdin-read", "2.1, sh STDIN, read",
        br"""read -r value
literal\path
printf '%s:%s\n' "$1" "$value"
""",
        Expected(b"arg:literal\\path\n"),
        args=(b"arg",),
        stdin_script=True,
    ),
    Fixture(
        "syntax-only", "sh -n",
        b"f() { printf data; }\nf >unwanted\n",
        Expected(),
        syntax_only=True,
    ),
    Fixture(
        "pipeline-over-capacity", "2.9.2",
        b"head -c 262144 /dev/zero | wc -c\n",
        Expected(b"262144\n"),
    ),
    Fixture(
        "path-empty-component", "2.9.1.4, 2.9.1.6",
        br"""value=parent
export value
PATH=: value=child nsh-differential-local-28471
printf 'located:%s:%s\n' "$?" "$value"
PATH=/usr/bin nsh-differential-local-28471
printf 'missing:%s:%s\n' "$?" "$value"
""",
        Expected(
            b"located:0:parent\nmissing:127:parent\n",
            stderr=None,
            files=(File(b"nsh-differential-local-28471", b'case "$value" in child) exit 0;; *) exit 3;; esac\n', 0o755),),
        ),
        files=(File(b"nsh-differential-local-28471", b'case "$value" in child) exit 0;; *) exit 3;; esac\n', 0o755),),
    ),
    Fixture(
        "tilde-protected-fields", "2.6.1",
        br"""HOME='/nsh differential home'
set -- ~ "~" \~
paths=~/a:~/b
printf '%s|<%s>|<%s>|<%s>|<%s>\n' "$#" "$1" "$2" "$3" "$paths"
""",
        Expected(b"3|</nsh differential home>|<~>|<~>|</nsh differential home/a:/nsh differential home/b>\n"),
    ),
    Fixture(
        "mixed-ifs-delimiters", "2.6.5",
        br"""IFS=' :'
value='  : alpha ::  beta :  '
set -- $value
printf '%s|<%s>|<%s>|<%s>|<%s>\n' "$#" "$1" "$2" "$3" "$4"
""",
        Expected(b"4|<>|<alpha>|<>|<beta>\n"),
    ),
    Fixture(
        "empty-home-tilde", "2.6.1",
        br"""HOME=
set -- ~
printf '%s|<%s>\n' "$#" "$1"
""",
        Expected(b"1|<>\n"),
        reference_alternatives=(("dash", Expected(b"0|<>\n"), "empty HOME drops the tilde field"),),
    ),
    Fixture(
        "quoted-empty-field-placement", "2.6.5",
        br"""IFS=' :'
value=' a '
set -- ''$value""
printf '%s|<%s>|<%s>|<%s>\n' "$#" "$1" "$2" "$3"
unset variable
set -- ${variable:-a:""}
printf '%s|<%s>|<%s>\n' "$#" "$1" "$2"
""",
        Expected(b"3|<>|<a>|<>\n2|<a>|<>\n"),
    ),
    Fixture(
        "star-with-empty-ifs", "2.5.2",
        br"""probe() { printf '%s|<%s>|<%s>\n' "$#" "$1" "$2"; }
set -- a b
IFS=
probe $*
probe "$*"
unset IFS
probe "$*"
""",
        Expected(b"2|<a>|<b>\n1|<ab>|<>\n1|<a b>|<>\n"),
    ),
    Fixture(
        "quoted-parameter-operand", "2.2.3, 2.6.2",
        br"""unset variable
printf '<%s>\n' "${variable:-it's}" "${variable:-a\}b}" "${variable:-\~}" "${variable:-\$variable}"
""",
        Expected(b"<it's>\n<a}b>\n<\\~>\n<$variable>\n"),
    ),
    Fixture(
        "utf8-ifs-delimiter", "2.5.2, 2.6.5",
        r"""IFS=é
value='xêéy'
set -- $value
printf '%s|<%s>|<%s>\n' "$#" "$1" "$2"
set -- left right
printf '<%s>\n' "$*"
""".encode(),
        Expected("2|<xê>|<y>\n<leftéright>\n".encode()),
        locale="C.UTF-8",
    ),
    Fixture(
        "heredoc-quoted-parameter-operand", "2.2.3, 2.7.4",
        br"""unset variable
cat <<EOF
${variable:-"two words"}
${variable:-a\}b}
EOF
""",
        Expected(b"two words\na}b\n"),
    ),
    Fixture(
        "unset-vs-empty-parameter", "2.6.2",
        br"""unset variable
printf '<%s><%s><%s><%s>\n' "${variable-absent}" "${variable:-absent}" "${variable+set}" "${variable:+set}"
variable=
printf '<%s><%s><%s><%s>\n' "${variable-absent}" "${variable:-absent}" "${variable+set}" "${variable:+set}"
printf '<%s><%s>\n' "${variable:=fallback}" "$variable"
printf '<%s>\n' "${variable:-$(printf data >unwanted)}"
""",
        Expected(b"<absent><absent><><>\n<><absent><set><>\n<fallback><fallback>\n<fallback>\n"),
    ),
    Fixture(
        "option-nounset", "2.8.1, set -u",
        br"""set -u
unset unset_for_nsh_test
printf '%s' "$unset_for_nsh_test"
printf WRONG
""",
        Expected(stderr=None, status=None),
    ),
    Fixture(
        "option-noclobber", "2.7.2, set -C",
        br"""printf ORIGINAL >target
set -C
if printf WRONG >target; then printf WRONG; else printf protected; fi
cat target
printf REPLACED >|target
""",
        Expected(b"protectedORIGINAL", stderr=None, files=(File(b"target", b"REPLACED"),)),
    ),
    Fixture(
        "option-noglob", "2.6.6, set -f",
        br"""set -- mark-*
printf '<%s>' "$@"
set -f
set -- mark-*
printf '<%s>' "$@"
""",
        Expected(b"<mark-a><mark-b><mark-*>", files=(File(b"mark-a", b""), File(b"mark-b", b""))),
        files=(File(b"mark-a", b""), File(b"mark-b", b"")),
    ),
    Fixture(
        "option-allexport", "2.5.3, set -a",
        br"""set -a
nsh_exported=present
set +a
nsh_private=local
printenv nsh_exported
if printenv nsh_private; then printf WRONG; else printf private; fi
""",
        Expected(b"present\nprivate"),
    ),
    Fixture(
        "temporary-environment", "2.9.1",
        br"""value=parent
export value
value=child printenv value
printf '%s\n' "$value"
""",
        Expected(b"child\nparent\n"),
    ),
    Fixture(
        "errexit-conditional-function", "2.8.1, set -e",
        b"""set -e
false && printf BAD
f() { false; printf F; }
if f; then printf T; fi
false
printf BAD
""",
        Expected(b"FT", status=1),
    ),
    Fixture(
        "enoexec-text", "2.9.1.6",
        b"./no-shebang 'two words'\n",
        Expected(
            b"./no-shebang|two words\n",
            files=(File(b"no-shebang", br"""printf '%s|%s\n' "$0" "$1"
""", 0o755),),
        ),
        files=(File(b"no-shebang", br"""printf '%s|%s\n' "$0" "$1"
""", 0o755),),
    ),
    Fixture(
        "non-utf8-argument-and-path", "2.6, 2.7",
        b'printf "%s" "$1" >"$1"\n',
        Expected(files=(File(b"raw-\xff", b"raw-\xff"),)),
        args=(b"raw-\xff",),
        reference_alternatives=(
            ("yash", Expected(stderr=None, status=2), "argument rejected in the C locale"),
        ),
    ),
    Fixture(
        "missing-program", "2.8.2, 2.9.1.6",
        b"nsh-differential-no-such-command-28471\n",
        Expected(stderr=None, status=127),
    ),
    Fixture(
        "unexecutable-program", "2.8.2, 2.9.1.6",
        b"./no-execute\n",
        Expected(stderr=None, status=126, files=(File(b"no-execute", b"exit 14\n"),)),
        files=(File(b"no-execute", b"exit 14\n"),),
    ),
    Fixture(
        "exec-error-exits", "2.8.1, exec",
        b"exec nsh-differential-no-such-command-28471\nprintf BAD\n",
        Expected(stderr=None, status=127),
    ),
    Fixture(
        "wait-child-status", "2.9.3.1, wait",
        br"""( exit 7 ) &
wait "$!"
printf '%s\n' "$?"
""",
        Expected(b"7\n"),
    ),
    Fixture(
        "trap-exit-status", "2.14, trap",
        br"""trap 'printf "exit=%s\n" "$?"' EXIT
exit 23
""",
        Expected(b"exit=23\n", status=23),
    ),
    Fixture(
        "trap-delivery", "2.11, trap",
        b"trap 'printf caught' USR1\nkill -USR1 $$\nprintf done\n",
        Expected(b"caughtdone"),
    ),
)


def fixture_environment(home, locale):
    return {
        "PATH": COMMAND_PATH,
        "HOME": os.fspath(home),
        "LC_ALL": locale,
        "TERM": "dumb",
        "TZ": "UTC",
    }


def executable(value, short_name, isolated=None):
    if value is not None:
        path = Path(value).resolve(strict=True)
    elif isolated is not None and isolated.is_file():
        path = isolated.resolve(strict=True)
    else:
        installed = shutil.which(short_name, path=COMMAND_PATH)
        if installed is None:
            raise ValueError(f"{short_name} is unavailable")
        path = Path(installed).resolve(strict=True)
    if not path.is_file() or not os.access(path, os.X_OK):
        raise ValueError(f"not executable: {path}")
    return path


def query(command):
    try:
        result = subprocess.run(command, check=True, capture_output=True, timeout=TIMEOUT)
        lines = result.stdout.decode("utf-8", errors="replace").splitlines()
        return lines[0] if lines else None
    except (OSError, subprocess.SubprocessError):
        return None


def reference_version(name, path, dash_version):
    if name == "dash":
        if dash_version:
            return dash_version
        for parent in path.parents:
            metadata = parent / ".PKGINFO"
            if metadata.is_file():
                for line in metadata.read_text().splitlines():
                    if line.startswith("pkgver = "):
                        return line.removeprefix("pkgver = ")
        return (
            query(["pacman", "-Q", "dash"])
            or query(["dpkg-query", "-W", "dash"])
            or "unavailable (provide --dash-version for an unpackaged binary)"
        )
    version = query([path, "--version"]) or "unavailable"
    if name == "yash" and (path.parent / ".git").exists():
        revision = query(["git", "-C", path.parent, "rev-parse", "HEAD"])
        if revision:
            version += f" (commit {revision})"
    return version


def snapshot(work):
    output = []
    for root, directories, files in os.walk(os.fsencode(work)):
        for name in directories + files:
            path = os.path.join(root, name)
            relative = os.path.relpath(path, os.fsencode(work))
            mode = os.lstat(path).st_mode
            content = b""
            if stat.S_ISLNK(mode):
                content = os.readlink(path)
            elif stat.S_ISREG(mode):
                with open(path, "rb") as stream:
                    content = stream.read()
            output.append(File(relative, content, stat.S_IMODE(mode)))
    return tuple(sorted(output, key=lambda entry: entry.name))


def run(shell, options, fixture):
    with tempfile.TemporaryDirectory(prefix="nsh-differential-") as sandbox:
        work = Path(sandbox) / "work"
        home = Path(sandbox) / "home"
        work.mkdir()
        home.mkdir()
        for file in fixture.files:
            with open(os.path.join(os.fsencode(work), file.name), "wb") as stream:
                stream.write(file.content)
                os.fchmod(stream.fileno(), file.mode)

        command = [os.fsencode(shell), *options]
        stdin = b""
        if fixture.syntax_only:
            command.append(b"-n")
        if fixture.stdin_script:
            command.extend([b"-s", *fixture.args])
            stdin = fixture.source
        else:
            command.extend([b"-c", fixture.source, b"differential-fixture", *fixture.args])

        with subprocess.Popen(
            command,
            cwd=work,
            env=fixture_environment(home, fixture.locale),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
            umask=0o022,
        ) as process:
            try:
                stdout, stderr = process.communicate(stdin, timeout=TIMEOUT)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.communicate(timeout=2)
                raise
            return Actual(stdout, stderr, process.returncode, snapshot(work))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("nsh", type=Path, nargs="?")
    parser.add_argument("--references-only", action="store_true")
    parser.add_argument("--dash")
    parser.add_argument("--dash-version")
    parser.add_argument("--bash")
    parser.add_argument("--yash")
    parser.add_argument("--only", action="append", choices=[fixture.name for fixture in CASES])
    args = parser.parse_args()
    if args.nsh is None and not args.references_only:
        parser.error("provide the NSH executable path or --references-only")

    try:
        shells = {}
        if not args.references_only:
            shells["nsh"] = (executable(args.nsh, "nsh"), [b"--posix"])
        shells["dash"] = (
            executable(args.dash, "dash", REFERENCES / "dash" / "usr" / "bin" / "dash"),
            [],
        )
        shells["bash"] = (executable(args.bash, "bash"), [b"--posix"])
        shells["yash"] = (
            executable(args.yash, "yash", REFERENCES / "yash" / "source" / "yash"),
            [b"--posix"],
        )
    except (OSError, ValueError) as error:
        parser.error(str(error))
    print(f"System {platform.system()} {platform.release()} {platform.machine()}, TZ=UTC, LC_ALL=C unless shown per fixture")
    for name, (path, _) in shells.items():
        version = reference_version(name, path, args.dash_version)
        with path.open("rb") as binary:
            checksum = hashlib.file_digest(binary, "sha256").hexdigest()
        print(f"{name} {version} at {path}, SHA-256 {checksum}")
    print("Recorded reference differences are compared against their expected observations.")
    print("A required error diagnostic is compared for presence unless exact bytes are specified.")

    problems = 0
    reference_differences = 0
    count = 0
    skipped = 0
    local_versions = {}
    for fixture in CASES:
        if args.only and fixture.name not in args.only:
            continue
        count += 1
        print(f"{fixture.name} [POSIX.1-2024 {fixture.section}, {fixture.locale}]")
        if fixture.locale != "C":
            if fixture.locale not in local_versions:
                probe = subprocess.run(
                    ["locale", "charmap"], env={"PATH": COMMAND_PATH, "LC_ALL": fixture.locale},
                    check=False, capture_output=True, timeout=TIMEOUT,
                )
                local_versions[fixture.locale] = probe.returncode == 0 and probe.stdout == b"UTF-8\n"
            if not local_versions[fixture.locale]:
                skipped += 1
                print(f"  SKIP required locale {fixture.locale} is unavailable")
                continue
        for name, (path, options) in shells.items():
            try:
                actual = run(path, options, fixture)
                errors = fixture.expected.compare(actual)
            except (OSError, subprocess.TimeoutExpired) as error:
                errors = [str(error)]
                problems += 1
                print(f"  {name}: ERROR {'; '.join(errors)}")
                continue
            if not errors:
                print(f"  {name}: PASS")
                continue
            alternative = next(
                ((expected, reason) for reference, expected, reason in fixture.reference_alternatives
                 if name == reference), None
            )
            if alternative and not alternative[0].compare(actual):
                reference_differences += 1
                disposition = f"REFERENCE DIFFERENCE ({alternative[1]})"
            else:
                problems += 1
                disposition = "FAIL"
            print(f"  {name}: {disposition} {'; '.join(errors)}")
            print(f"    actual {actual!r}")

    print(
        f"{count} fixtures, {problems} failures, {reference_differences} reference differences, "
        f"{skipped} skipped"
    )
    return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main())
