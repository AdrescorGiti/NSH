#!/usr/bin/env python3
"""Exercise NSH's terminal on an isolated controlling PTY."""
import errno
import fcntl
import os
import pathlib
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


BIN = str(pathlib.Path(sys.argv.pop(1) if len(sys.argv) > 1 else 'target/release/nsh').resolve())


class Shell:
    def __init__(self, env, *args):
        self.pid, self.fd = pty.fork()
        if not self.pid:
            os.execve(BIN, [BIN, *args], env)
        self.pending = b''
        self.resize(24, 80)

    def resize(self, rows, cols):
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))

    def send(self, data):
        os.write(self.fd, data)

    def output(self, expected):
        self.expect(b'\x1b[?2004l' + expected)

    def expect(self, needle):
        end = time.monotonic() + 4
        while (position := self.pending.find(needle)) == -1:
            left = end - time.monotonic()
            if left <= 0 or not select.select([self.fd], [], [], left)[0]:
                raise AssertionError(f'missing {needle!r} in {self.pending!r}')
            try:
                chunk = os.read(self.fd, 65536)
            except OSError as error:
                if error.errno != errno.EIO:
                    raise
                chunk = b''
            if not chunk:
                raise AssertionError(f'shell closed before {needle!r}: {self.pending!r}')
            self.pending += chunk
        self.pending = self.pending[position + len(needle):]

    def close(self):
        os.close(self.fd)
        try:
            os.killpg(self.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        os.waitpid(self.pid, 0)


class SessionTests(unittest.TestCase):
    def setUp(self):
        if not pathlib.Path(BIN).is_file():
            self.fail(f'Build and provide the NSH binary: {BIN}')
        self.tmp = self.enterContext(tempfile.TemporaryDirectory(prefix='nsh-pty-'))
        self.env = {
            'PATH': '/usr/bin:/bin', 'HOME': self.tmp, 'LANG': 'C.UTF-8',
            'TERM': 'xterm-256color', 'XDG_CONFIG_HOME': self.tmp + '/config',
            'XDG_STATE_HOME': self.tmp + '/state', 'PS1': '$ ', 'PS2': '> ',
        }
        self.config = pathlib.Path(self.tmp, 'config', 'nsh')
        self.config.mkdir(parents=True)

    def shell(self, *args, prompt=b'$ '):
        sh = Shell(self.env, *args)
        self.addCleanup(sh.close)
        sh.expect(prompt)
        return sh

    def test_history_draft_tab_and_interrupt(self):
        sh = self.shell('--vanilla')
        sh.send(b"printf '%s\\n' first\r")
        sh.output(b'first\r\n')
        sh.send(b"printf '%s\\n' dra\tft\x1b[A\x1b[B\r")
        sh.output(b'draft\r\n')
        sh.expect(b'\x1b[?2004h')
        sh.send(b'\x03')
        sh.expect(b'^C')
        sh.expect(b'\x1b[?2004h')
        sh.send(b"printf '%s\\n' \"$?\"\r")
        sh.output(b'130\r\n')
        self.assertFalse(pathlib.Path(self.tmp, 'state').exists())

    def test_bracketed_multiline_unicode_and_resize(self):
        sh = self.shell('--vanilla')
        sh.resize(24, 17)
        sh.send("\x1b[200~printf '%s\\n' 'e\u0301λλλλλλλλλλλ'\nprintf '%s\\n' pasted\x1b[201~\r".encode())
        sh.output('e\u0301λλλλλλλλλλλ\r\npasted\r\n'.encode())

    def test_reload_and_current_session(self):
        conf = self.config / 'conf.sh'
        conf.write_text("alias greeting='printf old'\n")
        sh = self.shell()
        conf.write_text("alias greeting='printf new'\n")
        sh.send(b"greeting; printf '\\n'\r")
        sh.output(b'old\r\n')
        sh.send(b'nsh set history.max_size 500; nsh get history.max_size\r')
        sh.output(b'500\r\n')
        second = self.shell()
        second.send(b"greeting; printf '\\n'\r")
        second.output(b'new\r\n')
        self.assertEqual(conf.read_text(), "alias greeting='printf new'\n")

    def test_config_check_and_noninteractive(self):
        marker = pathlib.Path(self.tmp, 'side-effect')
        conf = self.config / 'conf.sh'
        conf.write_text(f'echo activated > {marker}\n')
        checked = subprocess.run([BIN, 'config', 'check', str(conf)], env=self.env, capture_output=True, timeout=4)
        self.assertEqual(checked.returncode, 0, checked.stderr)
        self.assertIn(b'Dynamic settings', checked.stdout)
        self.assertFalse(marker.exists())
        result = subprocess.run([BIN, '-c', 'echo script'], env=self.env, capture_output=True, timeout=4)
        self.assertEqual((result.returncode, result.stdout), (0, b'script\n'))
        self.assertFalse(marker.exists())
        sh = self.shell('--vanilla')
        self.assertFalse(marker.exists())
        sh.send(b'echo ok\r')
        sh.output(b'ok\r\n')

    def test_ancestors_conf_and_cyclic_profiles(self):
        profiles = self.config / 'profiles'
        profiles.mkdir()
        (profiles / 'team.sh').write_text("nsh profile inherit developer\nnsh set history.max_size 17\n")
        (self.config / 'conf.sh').write_text('nsh set history.max_size 27\n')
        saved = subprocess.run([BIN, 'profile', 'use', 'team'], env=self.env, capture_output=True, timeout=4)
        self.assertEqual(saved.returncode, 0, saved.stderr)
        self.assertEqual((self.config / 'profile').read_text(), 'team\n')
        sh = self.shell('--profile', 'vanilla')
        sh.send(b'nsh get history.max_size\r')
        sh.output(b'27\r\n')
        self.assertEqual((self.config / 'profile').read_text(), 'team\n')
        blocked = subprocess.run([BIN, 'profile', 'delete', 'team'], env=self.env, capture_output=True, timeout=4)
        self.assertNotEqual(blocked.returncode, 0)
        (profiles / 'cycle.sh').write_text('nsh profile inherit cycle\n')
        cycle = subprocess.run([BIN, 'profile', 'use', 'cycle'], env=self.env, capture_output=True, timeout=4)
        self.assertNotEqual(cycle.returncode, 0)
        self.assertIn(b'cycle', cycle.stderr)
        self.assertEqual((self.config / 'profile').read_text(), 'team\n')
        (self.config / 'conf.sh').unlink()
        inherited = self.shell()
        inherited.send(b'nsh get history.max_size\r')
        inherited.output(b'17\r\n')

    def test_invalid_profile_setting_rolls_back_ancestors_and_other_changes(self):
        profiles = self.config / 'profiles'
        profiles.mkdir()
        (profiles / 'base.sh').write_text('''nsh set history.max_size 17
nsh bind F8 clear-line
nsh module enable highlighting
rollback_hook() { :; }
nsh hook add before-prompt rollback_hook
''')
        side_effect = pathlib.Path(self.tmp, 'external-effect')
        (profiles / 'team.sh').write_text(f'''nsh profile inherit base
echo made > '{side_effect}'
nsh set prompt.path_color invalid || :
nsh set editor.beep true
''')
        (self.config / 'conf.sh').write_text('nsh set history.ignore_duplicates true\n')
        sh = self.shell('--profile', 'team')
        sh.send(b'nsh get history.max_size; nsh get key.F8; nsh get editor.beep; nsh get history.ignore_duplicates; nsh hook list; nsh module list\r')
        sh.output(b'2000\r\nnone\r\nfalse\r\ntrue\r\ngit-prompt\tdisabled\r\nhighlighting\tdisabled\r\nhistory\tenabled\r\n')
        self.assertEqual(side_effect.read_text(), 'made\n')

    def test_failed_conf_keeps_successful_profile_settings(self):
        profiles = self.config / 'profiles'
        profiles.mkdir()
        (profiles / 'base.sh').write_text('nsh set history.max_size 17\n')
        (profiles / 'team.sh').write_text('nsh profile inherit base\nnsh set editor.beep true\n')
        (self.config / 'conf.sh').write_text('nsh set history.max_size 99\nnsh set editor.beep false\nnsh set key.Tab typo\n:\n')
        sh = self.shell('--profile', 'team')
        sh.send(b'nsh get history.max_size; nsh get editor.beep; nsh get key.Tab\r')
        sh.output(b'17\r\ntrue\r\nnone\r\n')

    def test_profile_syntax_error_preserves_working_prompt_and_defaults(self):
        profiles = self.config / 'profiles'
        profiles.mkdir()
        (profiles / 'broken.sh').write_text('nsh set history.max_size 17\nif; then\n')
        sh = self.shell('--profile', 'broken')
        sh.send(b'nsh get history.max_size\r')
        sh.output(b'2000\r\n')

    def test_missing_profile_parent_still_loads_independent_conf(self):
        profiles = self.config / 'profiles'
        profiles.mkdir()
        (profiles / 'broken.sh').write_text('nsh profile inherit missing\nnsh set history.max_size 17\n')
        (self.config / 'conf.sh').write_text('nsh set editor.beep true\n')
        sh = self.shell('--profile', 'broken')
        sh.send(b'nsh get history.max_size; nsh get editor.beep\r')
        sh.output(b'2000\r\ntrue\r\n')

    def test_pipeline_cannot_hide_dynamic_setting_error(self):
        profiles = self.config / 'profiles'
        profiles.mkdir()
        script = '''nsh set history.max_size 17
(nsh set prompt.path_color invalid || :) | cat >/dev/null
nsh set editor.beep true
'''
        (profiles / 'broken.sh').write_text(script)
        sh = self.shell('--profile', 'broken')
        sh.send(b'nsh get history.max_size; nsh get editor.beep\r')
        sh.output(b'2000\r\nfalse\r\n')

    def test_untrusted_startup_files_and_state(self):
        bad = pathlib.Path(self.tmp, 'outside.sh')
        marker = pathlib.Path(self.tmp, 'executed')
        bad.write_text(f'echo wrong > {marker}\n')
        (self.config / 'conf.sh').symlink_to(bad)
        state = pathlib.Path(self.tmp, 'state', 'nsh')
        state.mkdir(mode=0o700, parents=True)
        history = state / 'history'
        history.write_text('text from an existing file\n')
        history.chmod(0o644)
        sh = self.shell()
        sh.send(b'echo usable\r')
        sh.output(b'usable\r\n')
        sh.expect(b'\x1b[?2004h')
        self.assertFalse(marker.exists())
        self.assertTrue((self.config / 'conf.sh').is_symlink())
        self.assertEqual(history.read_text(), 'text from an existing file\n')

    def test_parallel_and_partial_history(self):
        first, second = self.shell(), self.shell()
        first.send(b'echo parallel_1\r')
        second.send(b'echo parallel_2\r')
        first.output(b'parallel_1\r\n')
        second.output(b'parallel_2\r\n')
        first.expect(b'\x1b[?2004h')
        second.expect(b'\x1b[?2004h')
        history = pathlib.Path(self.tmp, 'state', 'nsh', 'history')
        self.assertEqual(history.stat().st_mode & 0o777, 0o600)
        self.assertEqual(history.parent.stat().st_mode & 0o777, 0o700)
        recorded = {bytes.fromhex(line) for line in history.read_text().splitlines()}
        self.assertEqual(recorded, {b'echo parallel_1', b'echo parallel_2'})
        with history.open('a') as file:
            file.write('bad-hex\n6563686f20666f6f')
        third = self.shell()
        third.send(b'\x12parallel_1\r\r')
        third.output(b'parallel_1\r\n')
        third.expect(b'\x1b[?2004h')
        self.assertNotIn('bad-hex', history.read_text())
        self.assertNotIn('6563686f20666f6f', history.read_text())

    def test_editor_restores_canonical_input_for_read(self):
        sh = self.shell('--vanilla')
        sh.send(b"IFS= read -r value; printf '<%s>\\n' \"$value\"\r")
        sh.expect(b'\x1b[?2004l')
        sh.send(b'left\tright\\side\r')
        sh.expect(b'\r\n<left\tright\\side>\r\n')

    def test_idle_trap_leaves_draft_available(self):
        sh = self.shell('--vanilla')
        sh.send(b"trap 'printf signal-marker\\\\n' USR1\r")
        sh.expect(b'\x1b[?2004l')
        sh.expect(b'\x1b[?2004h')
        sh.send(b'echo par')
        sh.expect(b'$ echo par\r')
        os.kill(sh.pid, signal.SIGUSR1)
        sh.expect(b'signal-marker\r\n')
        sh.expect(b'\x1b[?2004h')
        sh.send(b'tial\r')
        sh.output(b'partial\r\n')

    def test_modules_run_once_and_hooks_are_isolated(self):
        mods = self.config / 'modules'
        mods.mkdir()
        loaded = pathlib.Path(self.tmp, 'loaded')
        events = pathlib.Path(self.tmp, 'events')
        modulescript = f'''printf 'load\\n' >> {loaded}
check_exit() {{
    cd /
    setting=bad
    printf '%s|%s\\n' "$1" "$2" >> {events}
    exit 9
}}
nsh hook add post-command check_exit
'''
        (mods / 'audit.sh').write_text(modulescript)
        conf = self.config / 'conf.sh'
        conf.write_text('setting=good\nnsh module enable audit\n')
        sh = self.shell()
        sh.send(b'false\r')
        sh.expect(b'post-command: check_exit exited with 9')
        sh.send(b"printf '%s|%s\\n' \"$setting\" \"$?\"\r")
        sh.output(b'good|1\r\n')
        sh.expect(b'post-command: check_exit exited with 9')
        sh.send(b'nsh module disable audit; nsh module enable audit; echo enabled\r')
        sh.output(b'enabled\r\n')
        sh.expect(b'post-command: check_exit exited with 9')
        self.assertEqual(loaded.read_text(), 'load\n')
        self.assertIn('false|1', events.read_text().splitlines())

    def test_developer_prompt_precedence_and_palette(self):
        self.env['PS1'] = 'literal> '
        explicit = self.shell('--profile', 'developer', prompt=b'literal> ')
        explicit.send(b"echo develop\ted\r")
        explicit.output(b'developed\r\n')
        self.env.pop('PS1')
        self.env['NO_COLOR'] = '1'
        developer = self.shell('--profile', 'developer', prompt='❯ '.encode())
        developer.send(b'nsh get color.mode\r')
        developer.output(b'auto\r\n')


if __name__ == '__main__':
    unittest.main(verbosity=2)
