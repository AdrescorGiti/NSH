<a id="пакеты-nsh-для-linux"></a>

# NSH packages for Linux

[Русская версия](PACKAGE.ru.md)

<a id="debian-и-rpm-дистрибутивы"></a>

## Debian and RPM distributions

DEB (`amd64`) and RPM (`x86_64`) packages contain the same optimized binary. Rust and bundled Lua are built for baseline x86-64-v1. The binary uses system glibc 2.17 or later and the system GCC support library. Lua support is built in. Packages install `nsh`, manuals and plugin examples. Changing the login shell is a separate operation.

Verify the checksums from the package directory:

```sh
sha256sum --ignore-missing -c SHA256SUMS-portable.txt
```

On Debian and Ubuntu, install the file through APT:

```sh
sudo apt install ./nsh_0.2.1-1_amd64.deb
```

On Fedora and compatible systems using DNF, install the RPM:

```sh
sudo dnf install ./nsh-0.2.1-1.x86_64.rpm
```

On systems using YUM, run `sudo yum localinstall ./nsh-0.2.1-1.x86_64.rpm`. On older Debian systems whose APT does not accept a local path, use `sudo dpkg -i ./nsh_0.2.1-1_amd64.deb` after installing dependencies. Run `nsh --version` after installation.

CPU and kernel requirements for the system itself depend on the distribution. To change the login shell, also install the system `chsh` and register `/usr/bin/nsh` in `/etc/shells`. [Login shell guide](LOGIN.md)

Installation, execution and removal have been tested on Debian 8, 10 and 13, Ubuntu 16.04, Fedora 45, Red Hat UBI 7.9 and 8.10, and ROSA Fresh 12.4. Scripts and a Lua plugin were also tested in QEMU with the baseline x86-64 instruction set and glibc 2.17.

<a id="сборка-deb-и-rpm"></a>

### Building DEB and RPM packages

Docker, rustup with Rust 1.98.1 and binutils are required. DEB builds require `dpkg-deb`. RPM is built with `rpmbuild` in a container. Run all commands from the project root:

```sh
tools/build-portable .build/portable/nsh
tools/package-deb .build/portable/nsh
tools/package-rpm .build/portable/nsh
(
    cd packages
    sha256sum nsh_0.2.1-1_amd64.deb nsh-0.2.1-1.x86_64.rpm > SHA256SUMS-portable.txt
)
```

<a id="arch-linux-и-cachyos"></a>

## Arch Linux and CachyOS

`tools/package-arch` creates a source archive from the project contents, calculates SHA-256, inserts it into the PKGBUILD and uses a clean Arch Linux x86_64 image. The base image is pinned by digest; build-environment packages come from the Arch Linux Archive dated 15 November 2026. `makepkg` runs through `chroot --userspec=nshbuild:nshbuild` in a clean container root without host directory mounts. Cargo downloads dependencies according to `Cargo.lock` during `prepare`. Build and check stages use `--frozen`.

```sh
tools/package-arch
```

The tool requires Docker, GNU tar, zstd and pacman for local metadata inspection. The resulting PKGBUILD and adjacent source archive can be moved into a separate clean Arch chroot with build dependencies installed; run `makepkg --verifysource`, then `makepkg --cleanbuild --noconfirm` there as an ordinary user. The `pacman -Qip` and `pacman -Qlp` output, local conflict checks and SHA-256 are saved in `logs/arch-package-inspect.log`.

`tools/test-arch-package` checks installation, upgrade, removal, a real `chsh` with PAM and rollback in a disposable Arch container with a separate local account. The container runs without networking or host directory mounts. Pass the previous package as an argument to `tools/package-arch` to test an upgrade between actual versions and a separate clean installation. With no argument, installation and removal of the new version are tested. Results are saved in `logs/arch-package-test.log`. The project’s final report determines the package state and release version.

Before installation or upgrade on the target machine, after approval, verify the specific file’s SHA-256 and contents and check for package-name and `/usr/bin/nsh` path conflicts. For a first installation, `tools/install-arch` accepts the absolute package path and expected SHA-256, displays the installation command and asks for confirmation. The login shell change procedure is described in [LOGIN.md](LOGIN.md).
