<a id="текущий-выпуск-nsh-021"></a>

# Current release: NSH 0.2.1

[Русская версия](CHANGELOG.ru.md)

- A POSIX-oriented shell interpreter written in Rust. Scripts run without initializing Lua.
- Bundled Lua 5.4 with API 1. Plugins can add commands, change session variables and working directory, and customize menus, prompts, highlighting and suggestions. Modules, palettes, dependencies and plugin reloads are supported.
- Transactional profile configuration, diagnostics with file and line information, and validation through `nsh config check`.
- Job control, recovery mode, and a verifiable way to change the login shell.
- Packages for Arch Linux and CachyOS, DEB packages for Debian and Ubuntu, and RPM packages for Fedora, RHEL-compatible systems and ROSA. DEB and RPM use a shared x86-64-v1 binary with glibc 2.17 or later.
- [Guides](MANUALS/) and [benchmarks of NSH, Dash, Zsh, Fish and Bash](MANUALS/benchmarks/current.md).
