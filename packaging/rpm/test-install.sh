#!/usr/bin/env bash
# Run inside a fresh disposable RPM distro container as root.
set -euo pipefail
[[ -f /.dockerenv && $(id -u) == 0 && ! -e /usr/bin/nsh ]]
package=/tmp/nsh-tested.rpm
test_version=$(rpm -qp --queryformat '%{VERSION}' "$package")
account=nsh-rpm-test
useradd -m -s /bin/bash "$account"
original_account=$(getent passwd "$account")
original_shells=$(sha256sum /etc/shells)
cp /etc/shells /tmp/nsh-original-shells
home=$(cut -d: -f6 <<< "$original_account")
owner="$(id -u "$account"):$(id -g "$account")"
as_account() {
    HOME="$home" USER="$account" LOGNAME="$account" chroot --userspec "$owner" / /bin/sh -c "$1"
}

. /etc/os-release
printf '%s, %s, %s\n' "$PRETTY_NAME" "$(rpm --version)" "$(getconf GNU_LIBC_VERSION)"
rpm -K --nosignature "$package"
if [[ "$ID" == fedora ]]; then
    dnf --disable-repo='*' --setopt=tsflags= -y install "$package"
else
    rpm -ivh -- "$package"
fi
rpm -V nsh
database=$(rpm --eval '%{_dbpath}')
[[ $(rpm --noplugins --dbpath "$database" -qf --queryformat '%{NAME}\n' -- /usr/bin/nsh) == nsh ]]
[[ $(rpm -qp --queryformat '%{PAYLOADCOMPRESSOR}' "$package") == gzip ]]
[[ $(getent passwd "$account") == "$original_account" ]]
[[ $(sha256sum /etc/shells) == "$original_shells" ]]
[[ $(as_account '/usr/bin/nsh --version') == *"$test_version"* ]]
[[ $(as_account "/usr/bin/nsh -c 'printf %s portable | cat'") == portable ]]

as_account 'mkdir -p "$HOME/.config/nsh/plugins/nsh-rpm-test"'
manifest="$home/.config/nsh/plugins/nsh-rpm-test/plugin.toml"
entry="$home/.config/nsh/plugins/nsh-rpm-test/init.lua"
cat > "$manifest" <<'MANIFEST'
name = "nsh-rpm-test"
version = "1.0.0"
api_version = 1
entry = "init.lua"
MANIFEST
cat > "$entry" <<'LUA'
nsh.command.register("nsh_rpm_lua_test", function() nsh.print("Lua works") end)
LUA
chown "$owner" "$manifest" "$entry"
as_account '/usr/bin/nsh plugin enable nsh-rpm-test'
[[ $(as_account "/usr/bin/nsh --plugins -c 'nsh_rpm_lua_test'") == 'Lua works' ]]
test -f /usr/share/man/man1/nsh.1.gz
test -f /usr/share/doc/nsh/examples/plugins/aurora-theme/init.lua
test -f /usr/share/doc/nsh/MANUALS/PLUGIN_API.md
test -f /usr/share/licenses/nsh/LICENSE

if [[ -x /usr/bin/chsh ]]; then
    grep -qxF /bin/bash /etc/shells || printf '%s\n' /bin/bash >> /etc/shells
    printf '%s\n' /usr/bin/nsh >> /etc/shells
    result=$(as_account '/usr/bin/nsh --set-default --dry-run')
    printf '%s\n' "$result"
    grep -qF 'Checks: passed' <<< "$result"
    cat /tmp/nsh-original-shells > /etc/shells
fi

rpm -e nsh
[[ ! -e /usr/bin/nsh && $(getent passwd "$account") == "$original_account" && -f "$entry" ]]
[[ $(sha256sum /etc/shells) == "$original_shells" ]]
printf '%s\n' 'Installation, loaded Lua plugin and package removal verified.'
