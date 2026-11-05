# Demo content: a short, playful to-do list.
printf '\033[?25l\033[2J\033[H'
y='\033[1;33m' c='\033[1;36m' d='\033[2m' g='\033[32m' r='\033[0m'
printf "\n  ${y}# Today${r}\n\n"
printf "  ${g}[x]${r} Tile the windows\n"
printf "  ${g}[x]${r} Nest the windows\n"
printf "  ${g}[x]${r} Zoom into the windows\n"
printf "  [ ] Teach the duck to quack\n"
printf "  [ ] Hand this window to the desktop\n\n"
printf "  ${c}> ${r}${d}Super + = zooms in, Super + - zooms out${r}\n"
exec sleep infinity
