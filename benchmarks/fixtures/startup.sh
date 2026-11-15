alias ll='ls -l'
alias gs='git status'
alias where='pwd'
export BENCH_THEME=vanilla BENCH_COLOR='#8DBEFF'
bench_label() {
    printf '%s\n' "${1-default}"
}
bench_action() {
    case $1 in
        print) shift; bench_label "$@" ;;
        *) return 1 ;;
    esac
}
PS1='nsh_bench_configured> '
