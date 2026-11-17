# Aurora Theme

[English](AURORA_THEME.md)

[Aurora Theme](../examples/plugins/aurora-theme/) показывает, как плагин объединяет своё приглашение, палитру и меню с клавиатурным управлением. Скопируйте каталог `aurora-theme` в `${XDG_CONFIG_HOME:-$HOME/.config}/nsh/plugins/` и выполните в NSH:

~~~sh
nsh plugin enable aurora-theme
~~~

Нажмите Tab на пустой строке. Up и Down меняют выбранный пункт, Escape закрывает меню. Первый Enter вставляет команду в строку, следующий запускает её.

В `plugin.toml` секция `[options]` задаёт цвета пути, символа успеха или ошибки, текста меню и фона выбранного пункта. Исходная палитра рассчитана на тёмный терминал. После сохранения выполните `nsh plugin reload aurora-theme`. Чтобы тема подключалась на каждом запуске, добавьте команду `nsh plugin enable aurora-theme` в `conf.sh`.

Цвета и меню реализованы через [публичный Lua API](PLUGIN_API.ru.md#стили-редактора). Список команд находится в массиве `shortcuts` файла `init.lua`.
