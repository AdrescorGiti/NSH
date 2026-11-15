"""Real package and chsh lifecycle for the disposable, unmounted Arch container."""
import errno
import os
from pathlib import Path
import pty
import pwd
import select
import signal
import stat
import subprocess
import time

ACCOUNT = "nshcheck"
UID = 19001
SHELL = "/usr/bin/nsh"
OLD = "/bin/bash"
HOME = Path("/home", ACCOUNT)
ENV = {
    "PATH": "/usr/bin:/bin",
    "HOME": str(HOME),
    "XDG_CONFIG_HOME": str(HOME / "config"),
    "XDG_STATE_HOME": str(HOME / "state"),
    "LC_ALL": "C",
    "TERM": "dumb",
    "USER": "deliberately-wrong",
    "SHELL": "/deliberately/wrong",
}
PASSWORD = b"NSH disposable test password"


def run(*args, user=False, expected=0, stdin=None):
    identity = {"user": UID, "group": UID, "extra_groups": []} if user else {}
    result = subprocess.run(args, input=stdin, env=ENV, capture_output=True,
                            start_new_session=True, timeout=25, **identity)
    if result.returncode != expected:
        raise AssertionError((args, result.returncode, result.stdout, result.stderr))
    return result.stdout + result.stderr


def pty_run(args, replies):
    pid, terminal = pty.fork()
    if pid == 0:
        os.setgroups([])
        os.setgid(UID)
        os.setuid(UID)
        os.chdir(HOME)
        os.execve(args[0], args, ENV)
    remaining = list(replies)
    output = b""
    unmatched = b""
    deadline = time.monotonic() + 15
    reaped = False
    try:
        while True:
            seconds = deadline - time.monotonic()
            if seconds <= 0 or not select.select([terminal], [], [], seconds)[0]:
                raise AssertionError(f"PTY timed out: {output!r}")
            try:
                data = os.read(terminal, 65536)
            except OSError as error:
                if error.errno != errno.EIO:
                    raise
                break
            if not data:
                break
            output += data
            unmatched += data
            if remaining and (offset := unmatched.find(remaining[0][0])) >= 0:
                needle, response = remaining.pop(0)
                unmatched = unmatched[offset + len(needle):]
                os.write(terminal, response)
        while time.monotonic() < deadline:
            child, status = os.waitpid(pid, os.WNOHANG)
            if child:
                reaped = True
                assert not remaining, (remaining[0][0], output)
                return os.waitstatus_to_exitcode(status), output
            time.sleep(0.01)
        raise AssertionError(f"PTY process did not exit: {output!r}")
    finally:
        os.close(terminal)
        if not reaped:
            try:
                os.killpg(pid, signal.SIGKILL)
            except ProcessLookupError:
                # The child may not yet have made itself a session leader.
                try:
                    os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            os.waitpid(pid, 0)


def verify_native_execution():
    script = HOME / "script-without-shebang"
    script.write_bytes(b"""case "$1" in vanilla) printf '%s\\n' native ;; *) exit 19 ;; esac\n""")
    os.chown(script, UID, UID)
    script.chmod(0o700)
    bash = Path("/usr/bin/bash")
    hidden = bash.with_name(".nsh-test-bash")
    assert not hidden.exists()
    mode = stat.S_IMODE(bash.stat().st_mode)
    bash.rename(hidden)
    try:
        hidden.chmod(0o700)
        for interpreter in ("/bin/sh", "/usr/bin/bash", "/usr/bin/dash", "/usr/bin/yash"):
            assert not Path(interpreter).exists()
        assert run(SHELL, "--vanilla", "-c", '"$HOME/script-without-shebang" vanilla',
                   user=True) == b"native\n"
    finally:
        hidden.chmod(mode)
        hidden.rename(bash)
    note("Native parser and ENOEXEC passed with the usual sh interpreters unavailable to the test user.")


def current_shell():
    return pwd.getpwnam(ACCOUNT).pw_shell


def note(text):
    print(text, flush=True)


