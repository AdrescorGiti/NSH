# NSH, Dash, Zsh, Fish, Bash

[Русская версия](current.ru.md)

CachyOS, x86_64, Linux 7.3.3-1-cachyos, glibc 2.44.
12th Gen Intel(R) Core(TM) i5-12400F. Measured 2026-11-15T19:22:10+00:00.
CPU affinity 10. Governor powersave. Python 3.14.9, LC_ALL=C.UTF-8, TZ=UTC.

- NSH 0.2.1 · 2596680 bytes · SHA-256 `f24fa9a4bdfa8e65f1ab10b8f9d5a4f1a0a066289d6a628d1c4a543601a7f5f0`
- Dash 0.5.13.6 · 139040 bytes · SHA-256 `ba57a3a99fa925a97e98392a293924e8b89f41e6942e9fb0a4153b55f713252c`
- Zsh 5.9.2 · 1184848 bytes · SHA-256 `14846ff02a25534afe9a892e89e81f3676ab06aab26eccfa71869a23fb35ec27`
- Fish 4.9.3 · 11791504 bytes · SHA-256 `a7f62f606704b055039e229d5a46e589b2b9c4a8699c95eedbcd656ccb876adc`
- Bash 5.3.20 · 1268912 bytes · SHA-256 `917e986de64e7d36e42bef7aff5db73b7ecef5b58477bdb990e595e8dfc930b2`

| Workload, ms [p10–p90] | n | NSH | Dash | Zsh | Fish | Bash |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Startup, warm ELF | 25 | 1.733 [1.661–1.771] | 1.536 [1.494–1.574] | 1.911 [1.818–2.014] | 2.338 [2.234–2.613] | 1.882 [1.800–1.953] |
| Startup, cold ELF pages | 20 | 3.811 [3.551–4.591] | 2.026 [1.907–2.407] | 3.192 [3.118–3.728] | 9.279 [8.650–10.933] | 3.145 [3.069–3.266] |
| 2000 true builtin calls | 25 | 2.361 [2.301–2.519] | 1.904 [1.889–1.945] | 3.802 [3.765–3.839] | 4.879 [4.808–5.150] | 2.737 [2.701–3.170] |
| 800 printf builtin calls | 25 | 3.494 [3.452–3.750] | 2.725 [2.609–3.497] | 5.195 [4.978–5.875] | 5.811 [5.685–6.715] | 4.234 [4.145–5.057] |
| 1600 function calls in two loops | 25 | 4.107 [4.039–4.210] | 3.762 [3.719–4.087] | 12.102 [11.969–12.791] | 13.872 [13.656–15.626] | 9.368 [9.214–9.661] |
| Multiline block of 1200 commands | 25 | 2.563 [2.537–2.644] | 1.818 [1.803–1.853] | 2.988 [2.940–3.079] | 3.985 [3.915–4.081] | 2.934 [2.895–3.005] |
| head and wc pipeline, 16 MiB | 25 | 5.718 [5.671–5.829] | 5.554 [5.464–5.698] | 5.935 [5.397–6.061] | 6.224 [6.108–6.376] | 5.915 [5.809–6.062] |
| 100 /usr/bin/true executions | 25 | 27.350 [26.497–29.273] | 19.106 [18.562–19.879] | 32.368 [31.083–34.373] | 21.767 [21.077–24.419] | 28.763 [27.764–31.825] |
| 100 cat executions via PATH | 25 | 39.492 [38.779–42.302] | 29.756 [28.483–33.954] | 44.202 [43.103–45.776] | 33.231 [32.437–34.038] | 39.674 [39.309–41.260] |

Time is measured from process start to exit. Shell order rotates. Before warm measurements, one preparatory run is performed per shell and workload. Each run checks status 0, exact stdout bytes and empty stderr. Raw samples, input hashes and settings are saved in [current.json](current.json).

Shells run noninteractively with the same temporary HOME and XDG values. Invocation flags are in the JSON. Zsh may read an early system zshenv. Startup executes one true builtin. Workloads use the same commands. Fish uses its own syntax for functions, loops and the conditional block with the same number of operations and output. In the PATH search test, two nonexistent directories precede /usr/bin and /bin.

