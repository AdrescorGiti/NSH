# RPM 4.11 reads v4/gzip, while current RPM also expects a strong payload digest.
%global _rpmformat 4
%global _binary_payload w9.gzdio
%global _build_id_links none
# The installed executable is the already stripped, audited portable ELF.
%global __os_install_post %{nil}
%global debug_package %{nil}

Name:           nsh
Version:        %{nsh_version}
Release:        1
Summary:        NSH: Neo SH, a POSIX-oriented shell with optional Lua plugins
License:        MIT
URL:            https://github.com/AdrescorGiti/NSH
Source0:        nsh-payload.tar
BuildArch:      x86_64

%description
NSH is a Linux shell with a POSIX-oriented Rust interpreter and optional Lua
plugins for the interactive session.

%install
mkdir -p "%{buildroot}"
tar -xf "%{SOURCE0}" -C "%{buildroot}"

%files
%{_bindir}/nsh
%{_mandir}/man1/nsh.1.gz
%doc /usr/share/doc/nsh
%license /usr/share/licenses/nsh
