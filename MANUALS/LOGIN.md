<a id="login-shell-на-linux"></a>

# Login shell on Linux

[Русская версия](LOGIN.ru.md)

The package installs `/usr/bin/nsh` and documentation. Changing the login shell is a separate operation for a local account. NSH identifies the account using the real UID and cross-checks the local entry against system lookups by UID and name. Running as root, mismatched real and effective UIDs or GIDs, an ambiguous entry, or unsuitable file permissions prevent the change.

1. From a working terminal run `/usr/bin/nsh --set-default --dry-run`. NSH checks ownership of the installed file using pacman, dpkg or RPM, availability of `/usr/bin/chsh`, registration of both shells, and whether rollback is possible. If `chsh` is missing, install the system package providing it. System-path directories must be owned and writable only by root. Trusted symbolic links, such as `/bin` on merged-`/usr` systems, are permitted for the previous shell.
2. If the check reports that `/usr/bin/nsh` is absent from `/etc/shells`, coordinate a separate change with the administrator. The administrator adds the exact path on its own line and preserves existing entries. Repeat the dry run.
3. Save the displayed rollback command and keep your current terminal open. Run `/usr/bin/nsh --set-default` to change the shell. The tool accepts `yes` through the controlling terminal. It then rechecks the account and runs `/usr/bin/chsh -s /usr/bin/nsh`. PAM interaction takes place directly through the terminal. NSH reads the changed account entry and verifies the result.
4. Log in again in a separate terminal or session. To test noninteractive invocation, use a tool that runs the shell specified in the account entry. If there is a problem, use the saved rollback command from the original terminal. Running `--set-default` again when NSH is already selected reports the current state without rerunning `chsh`.

<a id="откат-и-удаление"></a>

## Rollback and removal

Run the exact command displayed before the change. If the previous shell was Fish at `/bin/fish`, use:

```sh
/usr/bin/chsh -s '/bin/fish'
```

Check the entry with `getent passwd "$(id -u)"` and test a new login in another terminal. Once the previous shell is restored, remove the package with `sudo pacman -R nsh`, `sudo apt remove nsh` or `sudo dnf remove nsh`, depending on the system. An administrator removes the `/usr/bin/nsh` entry if it was added for NSH and is no longer used. User configuration and history remain in user XDG directories.

Manual pages: [chsh(1)](https://man.archlinux.org/man/chsh.1.en) and [shells(5)](https://man.archlinux.org/man/shells.5.en).