For each cold-start repetition, a separate copy of the ELF with the original name is created. After fsync, file pages are evicted with POSIX_FADV_DONTNEED. Before startup, mincore checks that no pages are resident. Libraries, system configuration, metadata and kernel caches may remain warm.

In a separate pass, strace -- version 7.3 collected process system calls. The exec count includes loading the shell. A single exec was verified for builtin workloads.

| Workload | Shell | Fork/clone, successes | Exec, attempts / errors | Wait calls |
| --- | --- | ---: | ---: | ---: |
| Startup, warm ELF | NSH | 0 | 1 / 0 | 0 |
| Startup, warm ELF | Dash | 0 | 1 / 0 | 0 |
| Startup, warm ELF | Zsh | 0 | 1 / 0 | 0 |
| Startup, warm ELF | Fish | 0 | 1 / 0 | 0 |
| Startup, warm ELF | Bash | 0 | 1 / 0 | 0 |
| 2000 true builtin calls | NSH | 0 | 1 / 0 | 0 |
| 2000 true builtin calls | Dash | 0 | 1 / 0 | 0 |
| 2000 true builtin calls | Zsh | 0 | 1 / 0 | 0 |
| 2000 true builtin calls | Fish | 0 | 1 / 0 | 0 |
| 2000 true builtin calls | Bash | 0 | 1 / 0 | 0 |
| 800 printf builtin calls | NSH | 0 | 1 / 0 | 0 |
| 800 printf builtin calls | Dash | 0 | 1 / 0 | 0 |
| 800 printf builtin calls | Zsh | 0 | 1 / 0 | 0 |
| 800 printf builtin calls | Fish | 0 | 1 / 0 | 0 |
| 800 printf builtin calls | Bash | 0 | 1 / 0 | 0 |
| 1600 function calls in two loops | NSH | 0 | 1 / 0 | 0 |
| 1600 function calls in two loops | Dash | 0 | 1 / 0 | 0 |
| 1600 function calls in two loops | Zsh | 0 | 1 / 0 | 0 |
| 1600 function calls in two loops | Fish | 0 | 1 / 0 | 0 |
| 1600 function calls in two loops | Bash | 0 | 1 / 0 | 0 |
| Multiline block of 1200 commands | NSH | 0 | 1 / 0 | 0 |
| Multiline block of 1200 commands | Dash | 0 | 1 / 0 | 0 |
| Multiline block of 1200 commands | Zsh | 0 | 1 / 0 | 0 |
| Multiline block of 1200 commands | Fish | 0 | 1 / 0 | 0 |
| Multiline block of 1200 commands | Bash | 0 | 1 / 0 | 0 |
| head and wc pipeline, 16 MiB | NSH | 2 | 3 / 0 | 2 |
| head and wc pipeline, 16 MiB | Dash | 2 | 3 / 0 | 3 |
| head and wc pipeline, 16 MiB | Zsh | 2 | 3 / 0 | 4 |
| head and wc pipeline, 16 MiB | Fish | 2 | 3 / 0 | 2 |
| head and wc pipeline, 16 MiB | Bash | 2 | 3 / 0 | 3 |
| 100 /usr/bin/true executions | NSH | 100 | 101 / 0 | 100 |
| 100 /usr/bin/true executions | Dash | 100 | 101 / 0 | 200 |
| 100 /usr/bin/true executions | Zsh | 100 | 101 / 0 | 200 |
| 100 /usr/bin/true executions | Fish | 100 | 101 / 0 | 100 |
| 100 /usr/bin/true executions | Bash | 100 | 101 / 0 | 200 |
| 100 cat executions via PATH | NSH | 100 | 301 / 200 | 100 |
| 100 cat executions via PATH | Dash | 100 | 101 / 0 | 200 |
| 100 cat executions via PATH | Zsh | 100 | 101 / 0 | 200 |
| 100 cat executions via PATH | Fish | 100 | 101 / 0 | 100 |
| 100 cat executions via PATH | Bash | 100 | 101 / 0 | 200 |
