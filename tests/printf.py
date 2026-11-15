"""Native printf tests, including POSIX Issue 8 and shell command search."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


NSH = str(Path(sys.argv.pop(1)).resolve(strict=True))


class PrintfTests(unittest.TestCase):
    def run_shell(self, script, *args, cwd=None, env=None):
        return subprocess.run([NSH, "--posix", "-c", script, "nsh", *args],
                              cwd=cwd, capture_output=True, timeout=6,
                              env={"PATH": "/usr/bin:/bin", "LC_ALL": "C", **(env or {})})

    def check_format(self, format_, args, output, status=0, diagnostic=False, env=None):
        result = self.run_shell('printf -- "$@"', format_, *args, env=env)
        self.assertEqual((result.returncode, result.stdout), (status, output), result.stderr)
        self.assertEqual(bool(result.stderr), diagnostic, result.stderr)

    def test_numbered_conversions_reuse_and_missing_sequential_operands(self):
        self.check_format("%2$s/%1$s%%\n", ("a", "b", "c", "d"), b"b/a%\nd/c%\n")
        self.check_format("%s:%s:%d:%c:%b\n", ("alpha",), b"alpha::0:\0:\n")
        self.check_format("%3$*2$.*1$s", ("2", "7", "abc"), b"     ab")

    def test_integer_flags_radices_and_width(self):
        self.check_format("%+#08x|%+06d|%-5.3d|%#.0o|%.0d|%u\n",
                          ("18", "-10", "017", "0", "0", "-1"),
                          b"0x000012|-00010|015  |0||18446744073709551615\n")
        self.check_format("%d|%X|%d", ("0x2a", "+052", "'A"), b"42|2A|65")
        self.check_format("%*.*s", ("-5", "2", "abc"), b"ab   ")

    def test_numeric_error_prints_partial_value_and_continues(self):
        self.check_format("%d/%d/%u", ("12bad", "-", "10"), b"12/0/10", 1, True)
        self.check_format("%d/%d", ("18446744073709551616", "-9223372036854775809"),
                          b"9223372036854775807/-9223372036854775808", 1, True)

    def test_escapes_precision_and_stop(self):
        self.check_format(r"\101\000\045\x42\n", (), b"A\0%B\n")
        self.check_format("%7.3b!", (r"\0101\000\aB",), b"    A\0\a!")
        self.check_format("%b AFTER\n", (r"a\tb\cNO", "unused"), b"a\tb")
        self.check_format("%b", (r"\U0001f600",), "😀".encode(),
                          env={"LC_ALL": "C.UTF-8"})
        self.check_format("%.1b next", (r"ab\c",), b"a")

    def test_bytes_in_string_and_character_conversion(self):
        self.check_format("%6.2s|%c", (b"a\xffz", b"\xe9xyz"), b"    a\xff|\xe9")

    def test_float_rendering_and_numeric_error(self):
        self.check_format("%+08.2f|%.2E|%#.4g|%a", ("1.25", "64", "2", "0.5"),
                          b"+0001.25|6.40E+01|2.000|0x1p-1")
        self.check_format("%.2f:%d", ("2.5oops", "14"), b"2.50:14", 1, True)
        self.check_format("%+#.1500f", ("1.5",), b"+1.5" + b"0" * 1499)

    def test_quote_extension_round_trips_non_utf8_bytes(self):
        value = b" a'\t\\\xff"
        result = self.run_shell(
            "quoted=$(printf %q \"$1\") || exit; "
            "eval \"set -- $quoted\"; printf %s \"$1\"", value
        )
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, value, b""))

    def test_errors_and_shell_context(self):
        self.check_format("printed%Z", (), b"printed", 1, True)
        result = self.run_shell(
            'tmp=parent; tmp=child printf "%s" "$tmp" >/dev/null; '
            'printf "%s" "$tmp"; printf "%s" closed >&- || echo "-write-failed"'
        )
        self.assertEqual((result.returncode, result.stdout), (0, b"parent-write-failed\n"))
        self.assertTrue(result.stderr)

    def test_path_order_temporary_assignments_and_exec(self):
        with tempfile.TemporaryDirectory() as work:
            custom = Path(work, "printf")
            custom.write_bytes(b'echo "custom:$1:$FLAG"\n')
            custom.chmod(0o755)
            result = self.run_shell(
                'PATH=:/usr/bin:/bin; FLAG=child printf first; '
                'PATH=/usr/bin:/bin printf "%4b\\n" X; '
                'command -p printf "%4b\\n" Y; '
                'printf() { echo function; }; printf ignore; '
                'command printf fourth; '
                './printf explicit; '
                '(exec ./printf direct)',
                cwd=work,
            )
            self.assertEqual((result.returncode, result.stdout, result.stderr),
                             (0, b"custom:first:child\n   X\n   Y\nfunction\n"
                              b"custom:fourth:\ncustom:explicit:\ncustom:direct:\n", b""))

    def test_unset_path_and_custom_path_without_system_directory(self):
        with tempfile.TemporaryDirectory() as work:
            custom = Path(work, "printf")
            custom.write_bytes(b'echo custom\n')
            custom.chmod(0o755)
            result = self.run_shell(
                'PATH=.; printf; unset PATH; printf "%4b" Y', cwd=work
            )
            self.assertEqual((result.returncode, result.stdout, result.stderr),
                             (0, b"custom\n   Y", b""))

    def test_failed_prior_candidate_allows_native_dispatch(self):
        with tempfile.TemporaryDirectory() as work:
            custom = Path(work, "printf")
            custom.write_bytes(b'echo inaccessible\n')
            result = self.run_shell('PATH=:/usr/bin; printf "%4b" Y', cwd=work)
            self.assertEqual((result.returncode, result.stdout, result.stderr), (0, b"   Y", b""))


if __name__ == "__main__":
    unittest.main()
