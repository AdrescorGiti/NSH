FROM archlinux@sha256:fa2d4ce3310ddd90a4208e5168be8ad367da5129af1245f38fd2d803bcbe3239 AS runtime-test
RUN printf '%s\n' 'Server = https://archive.archlinux.org/repos/2026/11/15/$repo/os/$arch' > /etc/pacman.d/mirrorlist \
    && pacman -Syu --noconfirm --needed python \
    && pacman -Scc --noconfirm

FROM runtime-test AS builder
RUN pacman -S --noconfirm --needed rust fish git \
    && pacman -Scc --noconfirm \
    && useradd --create-home nshbuild \
    && install -d -o nshbuild -g nshbuild /work /out
