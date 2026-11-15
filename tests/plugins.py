#!/usr/bin/env python3
"""Public plugin API and manager through the installed command-line interface."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

BIN = Path(sys.argv.pop(1)).resolve()


class Plugins(unittest.TestCase):
    def setUp(self):
        self.temp = self.enterContext(tempfile.TemporaryDirectory(prefix="nsh-plugins-"))
        self.home = Path(self.temp)
        self.root = self.home / "config" / "nsh"
        self.root.mkdir(parents=True)
        self.env = {"PATH": "/usr/bin:/bin", "HOME": self.temp, "TERM": "dumb",
                    "XDG_CONFIG_HOME": str(self.home / "config"), "LC_ALL": "C.UTF-8"}

    def plugin(self, name, source, extra=""):
        path = self.root / "plugins" / name
        path.mkdir(parents=True, exist_ok=True)
        (path / "plugin.toml").write_text(
            f'name = "{name}"\nversion = "1.0.0"\napi_version = 1\nentry = "init.lua"\n'
            f'description = "tests"\nauthor = "NSH"\n{extra}')
        (path / "init.lua").write_text(source)
        return path

    def nsh(self, *args, expected=0, cwd=None, stdin=None):
        p = subprocess.run([str(BIN), *args], env=self.env, cwd=cwd or self.home,
                           input=stdin, capture_output=True, timeout=8)
        self.assertEqual(p.returncode, expected, p.stderr)
        return p

    def test_discovery_explicit_enable_and_recovery(self):
        self.plugin("hello", 'nsh.command.register("hello", function() nsh.print("lua") end)')
        self.assertIn(b"hello", self.nsh("plugin", "list").stdout)
        self.assertIn(b"1.0.0", self.nsh("plugin", "info", "hello").stdout)
        self.nsh("plugin", "enable", "hello")
        self.assertEqual(self.nsh("--plugins", "-c", "hello").stdout, b"lua\n")
        self.nsh("-c", "hello", expected=127)
        self.nsh("--no-plugins", "--plugins", "-c", "hello", expected=127)
        self.nsh("plugin", "disable", "hello")
        self.nsh("--plugins", "-c", "hello", expected=127)

    def test_command_state_pipeline_and_redirection(self):
        (self.home / "dev").mkdir()
        self.plugin("state", '''
nsh.command.register("cdev", function(args)
  nsh.shell.set("VALUE", args[1])
  nsh.env.set("MARK", "exported")
  nsh.shell.cd(nsh.env.get("HOME") .. "/dev")
  nsh.print(args[1])
  return 43
end)
''', 'capabilities = ["shell-state"]\n')
        self.nsh("plugin", "enable", "state")
        p=self.nsh("--plugins", "-c",
            'cdev "first λ" > output; echo "$?"; read line < ../output; echo "$line"; '
            'echo "$VALUE"; /usr/bin/printenv MARK; pwd; '
            'cdev piped | /usr/bin/cat; echo "$VALUE"')
        self.assertEqual(p.stdout.splitlines(), [
            b"43", "first λ".encode(), "first λ".encode(), b"exported",
            os.fsencode(self.home / "dev"), b"piped", "first λ".encode()])

    def test_wrapping_keeps_original_command_reachable(self):
        self.plugin("wrap", '''
nsh.command.wrap("echo", function(args, original)
  nsh.print("wrapped, status=" .. original)
  return original
end)
''', 'capabilities = ["override"]\n')
        self.nsh("plugin", "enable", "wrap")
        p=self.nsh("--plugins", "-c", "echo original; command echo explicitly-original")
        self.assertEqual(p.stdout, b"original\nwrapped, status=0\nexplicitly-original\n")

    def test_missing_dependency_and_bad_api(self):
        self.plugin("dependent", "", 'dependencies = ["absent"]\n')
        p=self.nsh("plugin", "enable", "dependent", expected=1)
        self.assertIn(b"absent", p.stderr)
        bad=self.plugin("badapi", "")
        f=bad / "plugin.toml"
        f.write_text(f.read_text().replace("api_version = 1", "api_version = 99"))
        self.assertIn(b"99", self.nsh("plugin", "enable", "badapi", expected=1).stderr)

    def test_runtime_and_reload_roll_back_registrations(self):
        folder=self.plugin("reload", '''
nsh.command.register("old_fn", function() nsh.print("old") end)
nsh.command.register("crash", function() error("broken callback") end)
''')
        self.nsh("plugin", "enable", "reload")
        p=self.nsh("--plugins", "-ic", f"""
