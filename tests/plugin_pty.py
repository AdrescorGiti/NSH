#!/usr/bin/env python3
"""Run shipped Lua plugins against the same public APIs used by installed plugins."""
from pathlib import Path
import shutil
import tempfile
import unittest

from startup_pty import BIN, Terminal

PROJECT = Path(__file__).resolve().parents[1]
PROMPT = b"lua-test> "
GHOST = b"\x1b[2;90m"
RESET = b"\x1b[0m"
COLOR_COMMAND = b"\x1b[38;2;124;210;138m"
COLOR_UNKNOWN = b"\x1b[38;2;240;113;120m"


class PluginTerminalTests(unittest.TestCase):
    def setUp(self):
        self.home = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="nsh-plugin-ui-")))
        self.cwd = self.home / "work"
        self.cwd.mkdir()
        self.config = self.home / "config/nsh"
        self.plugins = self.config / "plugins"
        self.plugins.mkdir(parents=True)
        self.env = {
            "HOME": str(self.home),
            "PATH": "/usr/bin:/bin",
            "LC_ALL": "C.UTF-8",
            "TERM": "xterm-256color",
            "COLORTERM": "truecolor",
            "PS1": PROMPT.decode(),
            "XDG_CONFIG_HOME": str(self.home / "config"),
            "XDG_STATE_HOME": str(self.home / "state"),
        }

    def terminal(self, *plugins, args=(), prompt=PROMPT):
        for name in plugins:
            if not (self.plugins / name).exists():
                shutil.copytree(PROJECT / "examples/plugins" / name, self.plugins / name)
        (self.config / "conf.sh").write_text(
            "".join(f"nsh plugin enable {name}\n" for name in plugins)
        )
        terminal = Terminal([str(BIN), *args], self.env, self.cwd)
        self.addCleanup(terminal.close)
        terminal.read_until(prompt)
        return terminal

    def write_plugin(self, name, source):
        directory = self.plugins / name
        directory.mkdir()
        (directory / "plugin.toml").write_text(
            f'name = "{name}"\nversion = "1.0.0"\napi_version = 1\nentry = "init.lua"\n'
            'description = "PTY regression fixture"\nauthor = "NSH tests"\n'
        )
        (directory / "init.lua").write_text(source)

    def test_ghost_is_visual_until_right_arrow(self):
        (self.cwd / "msh.sh").touch()
        terminal = self.terminal("file-suggestions")
        terminal.send(b"printf '<%s>\\n' msh")
        terminal.read_until(b"msh" + GHOST + b".sh" + RESET)
        terminal.send(b"\r")
        terminal.read_until(b"\x1b[?2004l<msh>\r\n")
        terminal.read_until(PROMPT)
        terminal.send(b"printf '<%s>\\n' msh")
        terminal.read_until(b"msh" + GHOST + b".sh" + RESET)
        terminal.send(b"\x1b[C\r")
        terminal.read_until(b"\x1b[?2004l<msh.sh>\r\n")

    def test_unicode_suggestion_with_space_and_tilde(self):
        (self.home / "тест файл").touch()
        terminal = self.terminal("file-suggestions")
        terminal.send("printf '<%s>\\n' ~/тест".encode())
        terminal.read_until(GHOST + "\\ файл".encode() + RESET)
        terminal.send(b"\x1b[C\r")
        terminal.read_until(b"\x1b[?2004l<" + str(self.home / "тест файл").encode() + b">\r\n")

    def test_absolute_suggestion_inside_open_double_quotes(self):
        (self.cwd / "ab xyz").touch()
        terminal = self.terminal("file-suggestions")
        terminal.send(f"printf '<%s>\\n' \"{self.cwd}/ab".encode())
        terminal.read_until(GHOST + b" xyz" + RESET)
        terminal.send(b'\x1b[C"\r')
        terminal.read_until(b"\x1b[?2004l<" + str(self.cwd / "ab xyz").encode() + b">\r\n")

    def test_directory_suggestions_end_with_slash(self):
        (self.cwd / "documents").mkdir()
        terminal = self.terminal("file-suggestions")
        terminal.send(b"echo doc")
        terminal.read_until(GHOST + b"uments/" + RESET)
        terminal.send(b"\x1b[D")
        terminal.read_until(b"\x1b[J" + PROMPT + b"echo doc\r")
        terminal.send(b"\x03")

    def test_highlighting_and_suggestions_coexist(self):
        (self.cwd / "alpha.sh").touch()
        terminal = self.terminal("file-suggestions", "syntax-colors")
        terminal.send(b"echo alp")
        out = terminal.read_until(GHOST + b"ha.sh" + RESET)
        self.assertIn(COLOR_COMMAND + b"echo" + RESET, out)
        terminal.send(b"\x03")
        terminal.read_until(PROMPT)
        terminal.send(b"nsh_plugin_unknown_cmd")
        out = terminal.read_until(COLOR_UNKNOWN + b"nsh_plugin_unknown_cmd" + RESET)
        self.assertNotIn(COLOR_COMMAND + b"nsh_plugin_unknown_cmd", out)

    def test_syntax_colors_can_be_configured(self):
        self.env["NSH_SYNTAX_COMMAND"] = "#112233"
        terminal = self.terminal("syntax-colors")
        terminal.send(b"echo ready")
        terminal.read_until(b"\x1b[38;2;17;34;51mecho" + RESET)

    def test_shipped_examples_accept_manifest_palette(self):
        (self.cwd / "alpha.sh").touch()
        for name, options in {
            "file-suggestions": '\n[options.ghost]\nfg = "#8ADCF0"\nitalic = true\n',
            "syntax-colors": '\n[options.colors]\ncommand = "#5BD6CF"\n',
        }.items():
            destination = self.plugins / name
            shutil.copytree(PROJECT / "examples/plugins" / name, destination)
            with (destination / "plugin.toml").open("a") as manifest:
                manifest.write(options)
        terminal = self.terminal("file-suggestions", "syntax-colors")
        terminal.send(b"echo alp")
        output = terminal.read_until(b"\x1b[3;38;2;138;220;240mha.sh" + RESET)
        self.assertIn(b"\x1b[38;2;91;214;207mecho" + RESET, output)

    def test_no_plugins_recovers_plain_editor(self):
        (self.cwd / "alpha.sh").touch()
        terminal = self.terminal("file-suggestions", args=("--no-plugins",))
        terminal.send(b"echo alp\x1b[C\r")
        out = terminal.read_until(b"\x1b[?2004lalp\r\n")
        self.assertNotIn(GHOST, out)

    def test_provider_menu_and_virtual_text_are_visual(self):
        self.write_plugin("visual", """
            nsh.editor.on_change(function(line)
                if line == "echo hello" then
                    nsh.editor.set_virtual_text({
                        {offset = #line, text = string.char(27) .. "[2J"}
                    })
                end
            end)
            nsh.completion.register(function() return {"candidate-one", "candidate-two"} end)
        """)
        terminal = self.terminal("visual")
        terminal.send(b"echo hello\t")
        output = terminal.read_until(b"candidate-two")
        self.assertIn(rb"\u{1b}[2J", output)
        self.assertNotIn(b"\x1b[2J", output)
        self.assertIn(b"candidate-one", output)
        terminal.send(b"\r")
        terminal.read_until(b"\x1b[?2004lhello\r\n")

    def test_history_provider_drives_arrow_navigation(self):
        self.write_plugin("history-view", """
            nsh.history.register(function() return {"echo HISTORY_PROVIDED_MARKER"} end)
        """)
        terminal = self.terminal("history-view")
        terminal.send(b"\x1b[A\r")
        terminal.read_until(b"\x1b[?2004lHISTORY_PROVIDED_MARKER\r\n")

    def test_oversized_history_falls_back_to_session_history(self):
        self.write_plugin("large-history", """
            local line = string.rep("h", 65536)
            nsh.history.register(function()
                local entries = {}
                for i = 1, 129 do entries[i] = line end
                return entries
            end)
        """)
        terminal = self.terminal("large-history")
        self.assertIn(b"plugin history exceeds 8 MiB", terminal.data)
        terminal.send(b"echo HISTORY_FALLBACK\r")
        terminal.read_until(b"\x1b[?2004lHISTORY_FALLBACK\r\n")
        terminal.read_until(PROMPT)
        terminal.send(b"\x1b[A\r")
        terminal.read_until(b"\x1b[?2004lHISTORY_FALLBACK\r\n")

    def test_disable_removes_ghost_and_key_provider(self):
        (self.cwd / "alpha.sh").touch()
        terminal = self.terminal("file-suggestions")
        terminal.send(b"echo alp")
        terminal.read_until(GHOST + b"ha.sh" + RESET)
        terminal.send(b"\x03")
        terminal.read_until(PROMPT)
        terminal.send(b"nsh plugin disable file-suggestions\r")
        terminal.read_until(PROMPT)
        terminal.send(b"echo alp\x1b[C\r")
        output = terminal.read_until(b"\x1b[?2004lalp\r\n")
        self.assertNotIn(GHOST, output)

    def test_editor_cannot_load_a_new_module_from_disk(self):
        self.write_plugin("editor-module", """
            nsh.editor.on_change(function(line)
                if line == "echo working" then nsh.require("lazy") end
            end)
        """)
        (self.plugins / "editor-module" / "lazy.lua").write_text("error('lazy module executed')\n")
        terminal = self.terminal("editor-module")
        terminal.send(b"echo working")
        output = terminal.read_until(b"must first be loaded during plugin initialization")
        self.assertNotIn(b"lazy module executed", output)
        terminal.send(b"\r")
        terminal.read_until(b"working\r\n")

    def test_named_styles_reach_prompt_highlights_ghost_virtual_text_and_menu(self):
        self.write_plugin("styled-visual", """
            nsh.style.define("accent", {fg = "#123456", bg = "#543210", bold = true})
            nsh.prompt.register(function()
                return {{text = "lua-test> ", style = "accent"}}
            end)
            nsh.editor.on_change(function(line)
                if line == "echo mark" then
                    nsh.editor.set_ghost_text("ed", {fg = "#A1B2C3", italic = true})
                    nsh.editor.set_virtual_text({{offset = 0, text = "λ ", style = "accent"}})
                end
            end)
            nsh.highlight.register(function(line)
                if line == "echo mark" then
                    return {{start = 0, ["end"] = 4, style = {bg = "#010203", underline = true}}}
                end
            end)
            nsh.completion.register(function()
                return {
                    items = {"plain", {text = "item", style = {fg = "#FFF000"}}, "chosen"},
                    selected = 3,
                    style = "#112233",
                    selected_style = "accent",
                }
            end)
        """)
        terminal = self.terminal("styled-visual")
        accent = b"\x1b[1;38;2;18;52;86;48;2;84;50;16m"
        self.assertIn(accent + PROMPT, bytes(terminal.data))
        terminal.send(b"echo mark")
        output = terminal.read_until(b"\x1b[3;38;2;161;178;195med" + RESET)
        self.assertIn(accent + "λ ".encode() + RESET, output)
        self.assertIn(b"\x1b[4;48;2;1;2;3mecho" + RESET, output)
        terminal.send(b"\t")
        output = terminal.read_until(accent + b"> chosen" + RESET)
        self.assertIn(b"\x1b[38;2;17;34;51m  plain", output)
        self.assertIn(b"\x1b[38;2;255;240;0m  item", output)
        terminal.send(b"\r")
        terminal.read_until(b"\x1b[?2004lmark\r\n")

    def test_theme_prompt_obeys_terminal_palette_and_no_color(self):
        self.write_plugin("palette-policy", """
            nsh.prompt.register(function()
                return {{text = "lua-test> ", style = {fg = "#FF0000", bg = "#0000FF"}}}
            end)
        """)
        self.env.pop("COLORTERM")
        indexed = self.terminal("palette-policy")
        self.assertIn(b"\x1b[38;5;196;48;5;21m" + PROMPT, bytes(indexed.data))
        self.env["NO_COLOR"] = "1"
        plain = self.terminal("palette-policy")
        self.assertNotIn(b"\x1b[38;", bytes(plain.data))

    def test_aurora_theme_menu_selects_without_running(self):
        terminal = self.terminal("aurora-theme", prompt="❯".encode() + RESET)
        selected = b"\x1b[1;38;2;22;47;55;48;2;123;222;217m"
        terminal.send(b"\t")
        terminal.read_until(selected + b"> pwd" + RESET)
        terminal.send(b"\x1b[B")
        terminal.read_until(selected + b"> ls" + RESET)
        terminal.send(b"\x1b[A\r")
        output = terminal.read_until(RESET + b" pwd\r")
        self.assertNotIn(b"\x1b[?2004l", output)
        terminal.send(b"\r")
        terminal.read_until(b"\x1b[?2004l" + str(self.cwd).encode() + b"\r\n")

    def test_empty_menu_keeps_lower_priority_completion(self):
        self.write_plugin("empty-menu", """
            nsh.completion.register(function() return {} end)
        """)
        self.write_plugin("lower-menu", """
            nsh.completion.register(function() return {"LOWER_COMPLETION"} end)
        """)
        terminal = self.terminal("empty-menu", "lower-menu")
        terminal.send(b"\t")
        terminal.read_until(b"LOWER_COMPLETION")

    def test_oversized_visuals_reject_before_decoding_more_rows(self):
        self.write_plugin("safe-menu", """
            nsh.completion.register(function() return {"SAFE_COMPLETION"} end)
        """)
        for mode, source in (
            ("menu", """
                nsh.completion.register(function()
                    return {large, large, large, large, large, {text = "late", style = "missing"}}
                end)
            """),
            ("virtual", """
                nsh.editor.on_change(function(line)
                    if line == "echo stable" then
                        local row = {offset = 0, text = large}
                        nsh.editor.set_virtual_text({
                            row, row, row, row, row,
                            {offset = 0, text = "late", style = "missing"},
                        })
                    end
                end)
            """),
        ):
            with self.subTest(mode=mode):
                self.write_plugin(mode, 'local large = string.rep("oversized", 1024)\n' + source)
                terminal = self.terminal(mode, "safe-menu")
                terminal.send(b"echo stable\t")
                output = terminal.read_until(b"exceeds 32 KiB")
                self.assertNotIn(b"oversizedoversized", output)
                terminal.read_until(b"SAFE_COMPLETION")
                terminal.send(b"\r")
                terminal.read_until(b"\x1b[?2004lstable\r\n")

    def test_prompt_checks_escaped_text_budget(self):
        self.write_plugin("large-prompt", """
            nsh.prompt.register(function()
                return {{text = string.rep(string.char(27), 9000), style = "#00FF00"}}
            end)
        """)
        terminal = self.terminal("large-prompt")
        self.assertIn(b"editor prompt exceeds 32 KiB", terminal.data)
        self.assertNotIn(rb"\u{1b}\u{1b}", terminal.data)

    def test_prompt_preserves_grapheme_split_across_different_styles(self):
        self.write_plugin("split-prompt", """
            nsh.prompt.register(function()
                return {
                    {text = "👩‍", style = "#ABCDEF"},
                    {text = "💻", style = "#FF0000"},
                    "lua-test> ",
                }
            end)
        """)
        terminal = self.terminal("split-prompt")
        terminal.read_until(b"\x1b[12C")
        self.assertIn(b"\x1b[38;2;171;205;239m" + "👩‍💻".encode() + RESET, terminal.data)
        self.assertNotIn(b"\x1b[38;2;255;0;0m", terminal.data)


if __name__ == "__main__":
    unittest.main(verbosity=2)
