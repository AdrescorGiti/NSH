# Lua API 1

[Русская версия](PLUGIN_API.ru.md)

NSH embeds Lua 5.4 through mlua. The API is selected by the integer field `api_version = 1` in the manifest; incompatible changes increment the API number. The new features in 0.2.1 use the same API. To load a plugin using new calls only in a compatible shell, specify `nsh_version = ">=0.2.1, <0.3.0"`. Versions are available as `nsh.version` and `nsh.api_version`. [mlua](https://docs.rs/mlua/0.11.6/mlua/)

<a id="команды"></a>

## Commands

Create `~/.config/nsh/plugins/example/` and put `plugin.toml` in it.

```toml
name = "example"
version = "1.0.0"
api_version = 1
entry = "init.lua"
description = "Example shell command"
author = "Local user"
capabilities = ["shell-state"]
```

Create `init.lua` in the same directory.

```lua
nsh.command.register("hello", function(args)
    nsh.print("Hello", args[1] or "Lua")
    return 0
end)

nsh.command.register("cdev", function(args)
    local home = nsh.env.get("HOME")
    nsh.shell.cd(home .. "/dev")
    nsh.shell.set("LAST_DEV_ARG", args[1] or "")
    return 0
end)
```

Add `nsh plugin enable example` to `conf.sh`, or run it in an active NSH session. The handler receives a 1-indexed array of arguments after shell expansion, without the command name. Argument and variable strings preserve bytes. `nil` returns status 0. A numeric status is converted to the 0–255 range. State changes made by an executed command are visible to the shell; in a pipeline stage they remain in its child context.

| Method | Action |
|---|---|
| `nsh.command.register(name, fn)` | Registers a new command at load time. |
| `nsh.command.replace(name, fn)` | Requires `override`. Registers a command replacement. Special POSIX builtins are also subject to the shell permission policy. |
| `nsh.command.wrap(name, fn)` | Requires `override`. Calls `fn(args, status)` after the original; `nil` preserves the original status. |
| `nsh.command.unregister(name)` | Removes the plugin’s own command and returns whether it was registered. |
| `nsh.command.exists(name)` | Checks current commands, builtins and PATH lookup. The highlighting cache considers directory, PATH and a two-second lifetime. |
| `nsh.print(...)` | Writes simple values to stdout separated by tabs and followed by a newline. |
| `nsh.stdout.write(...)`, `nsh.stderr.write(...)` | Write byte strings directly. Require `stdio`. |
| `nsh.stdin.read_line()` | Reads a line without the trailing LF, up to 64 KiB. Returns `nil` at EOF. Requires `stdio`. |
| `nsh.stdin.read(max_bytes)` | Reads up to `max_bytes` bytes in one call, range 1 to 65536. Preserves NUL, newlines and arbitrary bytes; a short read is possible. Returns `nil` at EOF. Requires `stdio`. |
| `nsh.shell.get(name)`, `nsh.env.get(name)` | Read a shell variable or exported environment variable. |
| `nsh.shell.set(name, value)`, `nsh.env.set(name, value)` | Change a shell or exported variable. `nil` removes the value. Require `shell-state`. |
| `nsh.shell.cwd()`, `nsh.shell.status()` | Current directory and previous shell status. |
| `nsh.shell.cd(path)` | Queues a directory change and updates the snapshot for the current handler. Requires `shell-state`. |
| `nsh.process.run(program, args)` | Executes an external ELF or a file with a shebang using inherited redirections; returns its status. Requires `process`. For a text script without a shebang, specify NSH as the program and pass the script path as an argument. |
| `nsh.process.exec(program, args)` | The handler’s final effect. Requires `exec`. Replaces the current process. After a transition to Bash, the Lua engine and NSH plugins no longer participate in the session. |

`nsh.process.run` is synchronous. The child uses the current process group. To create a separate interactive job, invoke an external command through normal NSH syntax. Explicit `nsh.process.exec` runs after command effects have been checked, outside user configuration loading.

<a id="настройки-и-файлы-плагина"></a>

## Plugin settings and files

Add options to `plugin.toml` that users can change before `nsh plugin reload NAME`.

~~~toml
[options]
greeting = "Hello"
targets = ["NSH", "Lua"]
~~~

Create `lib/messages.lua` inside your plugin directory.

~~~lua
local messages = {}

function messages.hello(name)
    return nsh.config.get("greeting", "Hello") .. ", " .. name
end

return messages
~~~

Load it in `init.lua` and replace the previous `hello` handler.

~~~lua
local messages = nsh.require("lib.messages")

nsh.command.register("hello", function(args)
    local targets = nsh.config.get("targets", {"NSH"})
    nsh.print(messages.hello(args[1] or targets[1]))
    return 0
end)
~~~

`nsh.config.get()` returns a copy of `[options]`. A call with a top-level key returns that value; with a key and second argument it returns the default for a missing key. TOML arrays are indexed from 1, and dates are returned as strings. Changing the returned table affects only Lua.

`nsh.require("lib.messages")` reads `lib/messages.lua` within the current plugin. Trusted Lua source in regular files is permitted. The name is limited to 128 bytes and consists of dot-separated parts containing ASCII letters, digits, `_` and `-`. Lua caches the returned value for the current load. A module with `return nil` or no `return` returns `true`. First imports, including nested imports, run during plugin initialization. Already loaded modules are available to handlers. On reload NSH creates a new Lua cache. A plugin can load up to 64 modules of up to 1 MiB each.

<a id="жизненный-цикл"></a>

## Lifecycle

`nsh.on(event, fn)` and `nsh.events.on(event, fn)` register one event. `fn(args)` receives an array of strings. Available names include `load`, `enable`, `disable`, `reload`, `unload`, `error`, `shutdown`, `startup`, `before-prompt`, `pre-command`, `before-dispatch`, `after-dispatch`, `post-command`, `directory-changed` and `exit`.

Commands, events and providers are registered in top-level `init.lua`. Current session data is available to handlers. Top-level initialization code reads the inherited process environment. Lua has an 8 MiB memory limit per VM and an instruction budget. Proposed state changes are collected during a successful call. Output and explicitly launched processes are immediate side effects.

<a id="редактор-меню-и-история"></a>

## Editor, menus and history

The editor provides a UTF-8 string and a **zero-based byte offset** for the cursor. Replacement and cursor positions must lie on grapheme boundaries. For example, positions 0 and 2 are valid for `λ`.

| Method | Action |
|---|---|
| `nsh.editor.on_change(fn)` | Calls `fn(line, cursor)` on input changes. |
| `nsh.editor.on_tick(fn)` | Calls `fn(line, cursor)` during a background editor request. Without a tick handler, the change handler is called. |
| `nsh.editor.line()`, `nsh.editor.cursor()` | Return the current frame state. |
| `nsh.editor.set_line(line[, cursor])` | Replaces the entire line; by default the cursor moves to its end. |
| `nsh.editor.replace(start, end, text)`, `nsh.editor.set_cursor(offset)` | Change text or cursor position. `end` is exclusive. |
| `nsh.editor.set_ghost_text(text[, style])` | Draws a suggestion. `nil` clears it. Accepting a suggestion requires an explicit replacement. |
| `nsh.editor.set_virtual_text(items)` | Accepts `{offset, text, style}` items. The `color` field retains HEX support. |
| `nsh.editor.set_highlights(items)` | Accepts `{start, ["end"], style}` ranges. The `color` field retains HEX support. |
| `nsh.editor.set_menu(items[, options])` | Shows a list of strings or `{text, style}` items. Pass `nil` to hide the menu. |
| `nsh.keys.bind(key, fn)`, `nsh.keys.wrap(key, fn)` | Call `fn(key, default_action)`. `true` handles the key; `nil` or `false` keeps the default action. |
| `nsh.prompt.register(fn)` | `fn(default_prompt, status)` returns a string, list of `{text, style}` items or `nil`. |
| `nsh.highlight.register(fn)` | `fn(line, cursor)` returns a table of highlight ranges or `nil`. |
| `nsh.completion.register(fn)` | On Tab, `fn(line, cursor)` returns menu items, an `{items, selected, style, selected_style}` table or `nil`. |
| `nsh.history.entries()`, `nsh.history.register(fn)` | Read history. The `fn(default_entries)` provider returns up to 512 entries totaling up to 8 MiB, or `nil`. |
| `nsh.fs.read_dir(path, prefix?)`, `nsh.fs.request(path, prefix?)` | Asynchronous directory snapshot. Returns an array of `{name, is_dir}`, `nil` while a read is pending, or `nil, error`. Requires `fs`. |

The example inserts the visible suffix only after Right Arrow.

```lua
local ghost

nsh.editor.on_change(function(line, cursor)
    ghost = nil
    if line == "echo h" and cursor == #line then
        ghost = "ello"
    end
    nsh.editor.set_ghost_text(ghost)
end)

nsh.keys.bind("Right", function()
    local offset = nsh.editor.cursor()
    if ghost and offset == #nsh.editor.line() then
        nsh.editor.replace(offset, offset, ghost)
        nsh.editor.set_cursor(offset + #ghost)
        ghost = nil
        return true
    end
    return false
end)

nsh.highlight.register(function(line)
    local command = line:match("^([%a_][^%s]*)")
    if not command then return nil end
    local color = nsh.command.exists(command) and "#79DEA4" or "#F87183"
    return {{start = 0, ["end"] = #command, color = color}}
end)
```

Complete Lua examples are in `examples/plugins/`, including actual file lookup. Editor providers have read access to the shell snapshot. Synchronous command execution and stdin reads are available from ordinary command or event handlers so editor input is not blocked.

<a id="стили-редактора"></a>

## Editor styles

For `style`, pass a foreground color `"#RRGGBB"`, a style name, the built-in `"dim"`, or a table containing `fg`, `bg` and boolean fields `bold`, `dim`, `italic`, `underline`, `inverse`. In highlight ranges and virtual text, `style` takes precedence over `color`.

`nsh.style.define(name, style)` saves or replaces a style in the current plugin’s palette. `nsh.style.defined(name)` checks a palette name; `nsh.style.remove(name)` removes it and reports whether it existed. Names allow up to 64 ASCII letters, digits and `-_.` characters. A palette holds up to 512 names.

~~~lua
nsh.style.define("accent", {
    fg = "#81E6D9", bg = "#152C45", bold = true,
})

nsh.prompt.register(function(_, status)
    return {
        {text = " NSH ", style = "accent"},
        {text = status == 0 and " ❯ " or " ✗ ", style = "#FFA6B6"},
    }
end)

nsh.completion.register(function()
    return {
        items = {"first", {text = "second", style = "#BCD0FF"}},
        selected = 2,
        style = "#9EB1C8",
        selected_style = "accent",
    }
end)
~~~

A menu’s `selected` index starts at 1. NSH displays up to eight rows. A plugin can bind selection and insertion to keys with `nsh.keys.bind` and `nsh.editor.replace`. Structured styles respect `NO_COLOR`, `TERM`, `color.mode` settings and terminal capabilities. In 256-color mode NSH converts RGB to the palette. If parts of one grapheme fall in different prompt segments, the style of its first character is used.

<a id="границы-api"></a>

## API boundaries

The built-in Lua standard libraries include table, string, utf8 and math. Lua filesystem and system libraries, raw loaders, coroutines and protected calls are not exposed. These limits reduce accidental blocking. A plugin with `stdio`, `process`, `fs`, `shell-state` or `exec` capabilities remains executable user code. API permissions govern access to NSH operations. Process and filesystem isolation remain the OS’s responsibility.

The terminal profile and font are configured through the terminal’s own interface. For example, a plugin can invoke an external Konsole client with arguments and the `process` permission.

`before-dispatch` receives the command name and its already expanded arguments. `after-dispatch` receives the exit status, name and arguments. These are observable stages. The actual invocation sees environment and directory changes from `before-dispatch`. Use `command.replace` to replace invocation and `command.wrap` to handle its result.

Nonblocking directory reads are limited to 8192 entries, 512 KiB and three seconds. A large directory can produce a partial snapshot. The result is cached for two seconds. An external helper starts only when a listing is requested, through the current NSH with internal `--plugin-fs`. A menu displays up to eight rows. The plugin still selects keys and editing code. A history provider replaces the navigation snapshot for the current line. Ordinary history settings control saving commands to disk.
