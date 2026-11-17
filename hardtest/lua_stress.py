"""Exercise real plugin activation limits and recovery through the NSH CLI."""
from pathlib import Path
import os
import subprocess
import tempfile

BIN = '/opt/nsh/target/release/nsh'


def attempt(source, diagnostic):
    with tempfile.TemporaryDirectory() as root:
        home = Path(root)
        plugin = home / 'config/nsh/plugins/fixture'
        plugin.mkdir(parents=True)
        (plugin / 'plugin.toml').write_text('name = "fixture"\nversion = "1.0.0"\napi_version = 1\nentry = "init.lua"\n')
        (plugin / 'init.lua').write_text(source)
        (home / 'config/nsh/conf.sh').write_text('nsh plugin enable fixture\n')
        env = {'PATH': '/usr/bin:/bin', 'HOME': root, 'TERM': 'dumb', 'LC_ALL': 'C.UTF-8',
               'XDG_CONFIG_HOME': str(home / 'config'), 'XDG_STATE_HOME': str(home / 'state')}
        result = subprocess.run([BIN, '-ic', "printf 'recovered\\n'"], env=env, cwd=root,
                                capture_output=True, timeout=10)
        if result.returncode != 0 or result.stdout != b'recovered\n' or diagnostic not in result.stderr.lower():
            raise AssertionError(f'status={result.returncode}, stdout={result.stdout!r}, stderr={result.stderr!r}')


if __name__ == '__main__':
    attempt('while true do end', b'budget exceeded')
    attempt("local x=string.rep('x', 10*1024*1024)", b'memory')
    print('Lua instruction and heap budgets rejected; shell recovered')
