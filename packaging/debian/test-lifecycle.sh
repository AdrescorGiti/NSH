#!/bin/sh
set -eu

[ -f /.dockerenv ] && [ "$(id -u)" = 0 ]
export LC_ALL=C
umask 022
binary=/usr/bin/nsh
doc=/usr/share/doc/nsh
user=nshverify
home=/home/$user
config=$home/config/nsh
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT

getconf GNU_LIBC_VERSION
dpkg --version | head -n1
[ ! -e "$binary" ]
useradd -m -s /bin/sh "$user"
mkdir -p "$config/plugins" "$home/dev"
printf "nsh set prompt.symbol 'test'\n" > "$config/conf.sh"
cp "$config/conf.sh" "$fixture/conf.sh"
cp /etc/shells "$fixture/shells"
previous=$(getent passwd "$user" | cut -d: -f7)
system_sh=$(readlink /bin/sh)

# Some slim images exclude documentation from all installs.
dpkg --path-include="$doc" --path-include="$doc/*" \
    --path-include=/usr/share/man/man1/nsh.1.gz -i /nsh.deb
[ "$(dpkg-query -W -f='${Status}' nsh)" = 'install ok installed' ]
[ "$(dpkg-query -S -- "$binary")" = "nsh: $binary" ]
[ -z "$(dpkg -V nsh)" ]
cmp /etc/shells "$fixture/shells"
cmp "$config/conf.sh" "$fixture/conf.sh"
[ "$(getent passwd "$user" | cut -d: -f7)" = "$previous" ]
[ "$(readlink /bin/sh)" = "$system_sh" ]
dpkg -L nsh | grep -qx '/usr/share/man/man1/nsh.1.gz'
[ -f "$doc/copyright" ] && [ -f "$doc/MANUALS/CONFIG.md" ]

cp -R "$doc/examples/plugins/custom-commands" "$config/plugins/"
chown -R "$user:$user" "$home"
as_user() {
    runuser -u "$user" -- env HOME="$home" XDG_CONFIG_HOME="$home/config" \
        XDG_STATE_HOME="$home/state" TERM=xterm "$@"
}
version=$(dpkg-deb -f /nsh.deb Version)
[ "$(as_user "$binary" --version)" = "NSH (Neo SH) ${version%-*}" ]
command='join() { printf "%s:%s\n" "$1" "$2"; }; join "$0" "$1" | cat > "$HOME/result"; cat "$HOME/result"'
[ "$(as_user "$binary" -c "$command" 'Debian test' 'works')" = 'Debian test:works' ]
printf '#!/usr/bin/nsh\nprintf "%%s\\n" shebang-verified\n' > "$home/example.sh"
chmod 0755 "$home/example.sh"
[ "$(as_user "$home/example.sh")" = 'shebang-verified' ]
as_user "$binary" config check "$config/conf.sh"
as_user "$binary" plugin enable custom-commands
[ "$(as_user "$binary" --plugins -c 'hello Debian; cdev; pwd')" = "Hello from Lua, Debian!
$home/dev" ]
printf '%s\n' 'printf "%s%s\n" "interactive-" "verified"' exit \
    | as_user script -qec '/usr/bin/nsh --vanilla -i' /dev/null > "$fixture/pty.log"
grep -q 'interactive-verified' "$fixture/pty.log"

if as_user "$binary" --set-default --dry-run > "$fixture/check.log" 2>&1; then
    cat "$fixture/check.log"
    exit 1
fi
grep -q 'missing from /etc/shells' "$fixture/check.log" || { cat "$fixture/check.log"; exit 1; }
printf '\n%s\n' "$binary" >> /etc/shells
as_user "$binary" --set-default --dry-run > "$fixture/check.log" 2>&1 \
    || { cat "$fixture/check.log"; exit 1; }
grep -q 'Checks: passed' "$fixture/check.log"
[ "$(getent passwd "$user" | cut -d: -f7)" = "$previous" ]
cat "$fixture/shells" > /etc/shells

dpkg --purge nsh
[ ! -e "$binary" ]
cmp /etc/shells "$fixture/shells"
cmp "$config/conf.sh" "$fixture/conf.sh"
[ -f "$config/plugins/custom-commands/plugin.toml" ]
[ "$(getent passwd "$user" | cut -d: -f7)" = "$previous" ]
[ "$(readlink /bin/sh)" = "$system_sh" ]
printf '%s\n' 'Install, scripts, PTY, Lua, ownership checks, login-shell dry run and purge passed.'
