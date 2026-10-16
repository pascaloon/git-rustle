//! Derived edge styling, computed once when a remote preview is assembled.
//!
//! Follow lane geometry, not palette indices: colors and columns are reused.
//! Two sweeps mark edges with a remote child or parent, including connectors
//! passing through rows whose own commit is local.

use gix::ObjectId;

use super::layout::{CellKind, GraphCell};
use crate::model::History;

pub const GLYPH: u8 = 1;
pub const CONNECTOR: u8 = 2;

pub fn build(history: &History, is_remote: impl Fn(&ObjectId) -> bool) -> Vec<u8> {
    let remote: Vec<bool> = history.commits.iter().map(|c| is_remote(&c.id)).collect();
    let Some(first) = remote.iter().position(|&v| v) else {
        return Vec::new();
    };
    let last = remote.iter().rposition(|&v| v).unwrap();
    let mut flags = Vec::new();
    let mut lanes = Vec::new();
    for reverse in [false, true] {
        lanes.clear();
        let steps = if reverse {
            last + 1
        } else {
            history.len() - first
        };
        for step in 0..steps {
            let i = if reverse { last - step } else { first + step };
            // Once outside the remote rows, stop as soon as every affected
            // edge has reached its endpoint. Old local history can be huge.
            if ((reverse && i < first) || (!reverse && i > last))
                && !lanes.iter().any(|&active| active)
            {
                break;
            }
            let commit = &history.commits[i];
            let cells = history.row_cells(i);
            let range = commit.row.range();
            if flags.len() < range.end {
                flags.resize(range.end, 0);
            }
            let row_flags = &mut flags[range];
            lanes.resize(cells.len(), false);
            for (col, cell) in cells.iter().enumerate() {
                use CellKind::*;
                let active = lanes[col];
                let dim = match cell.kind() {
                    Node | MergeNode => {
                        lanes[col] = remote[i];
                        remote[i]
                    }
                    Vertical | Cross => active,
                    EndFromLeft | EndFromRight => {
                        lanes[col] = reverse && remote[i];
                        let dim = if reverse { remote[i] } else { active };
                        if dim {
                            mark_span(cells, row_flags, col, commit.node_lane as usize);
                        }
                        dim
                    }
                    StartToLeft | StartToRight | JoinFromLeft | JoinFromRight => {
                        let start = matches!(cell.kind(), StartToLeft | StartToRight);
                        let dim = if reverse { active } else { remote[i] };
                        lanes[col] = if reverse {
                            !start && active
                        } else {
                            remote[i] || (!start && active)
                        };
                        if dim {
                            mark_span(cells, row_flags, col, commit.node_lane as usize);
                        }
                        dim || (!start && active)
                    }
                    Empty | Horizontal => {
                        lanes[col] = false;
                        false
                    }
                };
                if dim {
                    row_flags[col] |= GLYPH;
                }
            }
        }
    }
    flags
}

fn mark_span(cells: &[GraphCell], flags: &mut [u8], a: usize, b: usize) {
    for col in a.min(b)..a.max(b) {
        flags[col] |= CONNECTOR;
        if cells[col].kind() == CellKind::Horizontal {
            flags[col] |= GLYPH;
        }
    }
    // A crossing glyph keeps the vertical lane's style; its horizontal
    // connectors have their own flags. Local commit nodes stay bright too.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{
        layout::Layout,
        render::{lane_color, render_row},
    };
    use crate::model::CommitRec;
    use crate::term::{Color, Screen};

    fn id(n: u8) -> ObjectId {
        ObjectId::from_bytes_or_panic(&[n; 20])
    }

    fn history(rows: &[(u8, &[u8])]) -> History {
        let mut history = History::default();
        let mut layout = Layout::new();
        for &(n, parents) in rows {
            let parents: Vec<_> = parents.iter().map(|&p| id(p)).collect();
            let row = layout.push(&id(n), &parents);
            let cells = history.push_cells(&row.cells);
            let parents = history.push_parents(&parents);
            history.commits.push(CommitRec {
                id: id(n),
                parents,
                author_time: 0,
                author_offset: 0,
                commit_time: 0,
                author: Default::default(),
                subject: Default::default(),
                row: cells,
                node_lane: row.node_lane,
            });
        }
        history
    }

    fn assert_rows(history: &History, remote: u8, expected: &[&[u8]]) {
        let flags = build(history, |oid| *oid == id(remote));
        for (i, expected) in expected.iter().enumerate() {
            let actual: Vec<_> = history.commits[i]
                .row
                .range()
                .map(|col| flags.get(col).copied().unwrap_or(0))
                .collect();
            assert_eq!(actual, *expected, "row {i}");
        }
    }

    #[test]
    fn remote_merge_dims_other_parent_colors_and_intervening_rows() {
        let h = history(&[(4, &[3, 2]), (3, &[1]), (2, &[1]), (1, &[])]);
        assert_rows(&h, 4, &[&[3, 1], &[0, 1], &[0, 0], &[0, 0]]);
        let flags = build(&h, |oid| *oid == id(4));
        let mut screen = Screen::new(4, 2);
        for i in 0..2 {
            render_row(
                &mut screen,
                0,
                i as u16,
                h.row_cells(i),
                h.commits[i].node_lane,
                false,
                &flags[h.commits[i].row.range()],
                Color::Default,
                4,
            );
        }
        let cells = screen.test_cells();
        assert_eq!(cells[1].ch, '─');
        assert_eq!(
            cells[1].style.fg,
            lane_color(h.row_cells(0)[1].color()).dimmed()
        );
        assert_eq!(cells[2].ch, '╮');
        assert_eq!(cells[2].style.fg, cells[1].style.fg);
        assert_eq!(cells[6].ch, '│');
        assert_eq!(cells[6].style.fg, cells[1].style.fg);
        assert_eq!(cells[4].style.fg, lane_color(h.row_cells(1)[0].color()));
    }

    #[test]
    fn edges_toward_remote_parent_and_into_local_parent_are_dimmed() {
        let h = history(&[(4, &[3, 2]), (3, &[1]), (2, &[1]), (1, &[])]);
        assert_rows(&h, 2, &[&[2, 1], &[0, 1], &[0, 1], &[2, 1]]);
    }

    #[test]
    fn crossing_preserves_local_vertical_lane() {
        let h = history(&[
            (10, &[1]),
            (11, &[2]),
            (12, &[3, 1]),
            (3, &[]),
            (2, &[]),
            (1, &[]),
        ]);
        let flags = build(&h, |oid| *oid == id(12));
        assert_eq!(h.row_cells(2)[1].kind(), CellKind::Cross);
        assert_eq!(&flags[h.commits[2].row.range()], &[3, 2, 1]);
        assert_eq!(&flags[h.commits[3].row.range()], &[1, 0, 0]);
    }

    #[test]
    fn reused_lanes_and_palette_colors_do_not_inherit_dimming() {
        let mut rows: Vec<(u8, &[u8])> = vec![(30, &[29]), (29, &[])];
        rows.extend((1..=20).map(|n| (n, &[] as &[u8])));
        let h = history(&rows);
        let flags = build(&h, |oid| *oid == id(30));
        assert_eq!(flags[0], GLYPH);
        assert!(flags[1..].iter().all(|&v| v == 0));
        assert!(build(&h, |_| false).is_empty());
    }
}
