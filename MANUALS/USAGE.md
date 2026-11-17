<a id="использование-nsh"></a>

# Using NSH

[Русская версия](USAGE.ru.md)

| Invocation | Behavior |
|---|---|
| `nsh` | Reads stdin. By default, interactive mode requires a TTY on stdin and stderr. |
| `nsh -i` | Forces interactive mode. |
| `nsh -c 'commands' name arguments…` | Executes a command string. The name becomes `$0`. |
| `nsh -s arguments…` | Reads commands from stdin. Operands become positional parameters. |
| `nsh -n script.sh` | Parses syntax without executing it. |
| `nsh -l` / `nsh --login` | Loads login files; also enabled by a leading `-` in `argv[0]`. |
| `nsh --posix` | Disables NSH appearance settings and configuration. Interactive startup uses the POSIX `ENV` variable when real and effective UID/GID match. |
| `nsh --vanilla` | Skips startup files. Interactive history remains in memory. |
| `nsh --no-plugins` | Keeps normal session configuration but skips Lua plugins. |
| `nsh --plugins -c 'commands'` | Explicitly loads the saved Lua plugin selection for a separate process. |

`--` ends options before an operand. After a `-c` string, `$0` can literally be `--`. Login mode loads `/etc/profile`, then `$HOME/.profile` if real and effective IDs match. A normal interactive session then loads the NSH profile and [configuration](CONFIG.md).

<a id="скрипты"></a>

## Scripts

- `nsh script.sh` executes a file through NSH. For direct execution, use `#!/usr/bin/env nsh` or an absolute path to NSH.
- When a file with `#!/bin/sh` is executed directly, the kernel uses the interpreter in the shebang.
- When `execve` returns `ENOEXEC` for an executable text file without a shebang, NSH parses the file itself.

Inside NSH, `nsh` accesses built-in settings for the active session. Use `command nsh` or an explicit absolute path to launch a separate process. See [checking and changing the login shell](LOGIN.md).
