decorate() {
    case $1 in
        a*) printf '%s\n' "ok:$1:$2" ;;
        b*) printf '%s\n' "ok:$1:$2" ;;
        *) return 4 ;;
    esac
}
sum=0
i=0
while :; do
    decorate alpha "$i"
    decorate beta "$i"
    if decorate gamma "$i"; then
        exit 7
    fi
    sum=$((sum + i * 2))
    i=$((i + 1))
    case $i in 400) break ;; esac
done > rows
selected=$(grep '^ok:alpha:' rows | wc -l | tr -d ' ')
case $selected in ''|*[!0-9]*) exit 8 ;; esac
(printf '%s\n' "$selected" "$sum")
rm -f rows