old_fn
crash; echo $?
printf '%s\\n' 'nsh.command.register("new_fn", function() nsh.print("new") end)' > '{folder}/init.lua'
nsh plugin reload reload
new_fn
old_fn; echo $?
nsh plugin disable reload
new_fn; echo $?
""")
        self.assertEqual(p.stdout, b"old\n1\nnew\n127\n127\n")
        self.assertIn(b"broken callback", p.stderr)
        self.assertIn(b"init.lua:", p.stderr)

    def test_dependencies_enable_in_order_and_disable_in_reverse(self):
        self.plugin("helper", """
nsh.on("enable", function() nsh.print("helper-on") end)
nsh.on("unload", function() nsh.print("helper-off") end)
nsh.command.register("help_from_lua", function() nsh.print("helper-command") end)
""")
        self.plugin("consumer", """
nsh.on("enable", function() nsh.print("consumer-on") end)
nsh.on("unload", function() nsh.print("consumer-off") end)
""", 'dependencies = ["helper@^1.0"]\npriority = -10\n')
        self.nsh("plugin", "enable", "consumer")
        listing = self.nsh("plugin", "list").stdout
        self.assertIn(b"helper\tenabled", listing)
        p = self.nsh("--plugins", "-c", "help_from_lua; nsh plugin disable consumer; help_from_lua; echo $?")
        self.assertEqual(p.stdout, b"helper-on\nconsumer-on\nhelper-command\nconsumer-off\nhelper-off\n127\n")

    def test_failed_hot_reload_keeps_old_runtime(self):
        folder = self.plugin("working", 'nsh.command.register("alive", function() nsh.print("alive") end)')
        self.nsh("plugin", "enable", "working")
        p = self.nsh("--plugins", "-c", f"""
printf '%s\\n' 'error("failed reload")' > '{folder}/init.lua'
nsh plugin reload working; echo "$?"
alive
""")
        self.assertEqual(p.stdout, b"1\nalive\n")
        self.assertIn(b"failed reload", p.stderr)
        self.assertIn(b"init.lua:", p.stderr)

    def test_special_builtin_requires_explicit_permission(self):
        self.plugin("untrusted-exit", 'nsh.command.replace("exit", function() nsh.print("wrong") end)',
                    'capabilities = ["override"]\n')
        self.nsh("plugin", "enable", "untrusted-exit")
        p = self.nsh("--plugins", "-c", "exit 22; echo wrong", expected=22)
        self.assertEqual(p.stdout, b"")
        self.assertIn(b"exit", p.stderr)

    def test_absolute_shebang_and_posix_stdin_and_dot(self):
        script = self.home / "script"
        script.write_text(f"#!{BIN}\n" + "echo \"script:$1\"; . ./library\n")
        script.chmod(0o700)
        (self.home / "library").write_text("echo sourced\n")
        direct = subprocess.run([str(script), "arg"], env=self.env, cwd=self.home,
                                capture_output=True, timeout=8)
        self.assertEqual((direct.returncode, direct.stdout), (0, b"script:arg\nsourced\n"))
        self.assertEqual(self.nsh("-s", stdin=b'echo via-stdin; . ./library\n').stdout,
                         b"via-stdin\nsourced\n")

    def test_command_not_loaded_when_config_is_rejected(self):
        self.plugin("ghost", 'nsh.command.register("phantom", function() nsh.print("loaded") end)')
        (self.root / "conf.sh").write_text("nsh plugin enable ghost\nnsh set prompt.path_color garbage\n")
        p=self.nsh("-ic", "phantom", expected=127)
        self.assertNotIn(b"loaded", p.stdout)
        self.assertIn(b"configuration transaction discarded", p.stderr)


if __name__ == "__main__":
    unittest.main()
