<a id="бенчмарки-nsh-dash-zsh-fish-и-bash"></a>

# Benchmarks: NSH, Dash, Zsh, Fish and Bash

[Русская версия](BENCHMARKS.ru.md)

[Results, versions and variation](benchmarks/current.md) · [Raw measurements and SHA-256](benchmarks/current.json)

Build NSH and supply paths to current builds of the other shells. You can extract NSH from an Arch package into a separate directory and pass the resulting ELF with `--nsh`.

~~~sh
cargo build --locked --release
tools/fetch-benchmark-dash
python3 benchmarks/run.py --nsh target/release/nsh \
  --dash .build/current-shells/bin/dash --dash-version 0.5.13.6 \
  --zsh /usr/bin/zsh --fish /usr/bin/fish --bash /usr/bin/bash \
  --cpu 10 --skip-trace --output-dir .build/benchmarks
~~~

Choose an available CPU or omit `--cpu`. Set the number of repetitions with `--samples` and `--cold-samples`. Select workloads with repeatable `--only` options. Specify `--strace` for system calls; `--skip-trace` collects timing measurements only.

All shells run in the same environment without user configuration. A workload succeeds when its exit status, stdout and stderr match the expected result. Where Fish syntax is incompatible, an equivalent native script with the same work and output is used and identified in the report. The first run is discarded for warm workloads. Reports include the median and p10–p90 percentiles. For cold startup, the harness evicts pages belonging to a separate copy of the ELF and checks them with `mincore`. Shared libraries and kernel caches may remain warm.

Stable versions have been checked against official sources. [Dash](https://kernel.googlesource.com/pub/scm/utils/dash/dash/+/refs/tags/v0.5.13.6) · [Zsh](https://www.zsh.org/pub/) · [Fish](https://github.com/fish-shell/fish-shell/releases/tag/4.9.3) · [Bash](https://ftp.gnu.org/gnu/bash/bash-5.3-patches/)

Build the SVG from raw results:

~~~sh
python3 benchmarks/shell_svg.py MANUALS/benchmarks/current.json benchmarks/shells.svg
~~~
