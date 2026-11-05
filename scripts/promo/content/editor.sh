# Demo content: a config file, as if open in an editor.
printf '\033[?25l\033[2J\033[H'
k='\033[1;35m' s='\033[32m' n='\033[33m' d='\033[2m' r='\033[0m'
printf "${d}  config.toml${r}\n\n"
printf "  ${k}[general]${r}\n"
printf "  gap = ${n}14${r}\n"
printf "  animation_ms = ${n}280${r}\n\n"
printf "  ${k}[theme]${r}\n"
printf "  accent = ${s}\"#2bd4a7\"${r}\n"
printf "  accent_end = ${s}\"#5c8ff2\"${r}\n\n"
printf "  ${k}[duck]${r}\n"
printf "  size = ${n}1.6${r}  ${d}# a big duck${r}\n"
exec sleep infinity
