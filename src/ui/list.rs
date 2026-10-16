//! The commit list: graph | hash | refs | subject | author | age.

use super::{chip_style, theme, time};
use crate::app::{App, Focus};
use crate::git::RefKind;
use crate::graph::render::{LANE_WIDTH, render_row};
use crate::term::{Color, Rect, Screen, Style};

const HASH_LEN: u16 = 7;
const AUTHOR_W: u16 = 14;
const AGE_W: u16 = 4;
/// Longest ref name shown in a chip before it is truncated.
const MAX_CHIP_W: u16 = 28;

pub fn draw(app: &mut App, screen: &mut Screen, area: Rect, now: i64) {
    if area.w == 0 || area.h == 0 {
        return;
    }
    app.list_rows = area.h as usize;
    let n = app.graph_len();
    // Keep scroll consistent with the current viewport.
    let max_scroll = n.saturating_sub(area.h as usize);
    if app.scroll > max_scroll {
        app.scroll = max_scroll;
    }
    if n == 0 {
        let msg = if app.search.active() {
            if app.search.busy {
                "Searching…"
            } else {
                "No matching commits"
            }
        } else if app.is_walking() {
            "Reading history…"
        } else {
            "No commits"
        };
        screen.put_str(
            area.x + 2,
            area.y + 1,
            msg,
            theme::dim(),
            area.w.saturating_sub(2),
        );
        return;
    }

    let head = app.head_id();
    let show_author = area.w >= 70;
    let show_age = area.w >= 50;

    // Graph column width: enough for the widest visible row, capped to a third of the list.
    let visible = app.scroll..(app.scroll + area.h as usize).min(n);
    let widest = visible
        .clone()
        .filter_map(|i| {
            if app.search.active() {
                return app
                    .search
                    .result
                    .as_ref()?
                    .graph
                    .get(i)
                    .map(|(cells, ..)| cells.len() as u16);
            }
            app.history_index(i)
                .or(app.pending.head_index)
                .and_then(|i| app.history.commits.get(i))
                .map(|rec| rec.row.len)
        })
        .max()
        .unwrap_or(1)
        .max(1);
    // Lanes beyond the cap are clipped; a one-cell gap always separates graph and hash
    // (the trailing connector cell provides it when nothing is clipped).
    let cap = (area.w / 3).max(LANE_WIDTH);
    let full_w = widest * LANE_WIDTH;
    let (graph_w, gap) = if full_w <= cap {
        (full_w, 0)
    } else {
        (cap - 1, 1)
    };

    let right_cols =
        (if show_author { AUTHOR_W + 1 } else { 0 }) + if show_age { AGE_W + 1 } else { 0 };

    for (row, i) in visible.enumerate() {
        let y = area.y + row as u16;
        let selected = i == app.graph_selected();
        let bg = if selected {
            if app.focus == Focus::List {
                theme::BG_SELECTED
            } else {
                theme::BG_SELECTED_INACTIVE
            }
        } else {
            Color::Default
        };
        if selected {
            screen.paint_bg(area.x, y, area.w, bg);
        }
        let Some(index) = app.history_index(i) else {
            draw_pending(app, screen, area.x, y, graph_w + gap, area.w, bg);
            continue;
        };
        let graph_row = i;
        let i = index;
        let rec = app.history.commits[i];
        let ghost = app.ghosts.contains_key(&rec.id);
        let mut x = area.x;

        // Graph.
        let is_head = head.is_some_and(|h| h == rec.id);
        let filtered = app
            .search
            .result
            .as_ref()
            .filter(|_| app.search.active())
            .and_then(|r| r.graph.get(graph_row));
        const SEARCH_NODE: [crate::graph::layout::GraphCell; 1] =
            [crate::graph::layout::GraphCell::new(
                crate::graph::layout::CellKind::Node,
                0,
            )];
        render_row(
            screen,
            x,
            y,
            filtered.map_or_else(
                || {
                    if app.search.active() {
                        &SEARCH_NODE[..]
                    } else {
                        app.history.row_cells(i)
                    }
                },
                |(cells, ..)| cells.as_slice(),
            ),
            filtered.map_or(
                if app.search.active() {
                    0
                } else {
                    rec.node_lane
                },
                |(_, lane, _)| *lane,
            ),
            is_head,
            if let Some((_, _, flags)) = filtered {
                flags
            } else if app.search.active() {
                if ghost {
                    &[crate::graph::dimming::GLYPH]
                } else {
                    &[]
                }
            } else {
                app.history.remote_cells.get(rec.row.range()).unwrap_or(&[])
            },
            bg,
            graph_w,
        );
        x += graph_w + gap;
        if x >= area.right() {
            continue;
        }

        // Short hash; marked commits show it as a chip.
        let hash = rec.id.to_hex_with_len(HASH_LEN as usize).to_string();
        let hash_style = if app.is_marked(&rec.id) {
            Style::new()
                .fg(Color::rgb(0x12, 0x14, 0x1a))
                .bg(theme::BG_MARK)
                .bold()
        } else {
            Style::new().fg(theme::FG_HASH).bg(bg)
        };
        x += screen.put_str(x, y, &hash, hash_style, area.right() - x);
        x += screen.put_str(x, y, " ", Style::new().bg(bg), area.right() - x);

        let text_end = area.right().saturating_sub(right_cols);

        // Ref labels as chips: ` name ` on a coloured background.
        if let Some(labels) = app.labels.get(&rec.id) {
            for label in labels {
                if x + 5 >= text_end {
                    break;
                }
                let mut style = chip_style(label.kind);
                if ghost && label.kind == RefKind::Remote {
                    style.bg = style.bg.dimmed();
                }
                let max = (text_end - x).saturating_sub(3).min(MAX_CHIP_W);
                x += screen.put_str(x, y, " ", style, 1);
                x += screen.put_str_ellipsis(x, y, &label.name, style, max);
                x += screen.put_str(x, y, " ", style, 1);
                x += screen.put_str(x, y, " ", Style::new().bg(bg), text_end.saturating_sub(x));
            }
        }

        // Subject.
        if x < text_end {
            let subject = app.history.subject(i);
            let style = if selected {
                theme::text().bg(bg).bold()
            } else {
                theme::text().bg(bg)
            };
            screen.put_str_ellipsis(x, y, subject, style, text_end - x);
        }

        // Author and age, right-aligned.
        let mut rx = area.right();
        if show_age {
            rx -= AGE_W;
            let age = time::format_relative(rec.author_time, now);
            let pad = AGE_W.saturating_sub(age.len() as u16);
            screen.put_str(
                rx + pad,
                y,
                &age,
                Style::new().fg(theme::FG_DATE).bg(bg),
                AGE_W,
            );
            rx -= 1;
        }
        if show_author {
            rx -= AUTHOR_W;
            let author = app.history.author(i);
            screen.put_str_ellipsis(
                rx,
                y,
                author,
                Style::new().fg(theme::FG_AUTHOR).bg(bg),
                AUTHOR_W,
            );
        }
    }
}

