<a id="настройка-nsh"></a>

# Configuring NSH

[Русская версия](CONFIG.ru.md)

NSH reads `${XDG_CONFIG_HOME:-$HOME/.config}/nsh/conf.sh` at the start of a normal interactive session. Relative XDG variable values are ignored. The file runs in NSH’s own interpreter and may contain `sh` commands, aliases and functions. Changes to the file take effect at the next startup. `nsh set`, `nsh unset`, `nsh bind` and `nsh unbind` change only the current session’s memory. `nsh config check <file>` checks syntax and Markdown fences without running the file. Dynamic values are type-checked when `conf.sh` executes.

<a id="проверка-старта"></a>

### Startup checks

Put ordinary shell text in `conf.sh`. When pasting examples, lines containing Markdown fences such as `~~~sh` are rejected before execution. Fence characters inside quotes or a here-document body remain data. Diagnostics identify the file, line and original command when available.

```sh
nsh config check ~/.config/nsh/conf.sh
```

Automatic configuration receives empty standard input by default. Commands can explicitly redirect input from a file. An `exec program` operation is rejected with a diagnostic while a session applies automatic configuration. Normal interactive input is available once startup completes. A subsequent successful status cannot hide an interpreter command error. Conditions that return an ordinary false status still control branches as in `sh`.

With no saved selection, the built-in `default` profile shows a two-line prompt in a cyan and blue palette. The `vanilla` profile uses a short prompt and history. `nsh --profile developer` adds the path, Git branch name and input highlighting. A one-time flag takes precedence over a saved selection. NSH applies defaults, ancestors from oldest to newest, the selected profile and `conf.sh`, in that order. The first meaningful line of a profile may be `nsh profile inherit developer`. Before startup, the entire chain, names and absence of cycles are checked. User profiles live in `nsh/profiles/` under the configuration directory. `nsh profile create <name>` creates a file; `nsh profile use <name>` atomically saves the selection. Built-in profiles and files in the saved chain cannot be removed with `nsh profile delete`. Select another working chain before deleting an ancestor.

Managed settings, key bindings, hook registrations and module state are applied after the entire profile chain loads successfully. A syntax error, configuration command error or unsuccessful startup file rolls back these changes. A later successful command, including an `|| :` handler, cannot conceal an already detected configuration error. `conf.sh` is applied as a separate stage over the last state saved in memory. If `conf.sh` fails, the shell uses the successfully loaded profile settings or built-in defaults.

Rollback is limited to managed NSH state. File changes and external processes already started remain in effect. Explicit profile save or delete commands write to disk. Ordinary `sh` commands can change variables, functions and the working directory. Configuration errors in a synchronous subshell or pipeline cancel the entire current transaction. Background commands run in their own session copy.

`--vanilla` skips startup files and keeps history in memory. `--posix` uses `ENV` at interactive startup if real and effective IDs match. When these flags are combined, `--vanilla` also skips `ENV`. Ordinary scripts and noninteractive `-c` bypass NSH configuration and history. A separate `nsh get` or `nsh set` process cannot access an already running shell. `config list`, `config describe`, `config check` and saved profile management are also available from a separate process.

Automatically loaded `conf.sh`, profiles and modules must belong to the current user, be regular files and not be group- or world-writable. Directory access is checked component by component using open file descriptors. Symbolic links in these paths are prohibited. A system directory with the sticky bit, such as `/tmp`, is permitted as an ancestor. User code runs with your privileges. For emergency startup, use `nsh --vanilla`.

<a id="реестр"></a>

## Registry

There are 138 keys. `nsh config list` shows the full list; `nsh config describe <key>` shows the type, constraints and default. `nsh get <key>` shows the active value. `nsh unset <key>` restores the default.

