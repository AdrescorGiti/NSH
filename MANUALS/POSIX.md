<a id="проверка-shell-command-language"></a>

# Shell Command Language checks

[Русская версия](POSIX.ru.md)

The target is POSIX.1-2024 (Issue 8). Selected older implementations are used in differential tests to compare behavior; the standard text is the authority for Issue 8. Tests compare stdout, stderr, status and filesystem state under identical conditions. Where diagnostic text is not standardized, tests check whether a diagnostic is present or absent. [Shell Command Language](https://pubs.opengroup.org/onlinepubs/9799919799/utilities/V3_chap02.html) · [sh utility](https://pubs.opengroup.org/onlinepubs/9799919799/utilities/sh.html)

| Requirement | Test | Result | Limitation |
|---|---|---|---|
| 2.2–2.3, quoting and tokens | `syntax::tests`, differential, core | Partial | `$( (cmd) )` is parsed with a space. Ambiguous `$((` without a space is treated as arithmetic. Known limitations involve line continuation within the start of a substitution and substitution of some open constructs from aliases. |
| 2.2.4 and 2.9.4.3, `$'…'` and `;&` | `test_issue8_case_dollar_quotes_pipefail`, differential | Tested cases | A decoded NUL truncates this literal. Dash can differ on newer constructs. |
| 2.4, 2.9 and 2.10, commands, lists, groups, conditions, loops, functions | `syntax::tests`, core, differential | Partial | Complex alias interactions and functions named after builtins have limited coverage. |
| 2.5, 2.6, variables, empty and unset, `"$@"`, IFS, expansion, glob | core, differential | Partial | Streams and paths are processed as bytes. Full IFS and quoting rules in parameter operands, range matching and multibyte delimiters need further testing. |
| 2.6.1, tilde | differential | Partial | Tests cover word beginnings and assignments. Uncommon contexts and NSS have limited coverage. |
| 2.6.4, arithmetic | core, differential | Partial | Lazy branches and basic operations are tested. Ambiguous operators and values outside the usual range need testing. |
| 2.7, redirection order, closing and duplication, here-doc | core, differential | Partial | Parent descriptors are restored. Overwrite protection and descriptor-number limits need further checks. |
| 2.8, exit on error and statuses 126/127 | core, differential | Partial | Common cases are tested. `errexit` contexts within nested compound commands need broader coverage. |
| 2.9.1, command search and Linux `ENOEXEC` | core, differential, native | Partial | Empty PATH components, permissions, scripts without a shebang and byte-oriented names are covered. A file with an explicit shebang is passed to the kernel. |
| 2.9.2, pipelines, negation and `pipefail` | core, differential, jobs_pty | Partial | A 1 MiB transfer, statuses and some interactive signals are tested. |
| 2.9.3 and `wait`, `jobs`, `fg`, `bg` sections | core, jobs_pty | Partial | PTY tests check foreground, stopping, continuing and terminal return. Some jobspec forms and options are unsupported. |
| 2.15, special builtins | core, differential | Partial | `.`, `:`, `break`, `continue`, `eval`, `exec`, `exit`, `export`, `readonly`, `return`, `set`, `shift`, `times`, `trap`, `unset` are implemented. The full option set, forms of `trap` and signal interactions during `wait` need testing. |
| `printf` and `test` utilities, Issue 8 | printf, performance_contracts, differential | Partial | `printf` runs inside NSH when selected from a system directory in PATH. Floating-point numbers use binary64 precision. Undefined forms of `test` and string comparisons in other locales are passed to the selected external utility. |
| 1.7 (XCU), intrinsic utilities | core, differential, fc_pty, session_pty, jobs_pty | Partial | `fc` numbers entries within the saved history segment; `HISTFILE` and `HISTSIZE` do not control NSH history format or limit. `ulimit`, `umask` and `kill` are tested in common modes. |
| `sh`, startup arguments and ENV | core, session_pty, login smoke | Partial | Arguments, stdin and startup are tested. ENV requires an absolute path to a trusted file and matching IDs. Mismatched UID/GID tests need expansion. |
| LC_CTYPE/LC_COLLATE | differential, jobs_pty | Partial | The differential corpus uses C; Unicode PTY uses C.UTF-8. National locales, multibyte IFS and complete collation rules have not been tested. |

Where the standard leaves a result undefined or unspecified, one external shell’s behavior is not treated as mandatory. If `PATH` is unset, NSH defaults to `/usr/bin:/bin`. Unsupported interpreter-specific shell extensions are outside the stated scope.

<a id="воспроизведение"></a>

## Reproduction

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
python3 tests/core.py target/release/nsh
python3 tests/jobs_pty.py target/release/nsh
python3 tests/session_pty.py target/release/nsh
python3 tests/fc_pty.py target/release/nsh
```

`python3 tests/differential.py --help` lists flags for paths to reference binaries. The project checks POSIX scenarios against its own test suite. The differential test prints versions and identifies reference differences.
