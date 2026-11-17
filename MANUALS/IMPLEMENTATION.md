<a id="проверка-сборки"></a>

# Build verification

[Русская версия](IMPLEMENTATION.ru.md)

~~~sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
python3 tests/core.py target/release/nsh
python3 tests/jobs_pty.py target/release/nsh
python3 tests/plugin_pty.py target/release/nsh
~~~

`tests/core.py` checks script execution and builtins. PTY tests check the prompt, keyboard input, plugins, signals and job control. `tests/differential.py` runs POSIX scenarios, while `tools/test-native` checks execution without an external shell.

Full build and installation checks are listed in the [package guide](PACKAGE.md). For measurements, use the [benchmarks](BENCHMARKS.md).