| Key | Type and default |
|---|---|
| `color.mode` | `auto`, `always` or `never`. By default `auto` |
| `color.fallback` | `256` or `none`. By default `256` |
| `prompt.style` | `two-line` or `minimal`. By default `two-line` |
| `prompt.template` | Template. By default empty |
| `prompt.symbol` | Printable string. By default empty |
| `prompt.symbol.success` | Printable string for `$? = 0`. By default empty |
| `prompt.symbol.error` | Printable string for `$? != 0`. By default empty |
| `prompt.symbol_color` | HEX. Empty default. An explicit value applies a common symbol color |
| `prompt.symbol.success_color` | HEX `#79DEA4` for status 0 |
| `prompt.symbol.error_color` | HEX `#F87183` for a nonzero status |
| `prompt.user` | Boolean `true` |
| `prompt.user_color` | HEX `#46DBEA` |
| `prompt.host` | Boolean `true` |
| `prompt.host_color` | HEX `#62B8F4` |
| `prompt.path` | Boolean `true` |
| `prompt.path_color` | HEX `#69B7FF` |
| `prompt.path_home` | Boolean `true`. Abbreviates the home path to `~` |
| `prompt.path_max` | Integer `0..4096`, default `0` unlimited |
| `prompt.git` | Boolean `true`. Field requires module `git-prompt` |
| `prompt.git_color` | HEX `#89A6C2` |
| `prompt.git_prefix` | Printable string ` [` |
| `prompt.git_suffix` | Printable string `]` |
| `prompt.git_max` | Integer `0..4096`, default `0` unlimited |
| `prompt.status_color` | HEX. By default empty |
| `cursor.style` | `default`, `block`, `underline` or `bar`. By default `default` |
| `cursor.blink` | Boolean `true`. Applies when a shape is set |
| `editor.bracketed_paste` | Boolean `true` |
| `editor.max_bytes` | Integer `64..1048576`, default `65536` bytes per editor buffer |
| `editor.escape_timeout_ms` | Integer `5..500`, default `35` ms to wait for a key continuation |
| `editor.beep` | Boolean `false` |
| `editor.control_echo` | Boolean `true`. Displays `^C` |
| `editor.search_prompt` | Printable string `history search: ` |
| `editor.ambiguous_width` | `1` or `2` columns for Unicode of ambiguous width. By default `1` |
| `history.enabled` | Boolean `true`. Controls recording and searching |
| `history.persistent` | Boolean `true`. Controls reading and writing to disk |
| `history.max_size` | Integer `0..50000`, default `2000` entries |
| `history.ignore_duplicates` | Boolean `false`. Compares adjacent entries |
| `history.ignore_space` | Boolean `false`. Ignores lines starting with a space |
| `history.search_ignore_case` | Boolean `false` |
| `highlighting.command` | HEX `#8DBEFF` |
| `highlighting.argument` | HEX. By default empty |
| `highlighting.single_quote` | HEX `#98D293` |
| `highlighting.double_quote` | HEX `#98D293` |
| `highlighting.variable` | HEX `#E9B980` |
| `highlighting.operator` | HEX `#BBA2EE` |
| `highlighting.redirect` | HEX `#FFAB8C` |
| `highlighting.number` | HEX `#DFCFA4` |
| `highlighting.assignment` | HEX `#ADC7CF` |
| `highlighting.comment` | HEX `#8A96A0` |
| `highlighting.escape` | HEX `#BBA2EE` |
| `highlighting.glob` | HEX `#E9B980` |

HEX has the form `'#RRGGBB'`. An empty value disables the corresponding element’s color. `NO_COLOR`, `TERM=dumb`, an unset `TERM`, or redirected stdin or stderr disable NSH colors in every mode. True RGB is selected by `COLORTERM=truecolor`, `COLORTERM=24bit` or `direct` in `TERM`. For `*-256color`, conversion to a 256-color palette is supported. `always` selects RGB on a color terminal; `never` disables color. Highlighting distinguishes types of already entered text and does not require looking up programs in `PATH`.

In the two-line prompt, the first line contains `user@host`, the path and Git branch; the second contains `❯`. Success colors the symbol green and failure colors it red. `prompt.user`, `prompt.host`, `prompt.path` and `prompt.git` control fields independently. Enable `nsh module enable git-prompt` for the Git branch. By default the two-line style displays the branch name without surrounding text; templates and explicit settings can add a prefix and suffix. Without a custom template, the legacy `prompt.symbol` and `prompt.path_color` keys apply to the two-line prompt. Explicit status colors take precedence over `prompt.symbol_color`.

The template uses `{cwd}`, `{git}`, `{symbol}`, `{status}`, `{uid}`, `{user}` and `{host}` fields. A newline in the template creates a new prompt line. `{{` and `}}` represent braces. Control characters in paths and branches are escaped, and field values are inserted as text. `{git}` is empty if Git is unavailable, outside a repository or in detached HEAD. An explicit `PS1` takes precedence over the template. `PS2` is the continuation prompt. Colored SGR sequences in `PS1` and `PS2` have zero rendering width.

```sh
nsh set color.mode auto
nsh set prompt.template '{cwd}{git} {symbol} '
nsh set prompt.symbol 'λ'
nsh set prompt.symbol.success '❯'
nsh set prompt.symbol.error '⚡'
nsh set prompt.path_color '#8DBEFF'
nsh set prompt.git false
nsh set cursor.style bar
nsh set history.max_size 10000
```

<a id="клавиши"></a>

### Keys

All 87 `key.<key>` keys accept an action name. `nsh bindings list` shows active bindings. `nsh unbind` assigns `none`; `nsh unset key.Tab` restores the default. In both supplied profiles, `Tab` has the `none` action. The commands do not provide lists of choices or history text while typing.

