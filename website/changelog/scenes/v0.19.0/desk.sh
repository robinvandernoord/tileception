# Super+D on a busy desk: 0.18.0 minimizes everything into the strip, 0.19.0
# shows the windows as outlines over the dashboard while the keys are held.
start desk "$DESKTOP_CONFIG"
msg desk pointer 1500 700
busy_desk desk
if [ "$ROLE" = before ]; then
    msg desk key Super+d
    sleep 1.5
    shot desk
else
    shot_held desk Super+d 3000 1800
fi
