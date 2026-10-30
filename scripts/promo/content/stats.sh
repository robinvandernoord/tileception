# Demo content: a small dashboard that ticks.
printf '\033[?25l'
b='\033[1;34m' y='\033[1;33m' m='\033[1;35m' d='\033[2m' r='\033[0m'
bar() { n=$1; s=""; while [ "$n" -gt 0 ]; do s="$s#"; n=$((n - 1)); done; printf "%s" "$s"; }
while :; do
    printf '\033[2J\033[H\n'
    printf "  ${y}windows tiled today${r}\n\n"
    for day in Mon Tue Wed Thu Fri; do
        printf "  ${d}%s${r}  ${b}%s${r}\n" "$day" "$(bar $((8 + RANDOM % 22)))"
    done
    printf "\n  ${m}zoom level${r}  %s   ${m}ducks${r}  1\n" $((1 + RANDOM % 4))
    sleep 1.2
done
