#!/usr/bin/env python3
"""Exercise executable resolution, startup rejection, and color on controlling PTYs."""
import errno
import fcntl
import os
from pathlib import Path
import pty
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unittest


BIN = Path(sys.argv.pop(1) if len(sys.argv) > 1 else "target/release/nsh").resolve()
FISH = Path("/usr/bin/fish")
BASH = Path("/usr/bin/bash")
POSIX_SH = Path("/bin/sh")
ROOT_PROMPT = b"# " if os.getuid() == 0 else b"$ "
NSH_PROMPT = "❯".encode()


class Terminal:
    def __init__(self, argv, env, cwd):
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            try:
                os.chdir(cwd)
                os.execve(argv[0], argv, env)
            finally:
                os._exit(126)
        self.data = bytearray()
        self.position = 0
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))

    def read_until(self, needle):
        stop = time.monotonic() + 4
        while True:
            end = self.data.find(needle, self.position)
            if end != -1:
                result = bytes(self.data[self.position:end + len(needle)])
                self.position = end + len(needle)
                return result
            remaining = stop - time.monotonic()
            if remaining <= 0 or not select.select([self.fd], [], [], remaining)[0]:
                raise AssertionError(f"missing {needle!r} in terminal output {self.data!r}")
            try:
                chunk = os.read(self.fd, 65_536)
            except OSError as error:
                if error.errno != errno.EIO:
                    raise
                chunk = b""
            if not chunk:
                raise AssertionError(f"terminal closed before {needle!r}: {self.data!r}")
            self.data.extend(chunk)

    def send(self, data):
        os.write(self.fd, data)

    def close(self):
        os.close(self.fd)
        until = time.monotonic() + 0.25
        while time.monotonic() < until:
            child, _ = os.waitpid(self.pid, os.WNOHANG)
            if child:
                return
            time.sleep(.01)
        try:
            os.killpg(self.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        os.waitpid(self.pid, 0)


class StartupTests(unittest.TestCase):
    def setUp(self):
        self.home = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="nsh-startup-")))
        self.config_dir = self.home / "config/nsh"
        self.config_dir.mkdir(parents=True)
        self.config = self.config_dir / "conf.sh"
        self.bin_dir = self.home / "bin"
        self.bin_dir.mkdir()
        (self.bin_dir / "nsh").symlink_to(BIN)
        self.env = {
            "HOME": str(self.home),
            "PATH": str(self.bin_dir) + ":/usr/bin:/bin",
            "LC_ALL": "C.UTF-8",
            "TERM": "xterm-256color",
            "XDG_CONFIG_HOME": str(self.home / "config"),
            "XDG_STATE_HOME": str(self.home / "state"),
        }

    def terminal(self, *args, launcher=None, env=None):
        if launcher is None:
            argv = [str(BIN), *args]
        elif launcher == FISH:
            argv = [str(launcher), "--no-config", "-c", "command -s nsh; exec nsh $argv", *args]
        else:
            flags = ["--noprofile", "--norc"] if launcher == BASH else []
            argv = [str(launcher), *flags, "-c", 'command -v nsh; exec nsh "$@"', "startup", *args]
        terminal = Terminal(argv, self.env if env is None else env, self.home)
        self.addCleanup(terminal.close)
        return terminal

    def assert_nsh_interactive(self, terminal):
        self.assertEqual(Path(f"/proc/{terminal.pid}/exe").resolve(), BIN)
        children = Path(f"/proc/{terminal.pid}/task/{terminal.pid}/children")
        self.assertEqual(children.read_text().split(), [])
        terminal.send(b'echo NSH_INTERACTIVE_PROBE\r')
        terminal.read_until(b"\x1b[?2004lNSH_INTERACTIVE_PROBE\r\n")

    @unittest.skipUnless(FISH.is_file(), "fish is unavailable")
    def test_clean_home_resolves_and_launches_from_fish(self):
        terminal = self.terminal(launcher=FISH)
        terminal.read_until(os.fsencode(self.bin_dir / "nsh") + b"\r\n")
        terminal.read_until(NSH_PROMPT)
        self.assert_nsh_interactive(terminal)

    @unittest.skipUnless(BASH.is_file(), "bash is unavailable")
    def test_clean_home_resolves_and_launches_from_bash(self):
        terminal = self.terminal(launcher=BASH)
        terminal.read_until(os.fsencode(self.bin_dir / "nsh") + b"\r\n")
        terminal.read_until(NSH_PROMPT)
        self.assert_nsh_interactive(terminal)

    def test_clean_home_resolves_and_launches_from_posix_sh(self):
        terminal = self.terminal(launcher=POSIX_SH)
        terminal.read_until(os.fsencode(self.bin_dir / "nsh") + b"\r\n")
        terminal.read_until(NSH_PROMPT)
        self.assert_nsh_interactive(terminal)

    def test_markdown_fence_is_rejected_before_startup_execution(self):
        side_effect = self.home / "unexpected"
        self.config.write_bytes(
            b"echo created > '" + os.fsencode(side_effect) + b"'\n"
            b"\x60\x60\x60sh\n"
            b"nsh set prompt.symbol FENCE\n"
            b"\x60\x60\x60\n"
        )
        for launcher in (None, FISH, BASH, POSIX_SH):
            if launcher is not None and not launcher.is_file():
                continue
            with self.subTest(launcher=launcher):
                terminal = self.terminal(launcher=launcher)
                output = terminal.read_until(NSH_PROMPT)
                self.assertIn(os.fsencode(self.config) + b":2:", output)
                self.assertIn(b"\x60\x60\x60sh", output)
                self.assertRegex(output.lower(), rb"markdown|fence")
                self.assertNotIn(b"sh-5.3", output)
                self.assertFalse(side_effect.exists())
                self.assert_nsh_interactive(terminal)

    def test_nested_source_syntax_error_leaves_nsh_prompt(self):
        extra = self.home / "extra.sh"
        extra.write_text("# first line\n) unexpected\n")
        self.config.write_text(
            "nsh set prompt.symbol.success NOT_COMMITTED\n"
            '. "$HOME/extra.sh"\n'
        )
        terminal = self.terminal()
        output = terminal.read_until(NSH_PROMPT)
        self.assertIn(os.fsencode(extra) + b":2:", output)
        self.assertIn(b") unexpected", output)
        self.assertNotIn(b"NOT_COMMITTED ", output)
        self.assert_nsh_interactive(terminal)

    def test_child_shell_exit_leaves_nsh_prompt(self):
        self.config.write_text(
            "nsh set prompt.symbol.success NOT_COMMITTED\n"
            "/bin/sh -c 'exit 47'\n"
        )
        terminal = self.terminal()
        output = terminal.read_until(NSH_PROMPT)
        self.assertNotIn(b"NOT_COMMITTED ", output)
        self.assert_nsh_interactive(terminal)


    def test_runtime_failure_stops_config_and_rolls_back(self):
        self.config.write_text(
            "nsh set history.max_size 17\n"
            "Hello\n"
            ": > unwanted\n"
        )
        terminal = self.terminal()
        output = terminal.read_until(NSH_PROMPT)
        self.assertIn(os.fsencode(self.config) + b":2:", output)
        self.assertIn(b"Hello", output)
        self.assertFalse((self.home / "unwanted").exists())
        terminal.send(b"nsh get history.max_size\r")
        terminal.read_until(b"\x1b[?2004l2000\r\n")

    def test_startup_exec_cannot_replace_the_shell(self):
        self.config.write_text("nsh set prompt.symbol.success NOT_COMMITTED\nexec /bin/sh\n")
        terminal = self.terminal()
        output = terminal.read_until(NSH_PROMPT)
        self.assertIn(os.fsencode(self.config) + b":2:", output)
        self.assertIn(b"exec /bin/sh", output)
        self.assertNotIn(b"sh-5.3", output)
        self.assertNotIn(b"NOT_COMMITTED ", output)
        self.assert_nsh_interactive(terminal)

    def test_startup_module_exec_cannot_replace_the_shell(self):
        module = self.config_dir / "modules" / "replace.sh"
        module.parent.mkdir()
        module.write_text("exec /bin/sh\n")
        self.config.write_text("nsh module enable replace\n")
        terminal = self.terminal()
        output = terminal.read_until(NSH_PROMPT)
        self.assertIn(b"exec /bin/sh", output)
        self.assertNotIn(b"sh-5.3", output)
        self.assert_nsh_interactive(terminal)

    def test_script_exec_still_replaces_process(self):
        result = subprocess.run(
            [BIN, "-c", "exec /bin/sh -c 'echo EXEC_CHILD; exit 43'"],
            env=self.env, cwd=self.home, stdin=subprocess.DEVNULL,
            capture_output=True, timeout=5,
        )
        self.assertEqual((result.returncode, result.stdout, result.stderr), (43, b"EXEC_CHILD\n", b""))

    def test_startup_stdin_is_null_and_explicit_file_input_still_works(self):
        (self.home / "prompt-symbol").write_text("FILE_INPUT\n")
        self.config.write_text(
            "/bin/sh -c 'if [ -t 0 ]; then echo TTY; "
            "elif ! IFS= read -r value; then echo EOF; else echo DATA; fi' > stdin-observed\n"
            "IFS= read -r startup_input < prompt-symbol\n"
            "nsh set color.mode never\n"
            'nsh set prompt.symbol.success "$startup_input"\n'
        )
        terminal = self.terminal()
        terminal.read_until(b"FILE_INPUT ")
        self.assertEqual((self.home / "stdin-observed").read_bytes(), b"EOF\n")
        self.assert_nsh_interactive(terminal)

    def test_config_check_rejects_markdown_fence_without_executing(self):
        self.config.write_bytes(b"\x60\x60\x60sh\n: > unwanted\n\x60\x60\x60\n")
        result = subprocess.run(
            [BIN, "config", "check", self.config],
            env=self.env, cwd=self.home, stdin=subprocess.DEVNULL,
            capture_output=True, timeout=5,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(os.fsencode(self.config) + b":1:", result.stderr)
        self.assertIn(b"\x60\x60\x60sh", result.stderr)
        self.assertRegex(result.stderr.lower(), rb"markdown|fence")
        self.assertFalse((self.home / "unwanted").exists())

    def test_quoted_heredoc_can_contain_markdown_fences(self):
        self.config.write_bytes(
            b"cat > literal <<'END'\n"
            b"\x60\x60\x60sh\n"
            b"inside a quoted heredoc\n"
            b"\x60\x60\x60\n"
            b"END\n"
            b"nsh set color.mode never\n"
            b"nsh set prompt.symbol.success HEREDOC_READY\n"
        )
        terminal = self.terminal()
        terminal.read_until(b"HEREDOC_READY ")
        self.assertEqual(
            (self.home / "literal").read_bytes(),
            b"\x60\x60\x60sh\ninside a quoted heredoc\n\x60\x60\x60\n",
        )
        self.assert_nsh_interactive(terminal)

    def test_vanilla_skips_malformed_config(self):
        self.config.write_bytes(b"\x60\x60\x60sh\n: > unwanted\n\x60\x60\x60\n")
        terminal = self.terminal("--vanilla")
        self.assertNotIn(b"fence", terminal.read_until(ROOT_PROMPT).lower())
        self.assertFalse((self.home / "unwanted").exists())
        self.assert_nsh_interactive(terminal)

    def test_hex_prompt_color_and_no_color(self):
        self.config.write_text(
            "nsh set prompt.template '{cwd} {symbol} '\n"
            "nsh set prompt.symbol '❯'\n"
            "nsh set prompt.path_color '#69B7FF'\n"
        )
        env = {**self.env, "COLORTERM": "truecolor"}
        terminal = self.terminal(env=env)
        self.assertIn(b"\x1b[38;2;105;183;255m~\x1b[0m", terminal.read_until(NSH_PROMPT))
        terminal = self.terminal()
        self.assertIn(b"\x1b[38;5;117m~\x1b[0m", terminal.read_until(NSH_PROMPT))
        terminal = self.terminal(env={**env, "NO_COLOR": "1"})
        self.assertNotIn(b"\x1b[38;", terminal.read_until(NSH_PROMPT))


if __name__ == "__main__":
    unittest.main(verbosity=2)
