i=0
sum=0
while :; do
    sum=$((sum + i))
    i=$((i + 1))
    case $i in 250) break ;; esac
done
printf '%s\n' "$sum"
