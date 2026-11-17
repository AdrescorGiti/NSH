# Подсветка синтаксиса

[English](SYNTAX_COLORS.md)

Скопируйте [пример syntax-colors](../examples/plugins/syntax-colors/) в каталог `plugins/` конфигурации NSH. Включите его командой `nsh plugin enable syntax-colors`.

Для настройки цветов экспортируйте одну из переменных `NSH_SYNTAX_COMMAND`, `NSH_SYNTAX_UNKNOWN`, `NSH_SYNTAX_OPTION`, `NSH_SYNTAX_STRING`, `NSH_SYNTAX_PATH`, `NSH_SYNTAX_VARIABLE`, `NSH_SYNTAX_OPERATOR` или `NSH_SYNTAX_COMMENT` со значением в формате `#RRGGBB`.

В NSH 0.2.1+ палитру можно сохранить в установленном `plugin.toml`. Существующие переменные окружения `NSH_SYNTAX_*` имеют приоритет. После сохранения выполните `nsh plugin reload syntax-colors`.

~~~toml
[options.colors]
command = "#5BD6CF"
unknown = "#FB7185"
option = "#9DAAFF"
~~~

Плагин эвристически подсвечивает незавершённый ввод. Он использует публичный API поиска команд и повторно использует результаты при изменении аргументов.
