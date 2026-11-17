# File suggestions

[Русская версия](FILE_SUGGESTIONS.ru.md)

Copy [the file-suggestions example](../examples/plugins/file-suggestions/) into the NSH configuration directory's `plugins/`
directory. Enable it with `nsh plugin enable file-suggestions`.

Type a path prefix at the end of the line. The plugin chooses the shortest
matching suffix, breaks ties lexically and adds a slash for directories.
Press Right Arrow to accept the visible suffix. Pressing Enter submits the
actual line as typed, without the visual suffix. Move left to dismiss it.

The Lua code handles relative and absolute paths, `~/`, Unicode, spaces and
typed shell quotes. It leaves words requiring parameter, command or wildcard
expansion to the usual shell. Host directory snapshots are asynchronous and
bounded. Some entries in very large directories and stale snapshots may
temporarily be unavailable. There is no suggestion scan without this plugin.

In NSH 0.2.1+, add these options to your installed copy of `plugin.toml` to
style the ghost text. Run `nsh plugin reload file-suggestions` in NSH after
saving. See the [style API](PLUGIN_API.md#editor-styles) for the valid fields.

~~~toml
[options.ghost]
fg = "#8ADCF0"
italic = true
~~~
