#!/bin/sh
# Writes Tileception's login screen badge (SVG) to standard output: a
# Mondrian-style composition made the way Tileception lays out windows, by
# splitting a square into nested tiles. The biggest tile is red and two small
# ones blue and yellow, between thick black lines.
#
#   scripts/session-badge.sh > tileception.svg
#
# Deterministic: the same splits every time, so builds are reproducible.
set -eu

awk 'BEGIN {
    size = 64; line = 3.6; inset = line / 2
    # Where each split falls, as a share of the side, in order of use.
    nr = split("0.62 0.58 0.40 0.66 0.45 0.70 0.35 0.55", ratio, " ")
    used = 0; n = 0
    tile(inset, inset, size - 2 * inset, size - 2 * inset, 3)

    # Colours by size: the largest red, the two smallest blue and yellow.
    for (i = 0; i < n; i++) order[i] = i
    for (i = 0; i < n; i++)
        for (j = i + 1; j < n; j++)
            if (lw[order[j]] * lh[order[j]] > lw[order[i]] * lh[order[i]]) {
                t = order[i]; order[i] = order[j]; order[j] = t
            }
    for (i = 0; i < n; i++) fill[i] = "#f4f1e8"
    fill[order[0]] = "#d62a1e"
    fill[order[n - 1]] = "#f2c230"
    fill[order[n - 2]] = "#1f4e9c"

    printf "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 %d %d\" width=\"%d\" height=\"%d\">\n", size, size, size, size
    printf "  <defs><clipPath id=\"c\"><rect width=\"%d\" height=\"%d\" rx=\"14\"/></clipPath></defs>\n", size, size
    printf "  <g clip-path=\"url(#c)\">\n"
    printf "    <rect width=\"%d\" height=\"%d\" fill=\"#111111\"/>\n", size, size
    for (i = 0; i < n; i++)
        printf "    <rect x=\"%.2f\" y=\"%.2f\" width=\"%.2f\" height=\"%.2f\" fill=\"%s\"/>\n", \
            lx[i] + inset, ly[i] + inset, lw[i] - line, lh[i] - line, fill[i]
    printf "  </g>\n"
    printf "  <rect x=\"%.2f\" y=\"%.2f\" width=\"%.2f\" height=\"%.2f\" rx=\"%.2f\" fill=\"none\" stroke=\"#111111\" stroke-width=\"%.2f\"/>\n", \
        inset, inset, size - line, size - line, 14 - inset, line
    printf "</svg>\n"
}

# Splits a tile across its longer side until depth runs out or it is small,
# recording the leaves.
function tile(x, y, w, h, depth,    r, a) {
    if (depth == 0 || w < 16 || h < 16) {
        lx[n] = x; ly[n] = y; lw[n] = w; lh[n] = h; n++
        return
    }
    r = ratio[used % nr + 1]; used++
    if (w >= h) {
        a = w * r
        tile(x, y, a, h, depth - 1)
        tile(x + a, y, w - a, h, depth - (a > w / 2 ? 2 : 1))
    } else {
        a = h * r
        tile(x, y, w, a, depth - 1)
        tile(x, y + a, w, h - a, depth - (a > h / 2 ? 2 : 1))
    }
}'
