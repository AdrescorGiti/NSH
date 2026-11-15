file='/data/out/archive.tar.gz'
words='red:green:blue'
empty=''
unset missing
i=0
while :; do
    name=${file##*/}
    stem=${name%.*}
    first=${words%%:*}
    remainder=${words#*:}
    suffix=${missing:-"$stem"}
    prefix=${empty:+unreachable}${empty:-"$first"}
    IFS=:
    set -- $remainder
    unset IFS
    joined=$prefix:$suffix:$1:$2
    i=$((i + 1))
    case $i in 800) break ;; esac
done
echo "$joined"
