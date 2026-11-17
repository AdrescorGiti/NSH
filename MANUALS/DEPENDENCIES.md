<a id="зависимости-и-лицензии"></a>

# Dependencies and licenses

[Русская версия](DEPENDENCIES.ru.md)

NSH is distributed under the MIT license. The minimum declared Rust version is 1.85. Targeted checks have been performed on Linux x86_64.

| Rust library | Locked version | License | Purpose |
|---|---:|---|---|
| libc | 0.2.191 | MIT OR Apache-2.0 | System calls, POSIX ABI, processes, files and terminal |
| unicode-segmentation | 1.12.0 | MIT OR Apache-2.0 | Grapheme boundaries for terminal input |
| unicode-width | 0.2.3 | MIT OR Apache-2.0 | Grapheme and text width in columns |

Crates.io checksums are recorded in Cargo.lock. DEB and RPM builds require glibc 2.17 or later and the GCC runtime library. Changing the login shell uses the installed package manager and `chsh`. Git is needed for the prompt module, and the `fc` command uses an external editor.

NSH uses mlua 0.11.6 (MIT) with Lua 5.4.8 (MIT). Plugin manifests are processed using serde 1.0.229, toml 0.9.8 and semver 1.0.28 (MIT OR Apache-2.0). Lua is built from the lua-src Cargo dependency and included in the binary. The minimum Rust version for mlua 0.11.6 is 1.85. Checksums are in Cargo.lock. The package installs license notices to `/usr/share/licenses/nsh/THIRD-PARTY.txt`. [Dependency licenses](https://github.com/AdrescorGiti/NSH/blob/main/docs/licenses/THIRD-PARTY.txt)
