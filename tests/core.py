"""CLI contracts. Run: python3 tests/core.py path/to/nsh"""
import os, pathlib, subprocess, sys, tempfile, unittest
BIN = os.path.abspath(sys.argv.pop(1))
BASE = {b"PATH": b"/usr/bin:/bin", b"LC_ALL": b"C"}

class Shell(unittest.TestCase):
    def run_shell(self, code, *args, stdin=b"", status=0, output=None, cwd=None):
        args = [BIN, "--vanilla", "-c", code, *args]
        p = subprocess.run(args, input=stdin, capture_output=True, env=BASE,
                           cwd=cwd, timeout=8)
        self.assertEqual(p.returncode, status, p.stderr)
        if output is not None:
            self.assertEqual(p.stdout, output, p.stderr)
        return p

    def test_arguments(self):
        self.run_shell('f() { printf "<%s>\n" "$0" "$#" "$@"; }; f "$@"',
                       "arg0", "", "b c", output=b"<arg0>\n<2>\n<>\n<b c>\n")

    def test_logic_and_loops(self):
        self.run_shell('for x in a b; do if false || true; then echo "$x"; fi; done; x=0; until [ "$x" -eq 2 ]; do x=$((x+1)); done; echo "$x"',
                       output=b"a\nb\n2\n")

    def test_arithmetic_and_parameters(self):
        self.run_shell('unset a; a=${a-2}; b=; echo "${b-x}${b:-x} $((2 + 3*4)) ${a:+yes}"; echo "$((1 ? 7 : 1/0))"',
                       output=b"x 14 yes\n7\n")

    def test_issue8_case_dollar_quotes_pipefail(self):
        self.run_shell("set -o pipefail; false | true; echo $?; case x in x) printf '%s' $'a\\n' ;& y) echo b ;; esac",
                       output=b"1\na\nb\n")

    def test_substitution(self):
        self.run_shell('echo "$(echo "$(printf hi)")" "`printf old`"; x=old; (x=new); echo "$x"',
                       output=b"hi old\nold\n")

    def test_here_and_redirections(self):
        with tempfile.TemporaryDirectory() as cwd:
            self.run_shell("a=ok; cat 3<<ONE 2>err <&3\n$a\nONE\n{ echo out; echo err >&2; } 2>&1 >out; cat out\n",
                           cwd=cwd, output=b"ok\nerr\nout\n")

    def test_big_pipeline(self):
        self.run_shell("head -c 1048576 /dev/zero | wc -c", output=b"1048576\n")

    def test_pipeline_with_stdin_closed_by_exec(self):
        p = subprocess.run([BIN, "-c", "exec 0<&-; echo payload | cat"],
                           stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=BASE, timeout=5)
        self.assertEqual((p.returncode, p.stdout, p.stderr), (0, b"payload\n", b""))

    def test_missing_and_permission(self):
        self.run_shell("does-not-exist", status=127)
        with tempfile.TemporaryDirectory() as cwd:
            pathlib.Path(cwd, "noaccess").touch()
            self.run_shell("./noaccess", cwd=cwd, status=126)

    def test_enoexec_and_non_utf8(self):
        with tempfile.TemporaryDirectory() as cwd:
            name = os.fsencode(cwd) + b"/\xff"
            fd = os.open(name, os.O_CREAT | os.O_WRONLY, 0o700)
            with os.fdopen(fd, "wb") as f:
                f.write(b'printf "%s %s\\n" "$0" "$1"\n')
            self.run_shell(b'"$1" arg', "nsh", b"./\xff", cwd=cwd, output=b"./\xff arg\n")

    def test_script_stdin_read(self):
        p = subprocess.run([BIN, "-s", "first"], input=b'read data\nPAID LOAD\nprintf "%s|%s\\n" "$1" "$data"\n',
                           capture_output=True, env=BASE, timeout=5)
        self.assertEqual((p.returncode, p.stdout), (0, b"first|PAID LOAD\n"), p.stderr)

    def test_readonly_and_errexit(self):
        self.run_shell("set -e; false && echo no; if false; then echo no; fi; false; echo no", status=1, output=b"")
        self.run_shell("readonly a=1; a=2; echo no", status=1, output=b"")

    def test_trap_wait_and_alias(self):
        self.run_shell("alias a='echo yes'\na\ntrap 'echo event' USR1; kill -USR1 $$; sleep 0.01 & wait $!; echo $?",
                       output=b"yes\nevent\n0\n")

    def test_wait_returns_early_for_trapped_signal(self):
        self.run_shell("trap 'echo trapped' USR1; (sleep .05; kill -USR1 $$) & "
                       "sleep .2 & p=$!; wait \"$p\"; echo $?; wait \"$p\"; echo $?",
                       output=b"trapped\n138\n0\n")

    def test_errexit_compound_exemptions(self):
        self.run_shell("set -e; { false && :; }; echo survived; (false && :); echo no",
                       status=1, output=b"survived\n")

    def test_cd_logical_still_checks_removed_component(self):
        with tempfile.TemporaryDirectory() as cwd:
            self.run_shell("cd nonexistent/..; echo $?", cwd=cwd, output=b"1\n")

    def test_declaration_assignment_and_command_options(self):
        p = subprocess.run([BIN, "-c", "-u", 'x="a b"; export X=$x; env | grep "^X="; echo "$0 $1"',
                            "--", "tail"], capture_output=True, env=BASE, timeout=5)
        self.assertEqual((p.returncode, p.stdout), (0, b"X=a b\n-- tail\n"), p.stderr)

    def test_posix_env_and_vanilla_are_independent(self):
        with tempfile.TemporaryDirectory() as directory:
            config = pathlib.Path(directory, "env.sh")
            config.write_text("ENV_DEFINED=present\n")
            env = {**BASE, b"HOME": os.fsencode(directory), b"ENV": b"$HOME/env.sh"}
            command = 'echo "${ENV_DEFINED-unset}"'
            for flags, expected in (
                (["--posix", "-i"], b"present\n"),
                (["--posix"], b"unset\n"),
                (["--vanilla", "--posix", "-i"], b"unset\n"),
            ):
                with self.subTest(flags=flags):
                    p = subprocess.run([BIN, *flags, "-c", command], env=env, capture_output=True, timeout=5)
                    self.assertEqual((p.returncode, p.stdout, p.stderr), (0, expected, b""))
            config.chmod(0o666)
            p = subprocess.run([BIN, "--posix", "-i", "-c", command], env=env, capture_output=True, timeout=5)
            self.assertEqual((p.returncode, p.stdout), (0, b"unset\n"))
            self.assertIn(b"untrusted startup file", p.stderr)

    def test_noexec_and_script_operands(self):
        with tempfile.TemporaryDirectory() as cwd:
            file = pathlib.Path(cwd, "--script")
            file.write_text("echo runtime > marker\n")
            p = subprocess.run([BIN, "-n", "--", file.name], env=BASE, cwd=cwd, capture_output=True, timeout=5)
            self.assertEqual((p.returncode, p.stdout, p.stderr), (0, b"", b""))
            self.assertFalse(pathlib.Path(cwd, "marker").exists())
            file.write_text('printf "%s|%s\\n" "$0" "$1"\n')
            p = subprocess.run([BIN, "--", file.name, "tail"], env=BASE, cwd=cwd, capture_output=True, timeout=5)
            self.assertEqual((p.returncode, p.stdout, p.stderr), (0, b"--script|tail\n", b""))

    def test_intrinsic_resources_and_signals_ignore_path(self):
        with tempfile.TemporaryDirectory() as cwd:
            self.run_shell("PATH=/unavailable; umask 027; umask -S; umask g+w,o-rwx; "
                           "umask; : > mode; (ulimit -S -n 64; ulimit -n); kill -l 143",
                           cwd=cwd, output=b"u=rwx,g=rx,o=\n0007\n64\nTERM\n")
            self.assertEqual(pathlib.Path(cwd, "mode").stat().st_mode & 0o777, 0o660)

    def test_getopts(self):
        self.run_shell('set -- -abvalue tail; while getopts ab: name; do echo "$name:${OPTARG-}"; done; shift "$((OPTIND - 1))"; echo "$1"',
                       output=b"a:\nb:value\ntail\n")

if __name__ == "__main__":
    unittest.main()
