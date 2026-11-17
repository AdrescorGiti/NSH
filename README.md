# NSH — Neo SH

[Русская версия](README.ru.md)

<img width="1040" height="300" alt="NSH GitHub banner" src="https://github.com/user-attachments/assets/16640a8d-6fb8-4707-a912-ab16ac3b57c6" />

**Just Vanilla SH.**

NSH is a Linux shell with its own interpreter written in Rust. Ordinary scripts run in a POSIX-oriented mode. In an interactive session, Lua plugins can add commands, change the prompt, and highlight input. Supported syntax is documented in the [POSIX guide](MANUALS/POSIX.md).

![NSH demo showing the shell and a custom mfs plugin](assets/nsh-demo.gif)

The demo includes `mfs`, a personal plugin command; it is not included in NSH by default.

## Quick start

[Install a package or build NSH from source](#installation), then try an interactive session without loading startup files:

```sh
nsh --vanilla
```

At the NSH prompt, run a command and return to your previous shell:

```sh
printf 'Hello from NSH!\n'
exit
```

The package installation does not change your login shell. If you built from source and have not installed NSH, run `./target/release/nsh --vanilla` from the repository root instead.

<a id="установка"></a>

## Installation

Choose the package for your system and download the `SHA256SUMS` file. [Latest release](https://github.com/AdrescorGiti/NSH/releases/latest)

| Distributions | Package | Installation |
|---|---|---|
| Arch Linux, CachyOS | `.pkg.tar.zst` | `sudo pacman -U -- ./nsh-0.2.1-2-x86_64.pkg.tar.zst` |
| Debian, Ubuntu and derivatives | `.deb` | `sudo apt install ./nsh_0.2.1-1_amd64.deb` |
| Fedora, RHEL-compatible systems, ROSA | `.rpm` | `sudo dnf install ./nsh-0.2.1-1.x86_64.rpm` |

Verify the file in the download directory before installing:

~~~sh
sha256sum --ignore-missing -c SHA256SUMS
~~~

The DEB and RPM packages use the same x86-64-v1 binary, compatible with glibc 2.17 and later. Installation has been tested on Debian, Ubuntu, Fedora, Red Hat UBI and ROSA Fresh. See [versions, dependencies and package builds](MANUALS/PACKAGE.md#debian-and-rpm-distributions).

Building from source requires Linux, Rust 1.85+ and a system C compiler:

~~~sh
cargo build --locked --release
target/release/nsh --vanilla -c 'printf "<%s>\n" "$@"' example one 'two words'
~~~

Run `nsh` after installation. Installing the package leaves your current login shell unchanged. Use `nsh --posix` for POSIX mode, `nsh --vanilla` to start without startup files, or `nsh --no-plugins` to start with Lua plugins disabled.

<a id="плагины"></a>
<a id="plugins"></a>

## Try a Lua plugin

The shipped `custom-commands` example adds a `hello` command. After installing an NSH package, run these commands in your current shell to copy the example into a temporary configuration and start NSH:

```sh
NSH_DEMO_CONFIG="$(mktemp -d)"
mkdir -p "$NSH_DEMO_CONFIG/nsh/plugins"
cp -R /usr/share/doc/nsh/examples/plugins/custom-commands "$NSH_DEMO_CONFIG/nsh/plugins/"
XDG_CONFIG_HOME="$NSH_DEMO_CONFIG" XDG_STATE_HOME="$NSH_DEMO_CONFIG/state" nsh
```

At the NSH prompt, enable the example for this session and try it:

```sh
nsh plugin enable custom-commands
hello NSH
exit
```

The `hello NSH` command prints `Hello from Lua, NSH!`. This demo uses a temporary configuration and leaves your usual NSH configuration unchanged. The temporary directory remains at the path stored in `NSH_DEMO_CONFIG` in your original shell.

If you built from source instead, run the commands from the repository root, replace the `cp` source with `examples/plugins/custom-commands`, and replace `nsh` in the last line with `./target/release/nsh`.

The example registers `hello` in Lua:

```lua
nsh.command.register("hello", function(args)
    nsh.print("Hello from Lua, " .. (args[1] or "NSH") .. "!")
    return 0
end)
```

The [included examples](examples/plugins/) also provide syntax highlighting, path suggestions accepted with the Right Arrow key, and [Aurora Theme](MANUALS/AURORA_THEME.md). See [plugin installation](MANUALS/PLUGINS.md), the [Lua API tutorial](MANUALS/PLUGIN_API.md), and [session configuration](MANUALS/CONFIG.md).

<a id="преимущества-nsh"></a>

## Why NSH?

- **Scripts and interactivity in one shell.** The Rust interpreter runs POSIX-oriented scripts without initializing Lua.
- **Lua can change the session.** Plugins can register commands, change the working directory and variables, customize the prompt, and add menus, highlighting and suggestions. Lua commands run in the NSH process and support redirections and pipelines.
- **Recoverable configuration.** `nsh config check` checks syntax, a loading error rolls back managed profile settings, and `nsh --no-plugins` starts a session with plugins disabled.

<a id="бенчмарки-пяти-оболочек"></a>

## Benchmarks across five shells

![Comparison of NSH, Dash, Zsh, Fish and Bash: median execution time in milliseconds](benchmarks/shells.svg)

In this recorded run, NSH ranked second by median in 6 of 9 workloads. For 1,600 function calls, NSH took 4.11 ms, less than half the measured time of Bash, Zsh and Fish in that workload. Dash had the lowest median in all nine workloads. These results describe the recorded environment and workloads, not every system.

[Versions, variation and raw measurements](MANUALS/benchmarks/current.md) · [How to reproduce the benchmarks](MANUALS/BENCHMARKS.md)

<a id="документация"></a>

## Documentation

[All guides](MANUALS/) · [GitHub Wiki](https://github.com/AdrescorGiti/NSH/wiki) · [Current release](CHANGELOG.md) · [MIT license](LICENSE)
