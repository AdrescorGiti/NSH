i=0
sum=0
while :; do
    sum=$((sum + i))
    i=$((i + 1))
    case $i in 2500) break ;; esac
done
echo "$sum"
