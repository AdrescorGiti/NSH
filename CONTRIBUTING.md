<a id="участие-в-разработке-nsh"></a>

# Contributing to NSH

[Русская версия](CONTRIBUTING.ru.md)

Issues and pull requests are welcome in English or Russian. Before opening an issue, check existing reports and use the appropriate template. For substantial changes, first describe the problem and proposed solution in an issue.

Building requires Linux, Rust 1.85 or newer, a C compiler and Python 3. Lua is built as part of the project's dependencies. From the repository root:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
python3 tests/core.py target/release/nsh
```

CI also runs the other integration tests in `tests/`. The POSIX differential tests require Dash, Bash and Yash. CI builds Dash 0.5.13.5 and Yash 2.61: older versions may not support the POSIX.1-2024 scenarios. Supply paths to those versions:

```sh
python3 tests/differential.py target/release/nsh --dash /path/to/dash --dash-version 0.5.13.5 --bash /usr/bin/bash --yash /path/to/yash
```

The suite checks recorded expectations and accounts for known differences in reference shells; it does not certify full POSIX conformance. Describe behavioral changes and add reproducible tests. Supported features are described in [MANUALS/POSIX.md](MANUALS/POSIX.md).

Create a branch, make your changes and open a pull request explaining the reason and test results. For measurements, follow the existing [benchmark instructions](MANUALS/BENCHMARKS.md); report the conditions, versions and results, and avoid drawing general conclusions from one run. The manually triggered Benchmarks workflow saves results as an artifact; shared GitHub runners do not provide a stable environment for performance comparisons.

The Build release binary workflow runs for `v*` tags or manually and stores a Linux x86-64 archive and checksum as run artifacts. It builds on Ubuntu 24.04 and does not claim glibc 2.17 compatibility; see `tools/build-portable` for that build. The workflow does not publish a GitHub Release.
