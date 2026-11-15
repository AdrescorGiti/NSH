#!/usr/bin/env python3
"""Inspect generated prompts under a real terminal and redirected input."""
import os
from pathlib import Path
import re
import socket
import subprocess
import tempfile
import unittest

from startup_pty import BIN, ROOT_PROMPT, Terminal


CSI = re.compile(rb"\x1b\[[0-?]*[ -/]*[@-~]")
ARROW = "❯".encode()


class PromptTests(unittest.TestCase):
    def setUp(self):
        self.home = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="nsh-prompt-")))
        self.work = self.home / "workspace"
        self.work.mkdir()
        self.config = self.home / "config/nsh/conf.sh"
        self.config.parent.mkdir(parents=True)
        self.env = {
            "HOME": str(self.home),
            "PATH": "/usr/bin:/bin",
            "USER": "terminal-user",
            "TERM": "xterm-256color",
            "COLORTERM": "truecolor",
            "LC_ALL": "C.UTF-8",
            "XDG_CONFIG_HOME": str(self.config.parents[1]),
            "XDG_STATE_HOME": str(self.home / "state"),
        }

    def open_terminal(self, *args, extra=None):
        env = {**self.env, **(extra or {})}
        terminal = Terminal([str(BIN), *args], env, self.work)
        self.addCleanup(terminal.close)
        return terminal

    def test_clean_home_has_two_line_rgb_prompt(self):
        terminal = self.open_terminal()
        output = terminal.read_until(ARROW)
        plain = CSI.sub(b"", output)
        self.assertRegex(plain, rb"terminal-user@[^\r\n]+  ~/workspace\r+\n" + ARROW)
        self.assertGreaterEqual(output.count(b"\x1b[38;2;"), 3)
        self.assertNotIn(b"\x1b[38;2;", plain)

    def test_status_symbol_colors_and_segment_masks_apply(self):
        self.config.write_text(
            "nsh set prompt.user false\n"
            "nsh set prompt.host false\n"
            "nsh set prompt.path_color '#6981AB'\n"
            "nsh set prompt.symbol.success READY\n"
            "nsh set prompt.symbol.error FAILED\n"
            "nsh set prompt.symbol.success_color '#A0E0F0'\n"
            "nsh set prompt.symbol.error_color '#FF5122'\n"
        )
        terminal = self.open_terminal()
        output = terminal.read_until(b"READY")
        self.assertNotIn(b"terminal-user@", output)
        self.assertIn(b"\x1b[38;2;105;129;171m~/workspace\x1b[0m", output)
        self.assertIn(b"\x1b[38;2;160;224;240mREADY", output)
        terminal.send(b"false\r")
        terminal.read_until(b"\x1b[?2004l")
        self.assertIn(b"\x1b[38;2;255;81;34mFAILED", terminal.read_until(b"FAILED"))

    def test_no_color_dumb_and_non_tty_disable_ansi_colors(self):
        self.config.write_text("nsh set color.mode always\n")
        for extra in ({"NO_COLOR": "1"}, {"TERM": "dumb"}):
            with self.subTest(extra=extra):
                terminal = self.open_terminal(extra=extra)
                output = terminal.read_until(ARROW)
                self.assertNotIn(b"\x1b[38;", output)
        result = subprocess.run(
            [str(BIN), "-i"], env=self.env, cwd=self.work, input=b"exit\n",
            capture_output=True, timeout=5,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, b"")
        self.assertIn(ARROW, result.stderr)
        self.assertNotIn(b"\x1b", result.stderr)

    def test_explicit_ps1_and_vanilla_preserve_minimal_control(self):
        terminal = self.open_terminal(extra={"PS1": "custom> "})
        output = terminal.read_until(b"custom> ")
        self.assertNotIn(ARROW, output)
        terminal = self.open_terminal("--vanilla")
        output = terminal.read_until(ROOT_PROMPT)
        self.assertNotIn(b"~/workspace", output)

    def test_template_renders_literal_newline_and_hostname(self):
        self.config.write_text("nsh set prompt.template '{host} {cwd}\n{symbol} '\n")
        terminal = self.open_terminal()
        output = CSI.sub(b"", terminal.read_until(ARROW))
        self.assertRegex(output, re.escape(os.fsencode(socket.gethostname())) + rb" ~/workspace\r+\n" + ARROW)

    def test_git_segment_is_opt_in_and_escapes_branch_data(self):
        tools = self.home / "tools"
        tools.mkdir()
        git = tools / "git"
        marker = self.home / "git-used"
        git.write_text(
            "#!/usr/bin/python3\n"
            "from pathlib import Path\n"
            f"Path({str(marker)!r}).touch()\n"
            "print('main\\x1b[2J')\n"
        )
        git.chmod(0o700)
        extra = {"PATH": f"{tools}:/usr/bin:/bin"}
        terminal = self.open_terminal(extra=extra)
        terminal.read_until(ARROW)
        self.assertFalse(marker.exists())
        self.config.write_text("nsh module enable git-prompt\n")
        terminal = self.open_terminal(extra=extra)
        output = terminal.read_until(ARROW)
        self.assertTrue(marker.exists())
        self.assertIn(b"  main\\u{1b}[2J", CSI.sub(b"", output))
        self.assertNotIn(b"\x1b[2J", output)


if __name__ == "__main__":
    unittest.main(verbosity=2)
