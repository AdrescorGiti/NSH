# Syntax colors

[Русская версия](SYNTAX_COLORS.ru.md)

Copy [the syntax-colors example](../examples/plugins/syntax-colors/) into the NSH configuration directory's `plugins/`
directory. Enable it with `nsh plugin enable syntax-colors`.

To customize a color, export one of `NSH_SYNTAX_COMMAND`, `NSH_SYNTAX_UNKNOWN`,
`NSH_SYNTAX_OPTION`, `NSH_SYNTAX_STRING`, `NSH_SYNTAX_PATH`,
`NSH_SYNTAX_VARIABLE`, `NSH_SYNTAX_OPERATOR` or `NSH_SYNTAX_COMMENT` as
`#RRGGBB`.

With NSH 0.2.1+, you can save a palette in your installed copy of `plugin.toml`.
Existing `NSH_SYNTAX_*` environment variables take precedence. Reload with
`nsh plugin reload syntax-colors` after saving.

~~~toml
[options.colors]
command = "#5BD6CF"
unknown = "#FB7185"
option = "#9DAAFF"
~~~

The plugin colors incomplete input heuristically. It uses the public command
lookup API and reuses command results across argument edits.
