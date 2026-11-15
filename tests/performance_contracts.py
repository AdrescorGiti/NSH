"""Regression checks for builtin selection and direct execution in job workers."""
import os
from pathlib import Path
import select
import subprocess
import sys
import tempfile
import unittest

BIN = str(Path(sys.argv.pop(1)).resolve())
ENV = {b"PATH": b"/usr/bin:/bin", b"LC_ALL": b"C"}


class Contracts(unittest.TestCase):
    def run_shell(self, source, *args, cwd=None, pass_fds=()):
        return subprocess.run([BIN, "--vanilla", "-c", source, "nsh", *args], env=ENV,
                              cwd=cwd, pass_fds=pass_fds, capture_output=True, timeout=5)

    def test_pipeline_stages_are_direct_children(self):
        result = self.run_shell(
            """parent=$$
            /usr/bin/python3 -c 'import os,sys; print(os.getppid()==int(sys.argv[1]))' "$parent" | /usr/bin/cat
            """)
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, b"True\n", b""))

    def test_background_simple_command_reports_its_pid(self):
        result = self.run_shell(
            """/usr/bin/python3 -c 'import os; print(os.getpid())' > actual &
            child=$!
            wait "$child"
            read actual < actual
            echo "$([ "$child" = "$actual" ] && echo same)"
            """, cwd=self.enterContext(tempfile.TemporaryDirectory()))
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, b"same\n", b""))

    def test_truth_comparisons_preserve_decimal_and_precedence(self):
        result = self.run_shell(
            """[ ' +00012 ' -eq 12 ] &&
               test -99999999999999999999999999 -lt -99999999999999999999999998 &&
               test '!' = '!' && [ ! -z x ] && [ ! a = b ] &&
               test a '<' b && echo truth
               test not-a-number -lt 5; echo invalid=$?
            """)
        self.assertEqual((result.returncode, result.stdout), (0, b"truth\ninvalid=2\n"))
        self.assertTrue(result.stderr)

    def test_file_primaries_follow_links_except_link_tests(self):
        with tempfile.TemporaryDirectory() as directory:
            cwd = os.fsencode(directory)
            target = cwd + b"/file-\xff"
            with open(target, "wb") as file:
                file.write(b"payload")
            os.link(target, cwd + b"/same")
            os.symlink(b"missing", cwd + b"/dangling")
            os.mkfifo(cwd + b"/fifo")
            result = self.run_shell(
                b'test -f "$1" && [ "$1" -ef same ] && test -L dangling && '
                b'! [ -e dangling ] && [ -p fifo ] && [ -s same ] && echo files',
                b"file-\xff", cwd=cwd)
            self.assertEqual((result.returncode, result.stdout, result.stderr), (0, b"files\n", b""))

    def test_test_prefers_earlier_path_over_builtin(self):
        with tempfile.TemporaryDirectory() as directory:
            custom = Path(directory, "test")
            custom.write_text("echo override\n")
            custom.chmod(0o700)
            result = self.run_shell('PATH="$1:/usr/bin:/bin" test ""; test ""; echo status=$?', directory)
            self.assertEqual((result.returncode, result.stdout, result.stderr), (0, b"override\nstatus=1\n", b""))

    def test_large_multiline_file_preserves_incremental_execution(self):
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory, "large.sh")
            script.write_bytes(
                b"alias step='printf \"first:%s\\n\"'\n"
                b"if :; then\n"
                + b"    : padding words\n" * 6_000
                + br"""    step "$(read -r line; printf '%s' "$line")"
fi
alias step='printf "second:%s\n"'
step ready
IFS= read -r line <<'END'
here document
END
"""
                + b"step \"$line\""
            )
            result = subprocess.run(
                [BIN, "--vanilla", script], env=ENV, input=b"external stdin\n",
                capture_output=True, timeout=5,
            )
            self.assertEqual(
                (result.returncode, result.stdout, result.stderr),
                (0, b"first:external stdin\nsecond:ready\nsecond:here document\n", b""),
            )

    def test_script_fifo_dispatches_before_writer_closes(self):
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory, "script")
            os.mkfifo(script)
            with subprocess.Popen(
                [BIN, "--vanilla", script], env=ENV,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            ) as shell:
                try:
                    # A read/write descriptor allows this open to finish even if the
                    # shell fails before opening the FIFO as a reader.
                    with os.fdopen(os.open(script, os.O_RDWR), "wb", buffering=0) as writer:
                        writer.write(b"echo first\n")
                        self.assertTrue(select.select([shell.stdout], [], [], 5)[0])
                        self.assertEqual(shell.stdout.readline(), b"first\n")
                        writer.write(b"echo last\n")
                    stdout, stderr = shell.communicate(timeout=5)
                    self.assertEqual((shell.returncode, stdout, stderr), (0, b"last\n", b""))
                finally:
                    if shell.poll() is None:
                        shell.kill()


if __name__ == "__main__":
    unittest.main()