def verify_install(package, registry, original_shells, system_sh, config, fixture):
    assert not Path(SHELL).exists()
    version = run("/usr/bin/pacman", "-Qp", package).strip()
    run("/usr/bin/pacman", "-U", "--noconfirm", package)
    assert run("/usr/bin/pacman", "-Q", "nsh").strip() == version
    assert run("/usr/bin/pacman", "-Qqo", SHELL).strip() == b"nsh"
    assert run("/usr/bin/pacman", "-Qqo", "/usr/lib/libgcc_s.so.1").strip() == b"libgcc"
    linkage = run("/usr/bin/ldd", SHELL)
    assert b"libgcc_s.so.1 => /usr/lib/libgcc_s.so.1" in linkage and b"not found" not in linkage, linkage
    run("/usr/bin/pacman", "-Qkk", "nsh")
    assert current_shell() == OLD and registry.read_bytes() == original_shells
    assert Path("/bin/sh").readlink() == system_sh and config.read_bytes() == fixture
    assert b"nsh" in run(SHELL, "--version", user=True).lower()
    command = 'printf "%s:%s\\n" "$0" "$1" | cat > "$HOME/result"; cat "$HOME/result"'
    assert run(SHELL, "-c", command, "nsh-check", "arg", user=True) == b"nsh-check:arg\n"
    assert (HOME / "result").read_bytes() == b"nsh-check:arg\n"
    assert run(SHELL, "-s", stdin=b"printf '%s\\n' stdin-script\n", user=True) == b"stdin-script\n"
    executable = HOME / "installed-shebang.sh"
    executable.write_bytes(b"#!/usr/bin/nsh\nprintf '%s\\n' absolute-shebang\n")
    os.chown(executable, UID, UID)
    executable.chmod(0o700)
    assert run(executable, user=True) == b"absolute-shebang\n"
    assert run(SHELL, "-c", '. "$HOME/installed-shebang.sh"', user=True) == b"absolute-shebang\n"
    note(f"Fresh installation of {version.decode()} passed owner, integrity, version, script arguments, pipeline and redirection checks. Account, /bin/sh and config were preserved.")
    return version


def prepare_plugin(config):
    directory = config.parent / "plugins/release-packaging"
    directory.mkdir(parents=True)
    manifest = directory / "plugin.toml"
    script = directory / "init.lua"
    manifest.write_text(
        'name = "release-packaging"\nversion = "1.0.0"\napi_version = 1\n'
        'entry = "init.lua"\ncapabilities = ["shell-state"]\n',
    )
    script.write_text(
        'nsh.command.register("release-lua-check", function(args)\n'
        '  nsh.shell.set("RELEASE_LUA_VALUE", args[1])\n'
        'end)\n',
    )
    for path in (directory.parent, directory, manifest, script):
        os.chown(path, UID, UID)
    return script, script.read_bytes()


def verify_plugins(version, config, fixture, plugin):
    script, contents = plugin
    for example in ("file-suggestions", "syntax-colors", "aurora-theme"):
        supplied = Path("/usr/share/doc/nsh/examples/plugins", example)
        assert (supplied / "plugin.toml").is_file()
        assert (supplied / "init.lua").is_file()
    docs = Path("/usr/share/doc/nsh")
    for page in ("README.md", "CONFIG.md", "PLUGINS.md", "PLUGIN_API.md", "BENCHMARKS.md",
                 "benchmarks/current.md", "benchmarks/current.json"):
        assert (docs / "MANUALS" / page).is_file()
    assert (docs / "benchmarks/shells.svg").is_file()

    assert b"release-packaging\tavailable" in run(SHELL, "plugin", "list", user=True)
    run(SHELL, "plugin", "enable", "release-packaging", user=True)
    assert b"release-packaging\tenabled" in run(SHELL, "plugin", "list", user=True)
    command = "release-lua-check works; printf '%s\\n' \"$RELEASE_LUA_VALUE\""
    assert run(SHELL, "-i", "-c", command, user=True) == b"works\n"
    assert run(SHELL, "--plugins", "-c", command, user=True) == b"works\n"
    run(SHELL, "-c", "release-lua-check test", expected=127, user=True)
    run(SHELL, "--no-plugins", "-i", "-c", "release-lua-check test", expected=127, user=True)
    assert config.read_bytes() == fixture and script.read_bytes() == contents
    run(SHELL, "plugin", "disable", "release-packaging", user=True)
    assert b"release-packaging\tavailable" in run(SHELL, "plugin", "list", user=True)
    note(f"Verified Lua plugin discovery, execution, explicit script loading and recovery on {version.decode()}.")


def verify_removed(registry, original_shells, system_sh, config, fixture):
    run("/usr/bin/pacman", "-R", "--noconfirm", "nsh")
    assert not Path(SHELL).exists()
    assert current_shell() == OLD and config.read_bytes() == fixture
    assert registry.read_bytes() == original_shells and Path("/bin/sh").readlink() == system_sh


