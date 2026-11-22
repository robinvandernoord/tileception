//! Where the Overview page's cards go: a grid laid out by Taffy, so the
//! cards are described by the columns and rows they take rather than by
//! offsets worked out by hand.

use taffy::prelude::*;
use tileception_core::Rect;

use crate::ui::Layout;

/// The space between cards, across and down.
const GAP: f32 = 12.;
/// Narrower than this, the grid has two columns rather than three.
const NARROW: f64 = 700.;

/// How many columns and rows a card spans. Each card goes in the first
/// place it fits, row by row, as in a CSS grid.
type Span = (u16, u16);

/// The cards, one for each of the first pages in `CATEGORIES` (card `i`
/// opens page `i`), in three columns: Appearance, with its preview, is two
/// rows high.
const WIDE: [Span; 10] = [(1, 2), (1, 1), (1, 1), (2, 1), (1, 1), (1, 1), (1, 1), (2, 1), (1, 1), (3, 1)];

/// The same cards in two columns.
const TWO: [Span; 10] = [(2, 1), (1, 1), (1, 1), (2, 1), (1, 1), (1, 1), (1, 1), (1, 1), (2, 1), (2, 1)];

/// Rows of the two-column grid, and of the three-column one below its
/// first two.
const ROW: f64 = 150.;
/// The first two rows of three columns are a quarter of the height to show
/// the cards in, less this for the gaps between them, and at most `TALL`.
const BETWEEN_ROWS: f64 = 40.;
const TALL: f64 = 176.;

/// The cards of an Overview `width` wide, whose top is at `origin`, with
/// about `height` to show them in: the first two rows of three columns
/// grow with it a little.
pub(super) fn cards(origin: (f64, f64), width: f64, height: f64) -> Vec<Rect> {
    let (columns, rows, spans): (usize, Vec<f64>, &[Span]) = if width < NARROW {
        (2, vec![2. * ROW, ROW, ROW, ROW, ROW, ROW, ROW], &TWO)
    } else {
        let top = ((height - BETWEEN_ROWS) / 4.).clamp(ROW, TALL);
        (3, vec![top, top, ROW, ROW, ROW], &WIDE)
    };
    let mut layout = Layout::new(origin);
    let cards: Vec<_> = spans
        .iter()
        .map(|&(across, down)| {
            layout.leaf(Style {
                grid_column: Line { start: auto(), end: span(across) },
                grid_row: Line { start: auto(), end: span(down) },
                ..Default::default()
            })
        })
        .collect();
    let grid = layout.node(
        Style {
            display: Display::Grid,
            size: Size { width: length(width as f32), height: auto() },
            gap: length(GAP),
            // Columns of exactly equal width.
            grid_template_columns: vec![flex(1.); columns],
            grid_template_rows: rows.into_iter().map(|row| length(row as f32)).collect(),
            ..Default::default()
        },
        &cards,
    );
    let placed = layout.compute(grid);
    cards.iter().map(|card| placed.rect(*card)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTHS: [f64; 6] = [480., 699.5, 700., 1100.37, 1480., 2200.];

    fn near(left: f64, right: f64) -> bool {
        (left - right).abs() < 0.01
    }

    #[test]
    fn every_card_opens_a_page() {
        assert!(WIDE.len() == TWO.len() && WIDE.len() <= super::super::CATEGORIES.len());
    }

    #[test]
    fn cards_fill_the_width_in_equal_columns() {
        for width in WIDTHS {
            let cards = cards((40., 30.), width, 700.);
            let columns = if width < NARROW { 2. } else { 3. };
            let column = (width - f64::from(GAP) * (columns - 1.)) / columns;
            for card in &cards {
                assert!(card.x >= 40. - 0.01 && card.right() <= 40. + width + 0.01, "{card:?} at width {width}");
                let spans = (card.w + f64::from(GAP)) / (column + f64::from(GAP));
                assert!(near(spans, spans.round()), "{card:?} is not whole columns at width {width}");
            }
            let last = cards[cards.len() - 1];
            assert!(near(last.w, width), "the last card spans the width: {last:?}");
        }
    }

    #[test]
    fn rows_have_their_heights() {
        let heights = |width, height| cards((0., 0.), width, height).iter().map(|card| card.h).collect::<Vec<_>>();
        let two = heights(600., 700.);
        assert!(near(two[0], 2. * ROW) && two[1..].iter().all(|card| near(*card, ROW)), "{two:?}");
        for (height, top) in [(400., ROW), (700., (700. - BETWEEN_ROWS) / 4.), (2000., TALL)] {
            let three = heights(1100., height);
            assert!(near(three[1], top) && near(three[3], top), "at height {height}: {three:?}");
            assert!(three[4..].iter().all(|card| near(*card, ROW)), "at height {height}: {three:?}");
        }
    }

    #[test]
    fn appearance_takes_two_rows_in_three_columns() {
        for height in [500., 700., 900.] {
            let cards = cards((0., 0.), 1100., height);
            assert!(near(cards[0].h, cards[1].h * 2. + f64::from(GAP)), "{:?} beside {:?}", cards[0], cards[1]);
        }
    }

    #[test]
    fn cards_never_touch() {
        for width in WIDTHS {
            let cards = cards((0., 0.), width, 700.);
            for (index, card) in cards.iter().enumerate() {
                for other in &cards[index + 1..] {
                    let apart = card.right() + 11.99 <= other.x
                        || other.right() + 11.99 <= card.x
                        || card.bottom() + 11.99 <= other.y
                        || other.bottom() + 11.99 <= card.y;
                    assert!(apart, "at width {width}: {card:?} and {other:?} are closer than the gap");
                }
            }
        }
    }
}
