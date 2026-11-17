<a id="плагины-nsh"></a>

# NSH plugins

[Русская версия](PLUGINS.ru.md)

Lua plugins live in `${XDG_CONFIG_HOME:-$HOME/.config}/nsh/plugins/`. Each directory contains `plugin.toml` and an entry file. NSH loads explicitly selected plugins and their declared dependencies. Ownership and permissions are checked along the entire path. Plugin files must belong to the current user and must not be group- or world-writable. Symbolic links in the plugin directory are rejected.

<a id="подключение-примеров"></a>

## Installing the examples

Copy the desired directories from `examples/plugins/` or `/usr/share/doc/nsh/examples/plugins/` to your plugin directory. Run `nsh plugin list` to see their names.

- `file-suggestions` shows one gray suffix for an existing path; Right accepts it. Lua selects a candidate and escapes spaces and Unicode appropriately. Rust provides an asynchronous directory snapshot and renders the text.
- `syntax-colors` highlights commands, arguments, strings, variables, paths and operators. Its palette is defined at the beginning of `init.lua`. Command lookups are cached briefly by directory and PATH.
- `custom-commands` adds `hello` and `cdev`. The latter changes the current session directory to `$HOME/dev`.
- [`aurora-theme`](AURORA_THEME.md) provides a two-line prompt and opens a command menu with Tab. Colors are changed in `plugin.toml`.

Add the enable command to `conf.sh` or run it in interactive NSH.

~~~sh
nsh plugin enable file-suggestions
nsh plugin enable syntax-colors
nsh plugin enable custom-commands
hello user
~~~

In a session, enable changes its active set. In `conf.sh`, plugin settings participate in the transaction. At startup configuration commands are applied first, then plugins from the file are prepared and activated. To explicitly persist a selection for later startups, run external `nsh plugin enable NAME` from another shell, or `command nsh plugin enable NAME` from NSH. External `disable` changes the saved selection in the same way. The `plugins/enabled` file is updated atomically.

<a id="управление"></a>

## Management

| Command | Action |
|---|---|
| `nsh plugin list` | Show discovered and selected plugins. |
| `nsh plugin info NAME` | Show metadata, compatibility and capabilities. |
| `nsh plugin enable NAME` | Enable; dependencies are enabled automatically. |
| `nsh plugin disable NAME` | Disable an explicit selection. A dependency remains active while used by another plugin. |
| `nsh plugin reload NAME` | Prepare a new Lua environment in the session, then replace the old registrations. On error the old plugin continues to work. |
| `nsh plugin doctor` | Check directories, manifests and dependencies without executing Lua. |

`command nsh` and an absolute path to the binary invoke the executable. External `reload` reports that it needs the session owning the plugin. Ordinary POSIX scripts use the built-in interpreter. Explicit `--plugins` activates the saved set for a script. Use `nsh --no-plugins` for recovery: it skips saved plugins and startup enable directives. `--vanilla` and `--posix` skip them too.

<a id="манифест-и-приоритет"></a>

## Manifest and priority

~~~toml
name = "demo"
version = "1.0.0"
api_version = 1
entry = "init.lua"
description = "Commands for the current session"
author = "Local user"
dependencies = ["common@^1.0", "palette"]
capabilities = ["shell-state", "stdio"]
priority = 10
nsh_version = ">=0.2.1, <0.3.0"

[options]
greeting = "Hello"
~~~

The name matches the directory name. ASCII letters, digits, hyphens and underscores are allowed. `entry` is a relative path without parent components. Version, API and dependency manifests are checked before initialization. Apart from dependencies, lower priority loads earlier. When selecting a provider, higher priority wins, then name. Registrations with the same name require explicit replacement with the `override` permission. A special POSIX builtin can be replaced only with `override` and `special-builtins`. The system configuration command `nsh` is reserved.

Plugins are executable user code. Permissions determine access to NSH operations. Untrusted code requires separate OS isolation. A callback error reports the plugin name, file and Lua line when available. Lua limits help with loops or memory exhaustion inside the VM. I/O and external programs can have their own delays and side effects.

The `[options]` section is available to the plugin through `nsh.config.get`. This section and `nsh.require` require NSH 0.2.1 or later. After changing options, run `nsh plugin reload NAME` inside the session.

Creating handlers, replacing line ranges and working with history are described in [Lua API 1](PLUGIN_API.md). For measurements, use the [performance workloads](PERFORMANCE.md).
