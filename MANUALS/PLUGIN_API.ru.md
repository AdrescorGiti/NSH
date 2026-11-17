# Lua API 1

[English](PLUGIN_API.md)

NSH встраивает Lua 5.4 через mlua. API выбирается целым полем `api_version = 1` в манифесте. Для несовместимого изменения увеличивается номер API. Новые возможности 0.2.1 используют тот же API. Чтобы плагин с новыми вызовами загружался только подходящей оболочкой, укажите `nsh_version = ">=0.2.1, <0.3.0"`. Версии доступны как `nsh.version` и `nsh.api_version`. [mlua](https://docs.rs/mlua/0.11.6/mlua/)

## Команды

Создайте каталог `~/.config/nsh/plugins/example/` и поместите в него `plugin.toml`.

```toml
name = "example"
version = "1.0.0"
api_version = 1
entry = "init.lua"
description = "Example shell command"
author = "Local user"
capabilities = ["shell-state"]
```

В том же каталоге создайте `init.lua`.

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

Добавьте `nsh plugin enable example` в `conf.sh` или вызовите эту команду в работающем NSH. Обработчик получает массив аргументов с индексом 1 после shell-подстановок и без имени команды. Строки аргументов и переменных сохраняют байты. `nil` возвращает статус 0. Числовой статус приводится к диапазону 0–255. Изменения состояния выполненной команды видны оболочке. В стадии пайплайна они остаются в её дочернем контексте.

| Метод | Действие |
|---|---|
| `nsh.command.register(name, fn)` | Регистрирует новую команду при загрузке. |
| `nsh.command.replace(name, fn)` | Требует `override`. Регистрирует замену команды. Для специальной POSIX builtin дополнительно действует политика разрешений оболочки. |
| `nsh.command.wrap(name, fn)` | Требует `override`. После оригинала вызывает `fn(args, status)`. При `nil` сохраняет исходный статус. |
| `nsh.command.unregister(name)` | Удаляет свою команду и возвращает наличие регистрации. |
| `nsh.command.exists(name)` | Проверяет текущие команды, встроенные имена и поиск через PATH. Кэш подсветки учитывает каталог, PATH и срок 2 секунды. |
| `nsh.print(...)` | Пишет простые значения в stdout через табуляцию и перевод строки. |
| `nsh.stdout.write(...)`, `nsh.stderr.write(...)` | Записывают байтовые строки напрямую. Требуют `stdio`. |
| `nsh.stdin.read_line()` | Читает строку без завершающего LF, до 64 КиБ. Возвращает `nil` на EOF. Требует `stdio`. |
| `nsh.stdin.read(max_bytes)` | Читает до `max_bytes` байт за один вызов. Диапазон от 1 до 65536. Сохраняет NUL, переводы строк и произвольные байты. Возможен короткий блок. Возвращает `nil` на EOF. Требует `stdio`. |
| `nsh.shell.get(name)`, `nsh.env.get(name)` | Читают shell-переменную или экспортированное окружение. |
| `nsh.shell.set(name, value)`, `nsh.env.set(name, value)` | Изменяют shell-переменную или экспортируемую переменную. `nil` удаляет значение. Требуют `shell-state`. |
| `nsh.shell.cwd()`, `nsh.shell.status()` | Текущий каталог и предыдущий статус оболочки. |
| `nsh.shell.cd(path)` | Ставит смену каталога в очередь и обновляет снимок для текущего обработчика. Требует `shell-state`. |
| `nsh.process.run(program, args)` | Выполняет внешний ELF или файл с shebang с унаследованными перенаправлениями. Возвращает статус. Требует `process`. Для текстового скрипта без shebang укажите NSH как программу и передайте путь к скрипту аргументом. |
| `nsh.process.exec(program, args)` | Последний эффект обработчика. Требует `exec`. Заменяет текущий процесс. После перехода в Bash Lua-движок и плагины NSH завершают участие в сеансе. |

`nsh.process.run` выполняется синхронно. Дочерний процесс использует текущую группу процессов. Чтобы создать отдельное интерактивное задание, вызывайте внешнюю команду через обычный синтаксис NSH. Явный `nsh.process.exec` выполняется после проверки эффектов команды, вне загрузки пользовательской конфигурации.

## Настройки и файлы плагина

Добавьте в `plugin.toml` настройки, которые пользователь сможет менять перед `nsh plugin reload NAME`.

~~~toml
[options]
greeting = "Привет"
targets = ["NSH", "Lua"]
~~~

Создайте файл `lib/messages.lua` в каталоге своего плагина.

~~~lua
local messages = {}

function messages.hello(name)
    return nsh.config.get("greeting", "Hello") .. ", " .. name
end

return messages
~~~

Подключите его в `init.lua` и замените предыдущий обработчик `hello`.

~~~lua
local messages = nsh.require("lib.messages")

nsh.command.register("hello", function(args)
    local targets = nsh.config.get("targets", {"NSH"})
    nsh.print(messages.hello(args[1] or targets[1]))
    return 0
end)
~~~

`nsh.config.get()` возвращает копию `[options]`. Вызов с ключом верхнего уровня возвращает отдельное значение, с ключом и вторым аргументом — значение по умолчанию для отсутствующего ключа. TOML-массивы получают индексы с 1, даты возвращаются строками. Изменение полученной таблицы действует только в Lua.

`nsh.require("lib.messages")` читает `lib/messages.lua` внутри текущего плагина. Допускается доверенный исходный Lua-код в обычных файлах. Имя ограничено 128 байтами и состоит из разделённых точками частей с ASCII-буквами, цифрами, `_` и `-`. Lua кэширует возвращённое значение для текущей загрузки. Модуль с `return nil` или без `return` возвращает `true`. Первые импорты, в том числе вложенные, выполняются при инициализации плагина. В обработчиках доступны уже загруженные модули. При reload NSH создаёт новый Lua-кэш. В одном плагине можно загрузить до 64 модулей размером до 1 МиБ каждый.

## Жизненный цикл

`nsh.on(event, fn)` и `nsh.events.on(event, fn)` регистрируют одно событие. `fn(args)` принимает массив строк. Доступные имена включают `load`, `enable`, `disable`, `reload`, `unload`, `error`, `shutdown`, `startup`, `before-prompt`, `pre-command`, `before-dispatch`, `after-dispatch`, `post-command`, `directory-changed` и `exit`.

Команды, события и провайдеры регистрируются в верхнеуровневом `init.lua`. Данные текущего сеанса доступны в обработчиках. Верхнеуровневый код при инициализации читает унаследованное окружение процесса. Для Lua действует ограничение памяти 8 МиБ на VM и бюджет инструкций. Предлагаемые изменения состояния собираются во время успешного вызова. Вывод и явно запущенные процессы являются непосредственными побочными эффектами.

## Редактор, меню и история

Редактор предоставляет строку UTF-8 и **нулевой байтовый индекс** курсора. Позиции замены и курсора должны совпадать с границами графем. Например, для строки `λ` позиции 0 и 2 допустимы.

| Метод | Действие |
|---|---|
| `nsh.editor.on_change(fn)` | Вызывает `fn(line, cursor)` на изменениях ввода. |
| `nsh.editor.on_tick(fn)` | Вызывает `fn(line, cursor)` во время фонового запроса редактора. При отсутствии своего tick вызывается обработчик изменений. |
| `nsh.editor.line()`, `nsh.editor.cursor()` | Возвращают состояние текущего кадра. |
| `nsh.editor.set_line(line[, cursor])` | Заменяет всю строку. По умолчанию курсор ставится в её конец. |
| `nsh.editor.replace(start, end, text)`, `nsh.editor.set_cursor(offset)` | Меняют текст или положение курсора. `end` — исключительная позиция. |
| `nsh.editor.set_ghost_text(text[, style])` | Рисует подсказку. `nil` очищает её. Принятие подсказки требует явной операции замены. |
| `nsh.editor.set_virtual_text(items)` | Принимает элементы `{offset, text, style}`. Поле `color` сохраняет поддержку HEX. |
| `nsh.editor.set_highlights(items)` | Принимает диапазоны `{start, ["end"], style}`. Поле `color` сохраняет поддержку HEX. |
| `nsh.editor.set_menu(items[, options])` | Показывает список строк или `{text, style}`. Передайте `nil`, чтобы скрыть меню. |
| `nsh.keys.bind(key, fn)`, `nsh.keys.wrap(key, fn)` | Вызывают `fn(key, default_action)`. `true` обрабатывает клавишу, `nil` или `false` оставляет стандартное действие. |
| `nsh.prompt.register(fn)` | `fn(default_prompt, status)` возвращает строку, список `{text, style}` или `nil`. |
| `nsh.highlight.register(fn)` | `fn(line, cursor)` возвращает таблицу диапазонов подсветки или `nil`. |
| `nsh.completion.register(fn)` | `fn(line, cursor)` при Tab возвращает пункты меню, таблицу `{items, selected, style, selected_style}` или `nil`. |
| `nsh.history.entries()`, `nsh.history.register(fn)` | Читает историю. Провайдер `fn(default_entries)` возвращает до 512 записей общим объёмом до 8 МиБ или `nil`. |
| `nsh.fs.read_dir(path, prefix?)`, `nsh.fs.request(path, prefix?)` | Асинхронный снимок каталога. Возвращает массив `{name, is_dir}`, `nil` пока ожидается чтение или `nil, error`. Требует `fs`. |

Пример вставляет видимый суффикс только после Right Arrow.

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

Полные примеры Lua находятся в `examples/plugins/`, включая поиск реальных файлов. Провайдеры редактора имеют доступ к снимку оболочки для чтения. Запуск синхронной команды и чтение stdin доступны из обычного обработчика команды или события, чтобы ввод редактора не блокировался.

## Стили редактора

В `style` можно передать цвет переднего плана `"#RRGGBB"`, имя стиля, встроенный `"dim"` или таблицу с `fg`, `bg` и логическими полями `bold`, `dim`, `italic`, `underline`, `inverse`. В диапазонах подсветки и виртуального текста `style` имеет приоритет над `color`.

`nsh.style.define(name, style)` сохраняет или заменяет стиль в палитре текущего плагина. `nsh.style.defined(name)` проверяет имя в палитре, `nsh.style.remove(name)` удаляет его и сообщает, было ли оно задано. Имя допускает до 64 ASCII-букв, цифр и символов `-_.`. Палитра хранит до 512 имён.

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

У меню индекс `selected` начинается с 1. NSH отображает до восьми строк. Выбор и вставку плагин может связать с клавишами через `nsh.keys.bind` и `nsh.editor.replace`. Структурированные стили учитывают `NO_COLOR`, `TERM`, настройки `color.mode` и возможности терминала. При 256-цветном режиме NSH переводит RGB в палитру. Если части одной графемы попали в разные сегменты приглашения, применяется стиль её первого символа.

## Границы API

Встроенные стандартные библиотеки Lua включают table, string, utf8 и math. Файловые и системные Lua-библиотеки, сырые загрузчики, coroutines и protected calls не открыты. Ограничения уменьшают случайные блокировки. Плагин с возможностями `stdio`, `process`, `fs`, `shell-state` или `exec` остаётся исполняемым кодом пользователя. Разрешения API управляют доступом к операциям NSH. Изоляция процессов и доступа к файловой системе остаётся задачей ОС.

Терминальный профиль и шрифт задаются через доступный интерфейс самого терминала. Например, плагин может вызвать внешний клиент Konsole с аргументами и соответствующим разрешением `process`.

`before-dispatch` получает имя команды и её уже подставленные аргументы. `after-dispatch` получает код завершения, имя и аргументы. Это наблюдаемые этапы. Изменения среды и каталога из `before-dispatch` видит фактический запуск. Для замены запуска используйте `command.replace`, для обработки его результата — `command.wrap`.

Неблокирующее чтение каталога ограничено 8192 записями, 512 КиБ и тремя секундами. Тяжёлый каталог может дать частичный снимок. Результат кешируется две секунды. Внешний помощник запускается только при запросе списка, через текущий NSH с внутренним `--plugin-fs`. Меню отображает до восьми строк. Клавиши и код редактирования по-прежнему выбирает плагин. Провайдер истории подменяет снимок навигации на текущую строку. Сохранением команд на диск управляют обычные настройки истории.