def main():
    assert Path("/.dockerenv").is_file() and os.geteuid() == 0
    assert not Path(SHELL).exists()
    registry = Path("/etc/shells")
    original_shells = registry.read_bytes()
    system_sh = Path("/bin/sh").readlink()
    assert SHELL.encode() not in original_shells
    run("/usr/bin/useradd", "-m", "-U", "-u", str(UID), "-s", OLD, ACCOUNT)
    run("/usr/bin/chpasswd", stdin=ACCOUNT.encode() + b":" + PASSWORD + b"\n")
    assert current_shell() == OLD
    note("Created a fresh local non-root account in the disposable Arch container.")

    config = HOME / "config/nsh/conf.sh"
    config.parent.mkdir(parents=True)
    config.write_text("nsh set prompt.symbol 'λ'\n")
    for path in (HOME / "config", config.parent, config):
        os.chown(path, UID, UID)
    fixture = config.read_bytes()
    plugin = prepare_plugin(config)
    initial = Path("/initial.pkg.tar.zst")
    update_path = Path("/update.pkg.tar.zst")
    version = verify_install(initial, registry, original_shells, system_sh, config, fixture)
    if not update_path.exists():
        verify_native_execution()
        verify_plugins(version, config, fixture, plugin)

    switch = [SHELL, "--set-default"]
    checks = run(*switch, "--dry-run", expected=1, user=True)
    assert b"missing from /etc/shells" in checks and current_shell() == OLD
    with registry.open("ab") as file:
        file.write(b"\n" + SHELL.encode() + b"\n")
    checks = run(*switch, "--dry-run", user=True)
    assert b"Checks: passed" in checks and b"/usr/bin/chsh -s '/bin/bash'" in checks
    assert current_shell() == OLD
    assert b"controlling" in run(*switch, expected=1, stdin=b"yes\n", user=True)
    assert current_shell() == OLD
    for reply in (b"no\n", b"\x04"):
        status, output = pty_run(switch, [(b"Type yes", reply)])
        assert status == 1 and b"cancelled" in output and current_shell() == OLD, output
    note("Verified blocked registration, dry-run, no controlling terminal, explicit cancellation and EOF.")

    status, output = pty_run(switch, [(b"Type yes", b"yes\n"), (b"Password:", PASSWORD + b"\n")])
    assert status == 0 and b'Actual login shell: "/usr/bin/nsh"' in output, output
    assert current_shell() == SHELL
    assert b"already the login shell" in run(*switch, user=True)
    result = run("/usr/bin/runuser", "-l", ACCOUNT, "-c", 'printf "LOGIN:%s\\n" "$0"')
    assert b"LOGIN:-nsh\n" in result and result.count(b"LOGIN:") == 1, result
    note("Confirmed through a controlling PTY, authenticated with real chsh/PAM, verified database readback and opened a separate login invocation.")

    if update_path.exists():
        update = run("/usr/bin/pacman", "-Qp", update_path).strip()
        assert update.split()[0] == version.split()[0] == b"nsh"
        assert run("/usr/bin/vercmp", update.split()[1], version.split()[1]).strip() == b"1"
        run("/usr/bin/pacman", "-U", "--noconfirm", update_path)
        assert run("/usr/bin/pacman", "-Q", "nsh").strip() == update
        run("/usr/bin/pacman", "-Qkk", "nsh")
        assert current_shell() == SHELL and config.read_bytes() == fixture
        assert Path("/bin/sh").readlink() == system_sh
        result = run("/usr/bin/runuser", "-l", ACCOUNT, "-c", 'printf "UPGRADED:%s\\n" "$0"')
        assert b"UPGRADED:-nsh\n" in result and result.count(b"UPGRADED:") == 1, result
        note(f"Upgraded {version.decode()} to {update.decode()} with login shell and config preserved, then verified a fresh NSH login.")
        verify_plugins(update, config, fixture, plugin)

    status, output = pty_run(["/usr/bin/chsh", "-s", OLD], [(b"Password:", PASSWORD + b"\n")])
    assert status == 0 and current_shell() == OLD, output
    result = run("/usr/bin/runuser", "-l", ACCOUNT, "-c", 'printf "RESTORED:%s\\n" "$0"')
    assert b"RESTORED:-bash\n" in result, result
    registry.write_bytes(original_shells)
    if update_path.exists():
        run("/usr/bin/pacman", "-U", "--noconfirm", initial)
        assert run("/usr/bin/pacman", "-Q", "nsh").strip() == version
        run("/usr/bin/pacman", "-Qkk", "nsh")
        assert current_shell() == OLD and config.read_bytes() == fixture
        assert plugin[0].read_bytes() == plugin[1]
        note(f"Rolled back the package to {version.decode()} with configuration and plugin files preserved.")
    verify_removed(registry, original_shells, system_sh, config, fixture)
    note("Restored the previous shell with real chsh/PAM, verified a separate login, removed NSH and verified config and system shell preservation.")

    if update_path.exists():
        assert verify_install(update_path, registry, original_shells, system_sh, config, fixture) == update
        verify_native_execution()
        verify_plugins(update, config, fixture, plugin)
        verify_removed(registry, original_shells, system_sh, config, fixture)
        note("Removed the freshly installed update and verified the account and registry again.")


if __name__ == "__main__":
    main()
