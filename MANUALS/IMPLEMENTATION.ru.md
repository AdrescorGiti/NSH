# Проверка сборки

[English](IMPLEMENTATION.md)

~~~sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
python3 tests/core.py target/release/nsh
python3 tests/jobs_pty.py target/release/nsh
python3 tests/plugin_pty.py target/release/nsh
~~~

`tests/core.py` проверяет запуск скриптов и встроенные команды. PTY-тесты проверяют приглашение, клавиатуру, плагины, сигналы и управление заданиями. `tests/differential.py` прогоняет набор POSIX-сценариев, а `tools/test-native` проверяет исполнение без сторонней shell.

Полные проверки сборки и установки перечислены в [руководстве по пакетам](PACKAGE.ru.md). Для измерений используйте [бенчмарки](BENCHMARKS.ru.md).
