//! Digital rain behind the login and lock screens (`wallpaper_fallback =
//! "rain"`, and `backdrop = "rain"` in a session's `[lock]`): streams of
//! quadrant blocks falling down a grid of cells. Quadrant blocks are tiny
//! tilings, so the rain is drawn as rectangles and needs no font.
//!
//! What is lit is a function of time alone, so drawing keeps no state, and
//! with reduced motion every frame shows the same moment.

use tileception_core::Rect;

/// About how many cells fit across the screen; the cell size follows.
const ACROSS: f64 = 86.0;
const MIN_CELL: f64 = 12.0;
/// Streams per column, each on a cycle of its own.
const STREAMS: u64 = 2;
/// Steps per second. The rain moves in whole steps, so frames between them
/// repeat the last and need not be drawn again.
const RATE: f64 = 30.0;
/// The moment shown with reduced motion.
const STILL_AT: f64 = 7.5;

/// The quadrant blocks the rain is made of, as masks: 1 is the top left
/// quarter, 2 the top right, 4 the bottom left and 8 the bottom right.
/// ▘ ▝ ▖ ▗ ▚ ▞ ▀ ▄ ▌ ▐ ▛ ▜ ▙ ▟; the full block is left out.
const GLYPHS: [u8; 14] = [1, 2, 4, 8, 9, 6, 3, 12, 5, 10, 7, 11, 13, 14];

/// The cells covering an area, from its top left corner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    pub left: f64,
    pub top: f64,
    pub cell: f64,
    pub cols: usize,
    pub rows: usize,
}

impl Grid {
    pub fn new(area: Rect) -> Grid {
        let cell = (area.w / ACROSS).max(MIN_CELL).round();
        let count = |length: f64| (length.max(0.0) / cell).ceil() as usize;
        Grid { left: area.x, top: area.y, cell, cols: count(area.w), rows: count(area.h) }
    }

    pub fn rect(&self, col: usize, row: usize) -> Rect {
        Rect::new(self.left + col as f64 * self.cell, self.top + row as f64 * self.cell, self.cell, self.cell)
    }
}

/// A lit cell: 1 at a stream's head, fading along its tail.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lit {
    pub bright: f32,
    pub head: bool,
    pub glyph: u8,
}

/// The moment the rain shows at `seconds`: in whole steps, or held still.
pub fn moment(seconds: f64, still: bool) -> f64 {
    if still { STILL_AT } else { (seconds.max(0.0) * RATE).floor() / RATE }
}

