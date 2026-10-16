//! Turn [`GraphCell`]s into glyphs and colours on the screen.

use super::dimming::{CONNECTOR, GLYPH};
use super::layout::{CellKind, GraphCell};
use crate::term::{Color, Screen, Style};

/// Lane colour palette (truecolor). Index with `GraphCell::color`.
pub const PALETTE: [Color; 10] = [
    Color::rgb(0xFF, 0xB8, 0x6C), // orange
    Color::rgb(0x7D, 0xCF, 0xFF), // sky
    Color::rgb(0xB5, 0xF2, 0x7A), // lime
    Color::rgb(0xFF, 0x7B, 0xB4), // pink
    Color::rgb(0xC7, 0xA8, 0xFF), // lavender
    Color::rgb(0x5E, 0xE6, 0xC7), // mint
    Color::rgb(0xFF, 0xE0, 0x6B), // yellow
    Color::rgb(0xFF, 0x8A, 0x80), // coral
    Color::rgb(0x8F, 0xB8, 0xFF), // periwinkle
    Color::rgb(0xA0, 0xE8, 0xA0), // sage
];

pub fn lane_color(idx: u8) -> Color {
    PALETTE[(idx as usize) % PALETTE.len()]
}

/// Width in cells of one lane column.
pub const LANE_WIDTH: u16 = 2;

fn glyph(kind: CellKind, is_head: bool) -> char {
    match kind {
        CellKind::Empty => ' ',
        CellKind::Vertical => '│',
        CellKind::Node => {
            if is_head {
                '◉'
            } else {
                '●'
            }
        }
        CellKind::MergeNode => {
            if is_head {
                '◉'
            } else {
                '○'
            }
        }
        CellKind::Horizontal => '─',
        CellKind::Cross => '┼',
        CellKind::EndFromRight => '╯',
        CellKind::EndFromLeft => '╰',
        CellKind::StartToRight => '╮',
        CellKind::StartToLeft => '╭',
        CellKind::JoinFromLeft => '┤',
        CellKind::JoinFromRight => '├',
    }
}

/// Does this cell draw a horizontal line entering from its left?
fn continues_left(kind: CellKind) -> bool {
    matches!(
        kind,
        CellKind::Horizontal
            | CellKind::Cross
            | CellKind::EndFromRight
            | CellKind::StartToRight
            | CellKind::JoinFromLeft
    )
}

/// Render one row of graph cells at (`x`, `y`) within `max_width` cells.
///
/// Each lane takes two cells: the lane glyph and a connector cell that carries a
/// horizontal line when the next lane continues one.
#[allow(clippy::too_many_arguments)]
pub fn render_row(
    screen: &mut Screen,
    x: u16,
    y: u16,
    cells: &[GraphCell],
    node_lane: u16,
    is_head: bool,
    dimming: &[u8],
    bg: Color,
    max_width: u16,
) {
    let mut cx = x;
    let end = x.saturating_add(max_width);
    for (i, cell) in cells.iter().enumerate() {
        if cx >= end {
            break;
        }
        let kind = cell.kind();
        let color = lane_color(cell.color());
        let flags = dimming.get(i).copied().unwrap_or(0);
        let color = if flags & GLYPH != 0 {
            color.dimmed()
        } else {
            color
        };
        let mut style = Style::new().fg(color).bg(bg);
        if matches!(kind, CellKind::Node | CellKind::MergeNode) {
            style = style.bold();
        }
        let ch = glyph(kind, is_head && i == node_lane as usize);
        screen.put_char(cx, y, ch, style);
        cx += 1;
        if cx >= end {
            break;
        }
        let (conn, conn_color) = match cells.get(i + 1) {
            Some(next) if continues_left(next.kind()) => (
                '─',
                if flags & CONNECTOR != 0 {
                    lane_color(next.color()).dimmed()
                } else {
                    lane_color(next.color())
                },
            ),
            _ => (' ', color),
        };
        screen.put_char(cx, y, conn, Style::new().fg(conn_color).bg(bg));
        cx += 1;
    }
    if cx < end {
        screen.paint_bg(cx, y, end - cx, bg);
    }
}
