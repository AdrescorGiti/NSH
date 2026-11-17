"""Check SIGTERM of an interactive shell, supplementing tests/jobs_pty.py."""
import os
import pty
import select
import signal
import sys
import time

pid, fd = pty.fork()
if pid == 0:
    os.execve('/opt/nsh/target/release/nsh', ['/opt/nsh/target/release/nsh', '--vanilla'],
              {'PATH': '/usr/bin:/bin', 'HOME': os.environ['HOME'], 'TERM': 'xterm', 'PS1': 'hardtest> '})
try:
    deadline = time.monotonic() + 5
    data = b''
    while b'hardtest> ' not in data:
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([fd], [], [], remaining)[0]:
            raise AssertionError('interactive prompt not observed')
        data += os.read(fd, 4096)
    os.kill(pid, signal.SIGTERM)
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        finished, status = os.waitpid(pid, os.WNOHANG)
        if finished:
            if not os.WIFSIGNALED(status) or os.WTERMSIG(status) != signal.SIGTERM:
                raise AssertionError(f'unexpected status {status}')
            pid = 0
            print('SIGTERM terminated interactive shell')
            break
        time.sleep(0.02)
    else:
        raise AssertionError('interactive shell ignored SIGTERM')
finally:
    os.close(fd)
    if pid:
        os.kill(pid, signal.SIGKILL)
        os.waitpid(pid, 0)
