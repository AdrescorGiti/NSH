"""Controlling-terminal tests with strict read timeouts and process-group cleanup."""
import errno
import fcntl
import os
from pathlib import Path
import pty
import select
import signal
import struct
import sys
import termios
import tempfile
import time
import unittest

BIN = str(Path(sys.argv.pop(1)).resolve())


class Terminal:
    def __enter__(self):
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            env = {"PATH": "/usr/bin:/bin", "TERM": "xterm-256color", "LC_ALL": "C.UTF-8", "PS1": "nsh> "}
            os.execve(BIN, [BIN, "--vanilla"], env)
        self.pending = b""
        self.prompt()
        return self

    def __exit__(self, *exc):
        if self.fd is None:
            return
        try:
            os.write(self.fd, b"\x03")
            os.write(self.fd, b"exit 0\r")
        except OSError:
            pass
        os.close(self.fd)
        limit = time.monotonic() + 2
        while time.monotonic() < limit:
            if os.waitpid(self.pid, os.WNOHANG)[0]:
                return
            time.sleep(0.02)
        os.kill(self.pid, signal.SIGKILL)
        os.waitpid(self.pid, 0)

    def expect(self, needle):
        deadline = time.monotonic() + 4
        while needle not in self.pending:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not select.select([self.fd], [], [], remaining)[0]:
                raise AssertionError(f"missing {needle!r} in {self.pending[-5000:]!r}")
            try:
                data = os.read(self.fd, 8192)
            except OSError as e:
                if e.errno != errno.EIO:
                    raise
                data = b""
            if not data:
                raise AssertionError(f"PTY ended waiting for {needle!r}: {self.pending[-5000:]!r}")
            self.pending += data
        end = self.pending.index(needle) + len(needle)
        output, self.pending = self.pending[:end], self.pending[end:]
        return output

    def prompt(self):
        self.expect(b"\x1b[?2004h")
        return self.expect(b"nsh> ")

    def send(self, data):
        os.write(self.fd, data)

    def shell_foreground(self):
        limit = time.monotonic() + 4
        while time.monotonic() < limit:
            if os.tcgetpgrp(self.fd) == self.pid:
                return
            time.sleep(0.01)
        raise AssertionError("shell did not regain terminal")

    def foreground(self):
        limit = time.monotonic() + 4
        while time.monotonic() < limit:
            group = os.tcgetpgrp(self.fd)
            if group != self.pid:
                return group
            time.sleep(0.02)
        while select.select([self.fd], [], [], 0)[0]:
            self.pending += os.read(self.fd, 8192)
        raise AssertionError(f"job did not obtain terminal: {self.pending[-2048:]!r}")


class Jobs(unittest.TestCase):
    def test_pipeline_interrupt_restores_foreground(self):
        with Terminal() as t:
            t.send(b"sleep 30 | cat\r")
            t.foreground()
            t.send(b"\x03")
            t.prompt()
            t.shell_foreground()
            t.send(b"echo status=$?\r")
            t.expect(b"status=130")
            t.prompt()
            t.send(b"\x03")
            t.prompt()
            t.send(b"echo 'still running'\r")
            t.expect(b"still running\r\n")

    def test_suspend_continue_foreground_wait(self):
        with Terminal() as t:
            t.send(b"sleep 30 | cat\r")
            group = t.foreground()
            t.send(b"\x1a")
            t.expect(b"Stopped")
            t.prompt()
            t.send(b"jobs\r")
            t.expect(b"Stopped")
            t.prompt()
            t.send(b"bg\r")
            t.prompt()
            t.shell_foreground()
            t.send(b"fg\r")
            self.assertEqual(t.foreground(), group)
            t.send(b"\x03")
            t.prompt()
            t.send(b"sleep .01 & wait $!; echo wait=$?\r")
            t.expect(b"wait=0")

    def test_terminal_hangup_notifies_background_group(self):
        with Terminal() as t, tempfile.TemporaryDirectory() as tmp:
            marker = os.fsencode(str(Path(tmp, "hangup")))
            ready = marker + b".ready"
            command = (
                b"{ trap 'echo HUP > " + marker + b"; exit' HUP; "
                b"echo ready > " + ready + b"; sleep 30; } &"
            )
            t.send(b"\x1b[200~" + command + b"\x1b[201~\r")
            t.prompt()
            deadline = time.monotonic() + 3
            while not os.path.isfile(ready) and time.monotonic() < deadline:
                time.sleep(.01)
            self.assertTrue(os.path.isfile(ready))
            os.close(t.fd)
            t.fd = None
            deadline = time.monotonic() + 4
            shell_exit = None
            while time.monotonic() < deadline:
                if shell_exit is None:
                    pid, status = os.waitpid(t.pid, os.WNOHANG)
                    if pid:
                        shell_exit = status
                if shell_exit is not None and os.path.isfile(marker):
                    break
                time.sleep(.01)
            if shell_exit is None:
                os.kill(t.pid, signal.SIGKILL)
                os.waitpid(t.pid, 0)
                self.fail("shell did not exit after hangup")
            self.assertEqual(Path(os.fsdecode(marker)).read_bytes(), b"HUP\n")

    def test_resize_and_terminal_mode(self):
        with Terminal() as t:
            fcntl.ioctl(t.fd, termios.TIOCSWINSZ, struct.pack("HHHH", 12, 20, 0, 0))
            t.send(("printf '%s\\n' '" + "⚡λe\u0301" * 18 + "'\r").encode())
            t.expect(("⚡λe\u0301" * 18 + "\r\n").encode())
            t.prompt()
            t.send(b"stty -echo; stty -a; echo MODE-DONE\r")
            out = t.expect(b"MODE-DONE\r\n")
            self.assertIn(b" echo ", out)
            self.assertNotIn(b" -echo ", out)


if __name__ == "__main__":
    unittest.main()
