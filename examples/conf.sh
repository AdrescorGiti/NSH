# Copy this example to ${XDG_CONFIG_HOME:-$HOME/.config}/nsh/conf.sh.
nsh set prompt.style two-line
nsh set prompt.user_color '#46DBEA'
nsh set prompt.host_color '#62B8F4'
nsh set prompt.symbol 'λ'
nsh set prompt.symbol.success '❯'
nsh set prompt.symbol.error '✗'
nsh set prompt.path_color '#69B7FF'
nsh set prompt.git_color '#89A6C2'
nsh set prompt.symbol.success_color '#79DEA4'
nsh set prompt.symbol.error_color '#F87183'
# nsh module enable git-prompt

nsh module enable history
nsh set history.max_size 10000
nsh set history.ignore_duplicates true
nsh bind Up history-previous
nsh bind Down history-next
nsh bind Ctrl-R history-search

alias ll='ls -lah'
alias gs='git status'

on_directory_changed() {
    printf 'Каталог: %s\n' "$1" >&2
}
nsh hook add directory-changed on_directory_changed
