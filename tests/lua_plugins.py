#!/usr/bin/env python3
"""Integration contracts for Lua commands through NSH's normal executor."""
import os
from pathlib import Path
import resource
import subprocess
import sys
import tempfile
import unittest

BIN = os.path.abspath(sys.argv.pop(1))


class LuaCommands(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="nsh-lua-api-")
        self.addCleanup(self.tmp.cleanup)
        self.home = Path(self.tmp.name)
        self.nsh_dir = self.home / ".config/nsh"
        self.env = {
            "PATH": "/usr/bin:/bin",
            "TERM": "dumb",
            "HOME": str(self.home),
            "XDG_CONFIG_HOME": str(self.home / ".config"),
            "XDG_STATE_HOME": str(self.home / ".state"),
            "LC_ALL": "C.UTF-8",
        }

    def fixture(self, script, *caps, name="fixture", entry="init.lua", options="", enabled=True):
        directory = self.nsh_dir / "plugins" / name
        directory.mkdir(parents=True)
        array = ", ".join(f'"{cap}"' for cap in caps)
        (directory / "plugin.toml").write_text(
            f'name = "{name}"\nversion = "0.1.0"\napi_version = 1\nentry = "{entry}"\ncapabilities = [{array}]\n'
            + options
        )
        entry_file = directory / entry
        entry_file.parent.mkdir(parents=True, exist_ok=True)
        entry_file.write_text(script)
        if enabled:
            with (self.nsh_dir / "conf.sh").open("a") as config:
                config.write(f"nsh plugin enable {name}\n")
        return directory

    def nsh(self, code, *options, input=None, limit_fds=None):
        def apply_limit():
            resource.setrlimit(resource.RLIMIT_NOFILE, (limit_fds, limit_fds))

        return subprocess.run(
            [BIN, "-i", *options, "-c", code],
            cwd=self.home,
            env=self.env,
            input=input,
            capture_output=True,
            timeout=8,
            preexec_fn=apply_limit if limit_fds is not None else None,
        )

    def test_dispatch_hooks_and_explicit_process_replacement(self):
        self.fixture(
            """
            nsh.on("before-dispatch", function(args)
              if args[1] == "/usr/bin/printenv" then nsh.env.set("LUA_STEP", "visible") end
            end)
            nsh.on("after-dispatch", function(args)
              if args[2] == "/usr/bin/printenv" then nsh.print("status=" .. args[1]) end
            end)
            nsh.command.register("transition", function()
              nsh.process.exec("/usr/bin/printf", {"replaced\\n"})
            end)
            """,
            "shell-state",
            "exec",
        )
        p = self.nsh("/usr/bin/printenv LUA_STEP; transition; echo unexpected")
        self.assertEqual((p.returncode, p.stdout, p.stderr),
                         (0, b"visible\nstatus=0\nreplaced\n", b""))

    def test_plugin_local_modules_are_cached_for_one_activation(self):
        root = self.fixture(
            """
            assert(require == nil and loadfile == nil and package == nil)
            local words = nsh.require("parts.words")
            assert(rawequal(words, nsh.require("parts.words")))
            assert(nsh.require("parts.no_result") == true)
            assert(nsh.require("parts.flag") == false)
            assert(nsh.require("parts.flag") == false and flag_loads == 1)
            nsh.command.register("nsh_test_module_speak", function()
              nsh.print(nsh.require("parts.words").value)
            end)
            """,
            entry="entry/init.lua",
        )
        (root / "parts").mkdir()
        (root / "parts/words.lua").write_text('return {value = "before"}\n')
        (root / "parts/no_result.lua").write_text('-- this module returns no value\n')
        (root / "parts/flag.lua").write_text('flag_loads = (flag_loads or 0) + 1; return false\n')
        result = self.nsh(
            "nsh_test_module_speak; printf '%s\\n' 'return {value = \"after\"}' > "
            '"$XDG_CONFIG_HOME/nsh/plugins/fixture/parts/words.lua"; '
            "nsh_test_module_speak; nsh plugin reload fixture; nsh_test_module_speak"
        )
        self.assertEqual((result.returncode, result.stdout, result.stderr),
                         (0, b"before\nbefore\nafter\n", b""))

    def test_plugin_local_modules_reject_escape_untrusted_files_and_cycles(self):
        root = self.fixture("")
        (self.home / "outside.lua").write_text("error('outside reached')\n")
        (root / "symlink.lua").symlink_to(self.home / "outside.lua")
        (root / "untrusted.lua").write_text("error('untrusted reached')\n")
        (root / "untrusted.lua").chmod(0o666)
        (root / "cycle_a.lua").write_text('return nsh.require("cycle_b")\n')
        (root / "cycle_b.lua").write_text('return nsh.require("cycle_a")\n')
        (root / "compiled.lua").write_bytes(b"\x1bLua")
        for module, diagnostic in [
            ("../outside", b"invalid plugin module name"),
            ("symlink", b"symlink.lua"),
            ("untrusted", b"untrusted file"),
            ("cycle_a", b"circular plugin module dependency"),
            ("compiled", b"binary chunk"),
        ]:
            with self.subTest(module=module):
                (root / "init.lua").write_text(f'nsh.require("{module}")\n')
                result = self.nsh("echo alive")
                self.assertEqual((result.returncode, result.stdout), (0, b"alive\n"))
                self.assertIn(diagnostic, result.stderr)
                self.assertNotIn(b"reached", result.stderr)

    def test_unloaded_modules_cannot_be_read_during_callbacks(self):
        root = self.fixture('nsh.command.register("too_late", function() nsh.require("unused") end)')
        (root / "unused.lua").write_text("error('module ran')\n")
        result = self.nsh("too_late; printf '%s\\n' \"$?\"")
        self.assertEqual((result.returncode, result.stdout), (0, b"1\n"))
        self.assertIn(b"must first be loaded during plugin initialization", result.stderr)
        self.assertNotIn(b"module ran", result.stderr)

    def test_plugin_manifest_options_are_typed_and_copied(self):
        self.fixture(
            """
            local defaults = nsh.config.get()
            assert(defaults.enabled == false and defaults.count == 4096)
            assert(defaults.message == 'Привет' and defaults.scale == 0.5)
            assert(defaults.date == '2026-11-15')
            assert(defaults.palette.path == '#69B7FF')
            assert(defaults.styles[1] == 'bold' and defaults.styles[2] == 8)
            assert(nsh.config.get('enabled', true) == false)
            assert(nsh.config.get('absent') == nil)
            assert(nsh.config.get('absent', 'fallback') == 'fallback')
            defaults.palette.path = 'changed'
            local palette = nsh.config.get('palette')
            assert(palette.path == '#69B7FF')
            nsh.command.register('options', function()
              nsh.print(nsh.config.get('palette').path, nsh.config.get('message'))
            end)
            """,
            options='''
[options]
enabled = false
count = 4096
scale = 0.5
message = "Привет"
date = 2026-11-15
styles = ["bold", 8]
[options.palette]
path = "#69B7FF"
''',
        )
        result = self.nsh("options")
        self.assertEqual((result.returncode, result.stdout, result.stderr),
                         (0, "#69B7FF\tПривет\n".encode(), b""))

    def test_non_finite_option_preserves_plugin_state_when_enabling_a_peer(self):
        self.fixture(
            """
            local sample = nsh.config.get('sample')
            assert(sample ~= sample)
            local count = 0
            nsh.command.register('nsh_test_option_count', function()
              count = count + 1
              nsh.print(count)
            end)
            """,
            options='\n[options]\nsample = nan\n',
        )
        self.fixture('', name='peer', enabled=False)
        result = self.nsh('nsh_test_option_count; nsh plugin enable peer; nsh_test_option_count')
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, b'1\n2\n', b''))

    def test_many_plugins_activate_under_a_low_descriptor_limit(self):
        names = [f"fd-fixture-{i:02}" for i in range(40)]
        for name in names[:-1]:
            self.fixture("", name=name, enabled=False)
        self.fixture(
            'nsh.command.register("nsh_test_last_plugin", function() nsh.print("active") end)',
            name=names[-1],
            enabled=False,
        )
        enabled = self.nsh_dir / "plugins" / "enabled"
        enabled.write_text("\n".join(names) + "\n")
        enabled.chmod(0o600)
        result = self.nsh("nsh_test_last_plugin", limit_fds=32)
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, b"active\n", b""))

    def test_binary_stdin_chunks_do_not_read_ahead(self):
        self.fixture(
            """
            nsh.command.register('binary', function()
              nsh.stdout.write(nsh.stdin.read(3))
              assert(nsh.stdin.read_line() == '')
              nsh.stdout.write(nsh.stdin.read(65536))
              assert(nsh.stdin.read(1) == nil)
            end)
            nsh.command.register('invalid_read', function() nsh.stdin.read(65537) end)
            """,
            "stdio",
        )
        (self.home / "payload").write_bytes(b"\xff\x00A\nremainder\n")
        result = self.nsh("binary < payload | /usr/bin/cat")
        self.assertEqual((result.returncode, result.stdout, result.stderr),
                         (0, b"\xff\x00Aremainder\n", b""))
        result = self.nsh("invalid_read")
        self.assertEqual(result.returncode, 1)
        self.assertIn(b"stdin read size must be between 1 and 65536", result.stderr)

    def test_binary_stdin_requires_stdio_capability(self):
        self.fixture('nsh.command.register("peek", function() nsh.stdin.read(1) end)')
        result = self.nsh("peek", input=b"private")
        self.assertEqual((result.returncode, result.stdout), (1, b""))
        self.assertIn(b"requires 'stdio' capability", result.stderr)

    def test_lua_command_arguments_and_exit_status(self):
        self.fixture(
            """
            nsh.command.register("luahello", function(args)
              nsh.print(args[1], args[2])
              return 7
            end)
            """,
            "stdio",
        )
        # Values include whitespace and a shell-metacharacter, passed literally.
        result = self.nsh("luahello 'a b' 'c|d'; printf 'status=%s\\n' \"$?\"")
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, b"a b\tc|d\nstatus=7\n")

    def test_command_mutates_live_shell_and_pipeline_keeps_isolation(self):
        (self.home / "work").mkdir()
        self.fixture(
            """
            nsh.command.register("change", function(args)
              nsh.shell.cd(nsh.env.get("HOME") .. "/work")
              nsh.shell.set("plugin_value", args[1])
              nsh.env.set("PLUGIN_EXPORT", "checked")
            end)
            """,
            "shell-state",
        )
        result = self.nsh(
            "change first; pwd; printf '%s\\n' \"$plugin_value\"; "
            "change second | cat; printf '%s\\n' \"$plugin_value\"; "
            "/usr/bin/printenv PLUGIN_EXPORT"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, f"{self.home}/work\nfirst\nfirst\nchecked\n".encode())

    def test_stdout_redirection_stdin_pipeline_and_stderr(self):
        self.fixture(
            """
            nsh.command.register("luaecho", function()
              nsh.stdout.write(nsh.stdin.read_line() .. "\\n")
              nsh.stderr.write("diagnostic\\n")
            end)
            """,
            "stdio",
        )
        result = self.nsh("printf 'from pipe\\n' | luaecho > result 2> error; cat result error")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, b"from pipe\ndiagnostic\n")

    def test_failed_command_does_not_apply_shell_actions(self):
        self.fixture(
            """
            nsh.command.register("fail-lua", function()
              nsh.shell.set("ordinary", "bad")
              error("fixture exploded")
            end)
            """,
            "shell-state",
        )
        result = self.nsh("ordinary=good; fail-lua; printf '%s\\n' \"$ordinary\"")
        self.assertEqual(result.stdout, b"good\n")
        self.assertIn(b"init.lua", result.stderr)
        self.assertIn(b"fixture exploded", result.stderr)

    def test_process_uses_staged_cwd_environment_and_native_execution(self):
        directory = self.home / "child"
        directory.mkdir()
        payload = directory / "no-shebang"
        payload.write_text("echo unexpectedly-started-a-shell\n")
        payload.chmod(0o700)
        self.fixture(
            """
            nsh.command.register("luaproc", function()
              nsh.shell.cd("child")
              nsh.env.set("RELEASE_LUA_ENV", "staged")
              assert(nsh.process.run("pwd") == 0)
              assert(nsh.process.run("printenv", {"RELEASE_LUA_ENV"}) == 0)
              assert(nsh.process.run("./no-shebang") == 126)
              return nsh.process.run("/missing-nsh-test-binary")
            end)
            """,
            "shell-state",
            "process",
        )
        result = self.nsh("luaproc > report; printf '%s\\n' \"$?\"; cat \"$HOME/report\"")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, f"127\n{directory}\nstaged\n".encode())

    def test_wrapper_and_unregister_follow_runtime_registry(self):
        self.fixture(
            """
            nsh.command.wrap("false", function(args, status)
              assert(status == 1)
              assert(nsh.command.unregister("false"))
              return 5
            end)
            """,
            "override",
        )
        result = self.nsh("false; printf '%s ' \"$?\"; false; printf '%s\\n' \"$?\"")
        self.assertEqual((result.returncode, result.stdout), (0, b"5 1\n"), result.stderr)

    def test_command_lookup_sees_other_plugins(self):
        self.fixture('nsh.command.register("lua-peer-command", function() end)', name="peer")
        self.fixture(
            """
            nsh.command.register("probe-peer", function()
              assert(nsh.command.exists("lua-peer-command"))
              return 11
            end)
            """,
        )
        self.assertEqual(self.nsh("probe-peer").returncode, 11)

    def test_no_plugins_recovery_switch(self):
        self.fixture(
            """
            nsh.command.register("cmd-recovery-test", function() return 0 end)
            """
        )
        result = self.nsh("cmd-recovery-test", "--no-plugins")
        self.assertEqual(result.returncode, 127)


if __name__ == "__main__":
    unittest.main()
