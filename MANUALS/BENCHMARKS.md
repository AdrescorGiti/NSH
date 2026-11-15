# Бенчмарки NSH, Dash, Zsh, Fish и Bash

[Результаты, версии и разброс](benchmarks/current.md) · [Сырые замеры и SHA-256](benchmarks/current.json)

Соберите NSH и укажите пути к актуальным сборкам остальных оболочек. NSH из Arch-пакета можно извлечь в отдельный каталог и передать полученный ELF через `--nsh`.

~~~sh
cargo build --locked --release
tools/fetch-benchmark-dash
python3 benchmarks/run.py --nsh target/release/nsh \
  --dash .build/current-shells/bin/dash --dash-version 0.5.13.6 \
  --zsh /usr/bin/zsh --fish /usr/bin/fish --bash /usr/bin/bash \
  --cpu 10 --skip-trace --output-dir .build/benchmarks
~~~

Выберите доступный CPU или опустите `--cpu`. Количество повторов задают `--samples` и `--cold-samples`. Сценарии выбираются повторяемым `--only`. Для системных вызовов укажите `--strace`. Флаг `--skip-trace` оставляет замеры времени.

Все оболочки запускаются в общем окружении без пользовательской конфигурации. Сценарий считается успешным при совпадении статуса, stdout и stderr с эталонным результатом. Для несовместимого синтаксиса Fish используется эквивалентный нативный сценарий с той же работой и выводом, помеченный в отчёте. Для прогретых сценариев первый запуск отбрасывается. В отчёте указаны медиана и процентили p10–p90. Холодный запуск вытесняет страницы отдельной копии ELF и проверяет их через `mincore`. Общие библиотеки и кэши ядра могут оставаться прогретыми.

Стабильные версии сверены с официальными источниками. [Dash](https://kernel.googlesource.com/pub/scm/utils/dash/dash/+/refs/tags/v0.5.13.6) · [Zsh](https://www.zsh.org/pub/) · [Fish](https://github.com/fish-shell/fish-shell/releases/tag/4.9.3) · [Bash](https://ftp.gnu.org/gnu/bash/bash-5.3-patches/)

SVG строится из сырых результатов:

~~~sh
python3 benchmarks/shell_svg.py MANUALS/benchmarks/current.json benchmarks/shells.svg
~~~