fn draw_pending(
    app: &App,
    screen: &mut Screen,
    x: u16,
    y: u16,
    graph_w: u16,
    width: u16,
    bg: Color,
) {
    use crate::graph::layout::CellKind;
    let style = Style::new().fg(theme::FG_MODIFIED).bg(bg).bold();
    let lane = app
        .pending
        .head_index
        .and_then(|i| app.history.commits.get(i))
        .map_or(0, |r| r.node_lane);
    if let Some(index) = app.pending.head_index {
        for (column, cell) in app.history.row_cells(index).iter().enumerate() {
            if (column as u16) * LANE_WIDTH >= graph_w {
                break;
            }
            if matches!(
                cell.kind(),
                CellKind::Vertical
                    | CellKind::Cross
                    | CellKind::JoinFromLeft
                    | CellKind::JoinFromRight
            ) {
                screen.put_char(
                    x + column as u16 * LANE_WIDTH,
                    y,
                    '│',
                    Style::new()
                        .fg(crate::graph::render::lane_color(cell.color()))
                        .bg(bg),
                );
            }
        }
    }
    if lane * LANE_WIDTH < graph_w {
        screen.put_char(x + lane * LANE_WIDTH, y, '◌', style);
    }
    let label = format!(
        "worktree Pending changes ({} files)",
        app.pending.snapshot.files.len()
    );
    screen.put_str_ellipsis(x + graph_w, y, &label, style, width.saturating_sub(graph_w));
}