/// SplitMix64: a well spread value for each input, so the rain needs no
/// random number generator and comes out the same every time.
fn hash(value: u64) -> u64 {
    let mut value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn seed(first: u64, second: u64, third: u64) -> u64 {
    hash(first ^ hash(second ^ hash(third)))
}

/// A number from 0 up to 1 for a seed.
fn unit(seed: u64) -> f64 {
    (hash(seed) >> 11) as f64 / (1u64 << 53) as f64
}

/// The glyph of a cell. It changes now and then on its own, and with each
/// stream that passes (`pass`).
pub fn glyph(col: usize, row: usize, pass: u64, moment: f64) -> u8 {
    let cell = seed(col as u64, row as u64, 7);
    let flicker = (moment * 0.6 + unit(cell) * 10.0).floor() as u64;
    GLYPHS[(seed(cell, pass, flicker) % GLYPHS.len() as u64) as usize]
}

/// The lit cells of one column, by row, at `moment` seconds.
pub fn column(col: usize, rows: usize, moment: f64) -> Vec<Option<Lit>> {
    let mut cells = vec![None; rows];
    let column = col as u64;
    for stream in 0..STREAMS {
        // Rows a second, and how many rows its tail is long.
        let speed = 9.0 + 16.0 * unit(seed(column, stream, 1));
        let length = 6.0 + 14.0 * unit(seed(column, stream, 2));
        let period = (rows as f64 + length + 1.0) / speed + 0.5 + 4.0 * unit(seed(column, stream, 3));
        let since = moment.max(0.0) + period * unit(seed(column, stream, 4));
        let pass = (since / period).floor() as u64;
        let head = (since - pass as f64 * period) * speed - 1.0;
        let first = (head - length).ceil().max(0.0) as usize;
        let last = head.floor();
        if last < 0.0 {
            continue;
        }
        for (row, cell) in cells.iter_mut().enumerate().take((last as usize + 1).min(rows)).skip(first) {
            let behind = head - row as f64;
            let bright = (1.0 - behind / length) as f32;
            if cell.is_some_and(|lit: Lit| lit.bright >= bright) {
                continue;
            }
            let pass = seed(stream, pass, 11);
            *cell = Some(Lit { bright, head: behind < 1.0, glyph: glyph(col, row, pass, moment) });
        }
    }
    cells
}

/// Where a glyph's quarters go in `cell`, a little apart; two quarters side
/// by side are one rectangle.
pub fn quadrants(mask: u8, cell: Rect) -> Vec<Rect> {
    let gap = (cell.w * 0.1).max(1.0);
    let half = cell.w / 2.0;
    let mut out = Vec::new();
    for (row, (left, right)) in [(1, 2), (4, 8)].into_iter().enumerate() {
        let top = cell.y + row as f64 * half + gap / 2.0;
        match (mask & left != 0, mask & right != 0) {
            (true, true) => out.push(Rect::new(cell.x + gap / 2.0, top, cell.w - gap, half - gap)),
            (true, false) => out.push(Rect::new(cell.x + gap / 2.0, top, half - gap, half - gap)),
            (false, true) => out.push(Rect::new(cell.x + half + gap / 2.0, top, half - gap, half - gap)),
            (false, false) => {}
        }
    }
    out
}

/// A character of the clock in cells, seven rows high: its width and, per
/// row, which cells are on, the leftmost in the highest bit.
fn character(character: char) -> Option<(u8, [u8; 7])> {
    Some(match character.to_ascii_uppercase() {
        '0' => (5, [0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e]),
        '1' => (5, [0x04, 0x0c, 0x04, 0x04, 0x04, 0x04, 0x0e]),
        '2' => (5, [0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f]),
        '3' => (5, [0x1f, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0e]),
        '4' => (5, [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02]),
        '5' => (5, [0x1f, 0x10, 0x1e, 0x01, 0x01, 0x11, 0x0e]),
        '6' => (5, [0x06, 0x08, 0x10, 0x1e, 0x11, 0x11, 0x0e]),
        '7' => (5, [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08]),
        '8' => (5, [0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e]),
        '9' => (5, [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x02, 0x0c]),
        ':' => (2, [0x0, 0x3, 0x3, 0x0, 0x3, 0x3, 0x0]),
        '.' => (2, [0x0, 0x0, 0x0, 0x0, 0x0, 0x3, 0x3]),
        ' ' => (2, [0x0; 7]),
        'A' => (5, [0x0e, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11]),
        'P' => (5, [0x1e, 0x11, 0x11, 0x1e, 0x10, 0x10, 0x10]),
        'M' => (5, [0x11, 0x1b, 0x15, 0x15, 0x11, 0x11, 0x11]),
        _ => return None,
    })
}

/// The cells of `grid` that spell `text` large in the middle of `area`, by
/// row then column; none when `text` has a character the clock cannot
/// draw, or it does not fit.
pub fn carve(text: &str, grid: &Grid, area: Rect) -> Option<Vec<bool>> {
    let characters: Vec<(u8, [u8; 7])> = text.chars().map(character).collect::<Option<_>>()?;
    if characters.is_empty() {
        return None;
    }
    let width: usize = characters.iter().map(|(width, _)| usize::from(*width) + 1).sum::<usize>() - 1;
    // The area in whole cells of the grid.
    let left = ((area.x - grid.left) / grid.cell).ceil().max(0.0) as usize;
    let top = ((area.y - grid.top) / grid.cell).ceil().max(0.0) as usize;
    let right = (((area.right() - grid.left) / grid.cell).floor().max(0.0) as usize).min(grid.cols);
    let bottom = (((area.bottom() - grid.top) / grid.cell).floor().max(0.0) as usize).min(grid.rows);
    let (across, down) = (right.saturating_sub(left), bottom.saturating_sub(top));
    // Each of the clock's cells is a square of the grid's, as big as fits.
    let size = ((across as f64 * 0.8 / width as f64).min(down as f64 * 0.75 / 7.0)).floor() as usize;
    if size == 0 {
        return None;
    }
    let first_col = left + (across - width * size) / 2;
    let first_row = top + (down - 7 * size) / 2;
    let mut cells = vec![false; grid.cols * grid.rows];
    let mut offset = 0;
    for (char_width, rows) in characters {
        for (row, bits) in rows.iter().enumerate() {
            for col in 0..usize::from(char_width) {
                if bits & (1 << (char_width as usize - 1 - col)) == 0 {
                    continue;
                }
                for down in 0..size {
                    for across in 0..size {
                        let grid_row = first_row + row * size + down;
                        let grid_col = first_col + (offset + col) * size + across;
                        cells[grid_row * grid.cols + grid_col] = true;
                    }
                }
            }
        }
        offset += usize::from(char_width) + 1;
    }
    Some(cells)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    #[test]
    fn the_grid_covers_the_area() {
        let grid = Grid::new(Rect::new(0.0, 0.0, 1920.0, 1080.0));
        assert_eq!(grid.cell, 22.0);
        assert!(grid.cols as f64 * grid.cell >= 1920.0 && grid.rows as f64 * grid.cell >= 1080.0);
        // A small window keeps cells big enough to see.
        assert_eq!(Grid::new(Rect::new(0.0, 0.0, 400.0, 300.0)).cell, MIN_CELL);
    }

    #[test]
    fn the_rain_moves_in_steps_and_holds_still() {
        assert_eq!(moment(1.0 + 0.01, false), moment(1.0 + 0.02, false));
        assert_ne!(column(3, 40, moment(1.0, false)), column(3, 40, moment(1.2, false)));
        assert_eq!(moment(0.0, true), moment(500.0, true));
        // The same moment draws the same rain.
        assert_eq!(column(5, 40, 3.0), column(5, 40, 3.0));
    }

    #[test]
    fn some_of_the_screen_is_lit_and_most_is_dark() {
        let rows = 50;
        for seconds in [0.0, 2.5, 7.5, 61.0] {
            let lit: usize = (0..86).map(|col| column(col, rows, seconds).iter().flatten().count()).sum();
            let share = lit as f64 / (86 * rows) as f64;
            assert!((0.05..0.45).contains(&share), "{share} lit at {seconds}");
        }
    }

    #[test]
    fn a_stream_is_brightest_at_its_head() {
        let heads: Vec<(usize, Vec<Option<Lit>>)> = (0..86)
            .map(|col| (col, column(col, 50, 4.0)))
            .filter(|(_, cells)| cells.iter().flatten().any(|lit| lit.head))
            .collect();
        assert!(!heads.is_empty());
        for (_, cells) in heads {
            for (row, lit) in cells.iter().enumerate().filter_map(|(row, cell)| Some((row, (*cell)?))) {
                if !lit.head {
                    continue;
                }
                // Brighter than the cell of its tail right above it.
                assert!(lit.bright > 0.8);
                let above = row.checked_sub(1).and_then(|above| cells[above]);
                assert!(above.is_none_or(|above| above.head || above.bright < lit.bright));
            }
        }
    }

    #[test]
    fn glyphs_are_quarters_of_the_cell() {
        let cell = Rect::new(10.0, 20.0, 20.0, 20.0);
        for mask in GLYPHS {
            let parts = quadrants(mask, cell);
            assert!((1..=2).contains(&parts.len()), "{mask}");
            for part in parts {
                assert!(part.x >= cell.x && part.y >= cell.y && part.right() <= cell.right());
                assert!(part.bottom() <= cell.bottom());
            }
        }
        // ▀ is one bar along the top half, ▚ two corners.
        assert_eq!(quadrants(3, cell), vec![Rect::new(11.0, 21.0, 18.0, 8.0)]);
        assert_eq!(quadrants(9, cell), vec![Rect::new(11.0, 21.0, 8.0, 8.0), Rect::new(21.0, 31.0, 8.0, 8.0)]);
    }

    #[test]
    fn the_clock_is_carved_in_the_middle_of_its_area() {
        let grid = Grid::new(Rect::new(0.0, 0.0, 1920.0, 1080.0));
        let area = Rect::new(0.0, 34.0, 1920.0, 500.0);
        let cells = carve("12:34", &grid, area).unwrap();
        let carved: Vec<(usize, usize)> = (0..cells.len())
            .filter(|index| cells[*index])
            .map(|index| (index % grid.cols, index / grid.cols))
            .collect();
        let (left, right) =
            (carved.iter().map(|cell| cell.0).min().unwrap(), carved.iter().map(|cell| cell.0).max().unwrap());
        let (top, bottom) =
            (carved.iter().map(|cell| cell.1).min().unwrap(), carved.iter().map(|cell| cell.1).max().unwrap());
        // Seven rows of the font at two cells each, inside the area and
        // about as far from either side.
        assert_eq!(bottom - top + 1, 14);
        assert!(grid.rect(left, top).y >= area.y && grid.rect(right, bottom).bottom() <= area.bottom());
        assert!(left.abs_diff(grid.cols - 1 - right) <= 2);
        // 12-hour clocks work too, others draw nothing rather than half.
        assert!(carve("9:41 pm", &grid, area).is_some());
        assert!(carve("Tue 9:41", &grid, area).is_none());
        assert!(carve("12:34", &grid, Rect::new(0.0, 0.0, 1920.0, 20.0)).is_none());
    }
}
