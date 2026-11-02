# Demo content: a build that keeps going, so the window looks busy.
printf '\033[?25l'
g='\033[1;32m' b='\033[1;34m' d='\033[2m' r='\033[0m'
crates="smithay calloop wayland-server xkbcommon tileception-core tileception-handoff tileception-acp tileception"
printf "${d}\$ ${r}cargo build --release\n"
for c in $crates; do
    printf "   ${g}Compiling${r} %s ${d}v0.%s.%s${r}\n" "$c" $((RANDOM % 9)) $((RANDOM % 20))
    sleep 0.35
done
printf "    ${g}Finished${r} \`release\` profile [optimized] target(s) in 41.20s\n\n"
printf "${d}\$ ${r}cargo test\n"
while :; do
    for t in zoom::into_group zoom::out_to_overview nest::split_longer_side tabs::drop_on_centre \
        handoff::window_crosses_edge duck::waddles_home views::save_and_return float::above_tiles; do
        printf "test %-34s ... ${g}ok${r}\n" "$t"
        sleep 0.45
    done
done
