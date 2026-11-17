# Aurora Theme

[Русская версия](AURORA_THEME.ru.md)

[Aurora Theme](../examples/plugins/aurora-theme/) shows how a plugin can combine a prompt, palette and keyboard-controlled menu. Copy the `aurora-theme` directory to `${XDG_CONFIG_HOME:-$HOME/.config}/nsh/plugins/` and run in NSH:

~~~sh
nsh plugin enable aurora-theme
~~~

Press Tab on an empty line. Up and Down change the selection; Escape closes the menu. The first Enter inserts the selected command into the line, and the second executes it.

The `[options]` section of `plugin.toml` sets colors for the path, success/error symbol, menu text and selected item background. The default palette is designed for a dark terminal. After saving changes, run `nsh plugin reload aurora-theme`. To activate the theme on every startup, add `nsh plugin enable aurora-theme` to `conf.sh`.

The colors and menu use the [public Lua API](PLUGIN_API.md#editor-styles). The command list is in the `shortcuts` array in `init.lua`.