| Keys | Defaults |
|---|---|
| `Up`/`Down`, `Ctrl-P`/`Ctrl-N` | `history-previous`/`history-next` |
| `Left`/`Right`, `Ctrl-B`/`Ctrl-F` | `move-left`/`move-right` |
| `Home`/`End`, `Ctrl-A`/`Ctrl-E` | `move-home`/`move-end` |
| `Backspace`/`Delete` | `delete-left`/`delete-right` |
| `Enter`, `Ctrl-J` | `accept-line` |
| `Ctrl-C`/`Ctrl-D` | `interrupt`/`eof` |
| `Ctrl-L`/`Ctrl-U`/`Ctrl-W` | `clear-screen`/`clear-line`/`delete-word` |
| `Ctrl-R` | `history-search` |
| `Ctrl-Left`/`Ctrl-Right`, `Alt-Left`/`Alt-Right`, `Alt-b`/`Alt-f` | `move-word-left`/`move-word-right` |
| `Tab`, `Escape`, `PageUp`, `PageDown`, `Insert`, `Ctrl-Up`, `Ctrl-Down`, `Alt-Up`, `Alt-Down`, `Shift-Left`, `Shift-Right`, `Shift-Up`, `Shift-Down`, `F1`–`F12` | `none` |
| `Ctrl-G`, `Ctrl-K`, `Ctrl-O`, `Ctrl-Q`, `Ctrl-S`, `Ctrl-T`, `Ctrl-V`, `Ctrl-X`, `Ctrl-Y`, `Ctrl-Z` | `none` |
| `Alt-a`–`Alt-z`, except `Alt-b` and `Alt-f` | `none` |

The editor also provides the `insert-newline` action. Key names represent common xterm sequences. Some combinations may be intercepted by the terminal or desktop environment. `Backspace` supports DEL and Ctrl-H. Keys that send the same sequence share a binding.

```sh
nsh bind Up history-previous
nsh bind F8 insert-newline
nsh set key.Alt-r history-search
```

Moving down out of history restores the original draft and cursor. Ctrl-R starts an explicit search. Enter a query and press Ctrl-R again to go to the previous match. Enter accepts the found line for editing; Escape restores the draft. Ctrl-D ends the session on an empty line and deletes the next character on a nonempty line. Editor settings apply when stdin and stderr are TTYs and `TERM` works.

History is stored in `${XDG_STATE_HOME:-$HOME/.local/state}/nsh/history`. NSH creates the directory with `0700` permissions and the file with `0600`. Lines containing newlines are written in full as hex records ending in a newline. The file is updated atomically under a directory lock so concurrent sessions read the latest shared version before writing. Corrupt or incomplete records are skipped. Storage is limited to 32 MiB. On a read or write error, the current session keeps history in memory. `nsh module disable history` disables both recording and navigation.

<a id="модули-и-hooks"></a>

## Modules and hooks

`nsh module list` lists built-in and available user modules. `nsh module enable <name>` activates a module. A user `nsh/modules/<name>.sh` is executed on its first explicit enable. Loading order follows the order of `enable` calls. `nsh module disable <name>` disables associated hooks and built-in visual components. Re-enabling a successfully loaded module restores its hooks without rerunning the file. Functions, variables and external effects already created remain after disabling. Profile and module names are limited to 1–64 ASCII letters, digits, `_` and `-`.

| Hook | When | Arguments |
|---|---|---|
| `startup` | After startup files, before the first command | — |
| `before-prompt` | Before the primary prompt | — |
| `pre-command` | After a complete command is accepted, before execution | Command text |
| `directory-changed` | After a command changes the working directory | New path, previous path |
| `post-command` | After a user command | Command text, exit status |
| `exit` | Before NSH exits | Exit status |

`nsh hook add <event> <function>` appends a handler. `nsh hook remove <event> <function>` unregisters it. `nsh hook list` lists active handlers. A handler receives arguments as values and runs in a separate shell context. Its variables, `cd` and exit do not change the parent session. Handlers are not added to history, and their statuses do not replace the user command’s status.

```sh
on_directory_changed() {
    printf 'Directory: %s\n' "$1" >&2
}
nsh hook add directory-changed on_directory_changed
```


<a id="плагины-lua"></a>

## Lua plugins

`nsh plugin enable NAME` in configuration defines the active set for the current session. Plugins load from `${XDG_CONFIG_HOME:-$HOME/.config}/nsh/plugins/` after a successful transaction. Manage the saved set with external `command nsh plugin enable NAME` or `disable`. `--no-plugins` keeps the NSH editor and normal prompt settings available. Shell modules managed by `nsh module` and existing shortcuts retain their prior API.

[Plugin installation](PLUGINS.md) · [Lua API](PLUGIN_API.md)
