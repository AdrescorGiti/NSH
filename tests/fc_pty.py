#!/usr/bin/env python3
"""Test command-history listing, replay and editing with a controlling PTY."""
import os
import pathlib
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
from session_pty import Shell


class HistoryCommands(unittest.TestCase):
    def setUp(self):
        self.directory = self.enterContext(tempfile.TemporaryDirectory(prefix='nsh-fc-'))
        env = {
            'PATH': '/usr/bin:/bin', 'HOME': self.directory, 'TERM': 'xterm',
            'LANG': 'C.UTF-8', 'PS1': 'fc> ',
        }
        self.shell = Shell(env, '--vanilla')
        self.addCleanup(self.shell.close)
        self.shell.expect(b'fc> ')

    def enter(self, code, result):
        self.shell.send(code + b'\r')
        self.shell.output(result)
        self.shell.expect(b'\x1b[?2004h')

    def test_list_and_substitute(self):
        self.enter(b'echo one', b'one\r\n')
        self.enter(b'echo two', b'two\r\n')
        self.enter(b'fc -lnr 1 2', b'\techo two\r\n\techo one\r\n')
        self.enter(b"fc -s one=three 'echo one'", b'three\r\n')
        self.enter(b'fc -ln -1', b'\techo three\r\n')

    def test_editor_is_invoked_with_private_file_and_reexecuted(self):
        editor = pathlib.Path(self.directory, 'replace.py')
        editor.write_text('''#!/usr/bin/python3
import pathlib, sys
file = pathlib.Path(sys.argv[1])
assert file.parent.stat().st_mode & 0o777 == 0o700
assert file.stat().st_mode & 0o777 == 0o600
assert file.read_bytes() == b'echo original\\n'
file.write_bytes(b'echo edited\\n')
''')
        os.chmod(editor, 0o700)
        self.enter(b'echo original', b'original\r\n')
        self.enter(b'fc -e ' + os.fsencode(editor), b'edited\r\n')
        self.enter(b'fc -ln -1', b'\techo edited\r\n')


if __name__ == '__main__':
    unittest.main(verbosity=2)
