<a id="архитектура-nsh"></a>

# NSH architecture

[Русская версия](ARCHITECTURE.ru.md)

<a id="интерпретатор-и-редактор"></a>

## Interpreter and editor

`syntax.rs` parses the byte stream and retains positions for configuration diagnostics. `expand.rs` handles POSIX expansions and IFS. `shell.rs` manages shell state and AST execution; `builtins.rs` implements builtins; `jobs.rs` handles processes, signals and the terminal. `editor.rs` keeps input separate from rendering. `session.rs` defines settings, profiles, history, shell modules and hooks. `session_files.rs` reads user-owned XDG files with directory checks and symbolic links prohibited.

Shell modules manage the settings registry and invoke sh functions through hooks. Managed configuration is applied transactionally.

<a id="механизм-lua"></a>

## Lua integration

`plugin_lua.rs` connects Lua 5.4 to Rust via mlua. Each plugin owns its Lua state and the commands, events and providers registered in it. Replacing a plugin prepares the new registrations before removing the old ones. Priority determines provider order. Dependencies load before the plugins that depend on them.

`plugins.rs` validates manifests and manages the active Lua states. A session without plugins keeps an empty pointer to this set. Normal script mode skips plugin discovery. An interactive session checks the saved enabled selection once. Commands in `conf.sh` prepare changes as part of the current transaction.

The shell invokes Lua commands directly. A command receives a snapshot of the state and accumulates requested changes, which NSH validates before applying. The shell sets up pipes and redirections before invoking the command. In a child context, changes remain in the process for that pipeline stage. The shell remains single-threaded. Long directory scans run in a separate bounded process with nonblocking reads.

`plugin_editor.rs` describes editor events and line changes. The editor applies byte ranges to UTF-8 at grapheme boundaries. Virtual text, highlighting and menus use a common rendering interface. Examples use the public API.

<a id="измерения"></a>

## Measurements

The shell passes full lines from the already-read script buffer to the parser. Complete commands run one at a time, so each subsequent command sees changes to aliases and parser settings. A multiline block is revisited if another buffer is needed. Configuration is read line by line for precise diagnostics. [Measurements](benchmarks/current.md)
