//! A first-parent branch track, bounded to the visible graph geometry.
//!
//! Palette colors and lane numbers are reused. Follow starts/ends instead of
//! treating every occurrence of a color as the same branch. No ancestry reads,
//! object access or whole-history scan is needed when the pointer moves.
use super::layout::{CellKind, GraphCell};

pub const GLYPH: u8 = 1;
pub const CONNECTOR: u8 = 2;
// Temporary per-build traversal marks, stripped before exposing render flags.
const FORWARD_VISITED: u8 = 4;
const REVERSE_VISITED: u8 = 8;

#[derive(Clone, Copy)]
pub struct Row<'a> {
    pub cells: &'a [GraphCell],
    pub node_lane: usize,
    pub has_parents: bool,
}

/// Contiguous selection flags: one payload allocation and one row-offset allocation.
#[derive(Default)]
pub struct FlatFlags {
    offsets: Vec<usize>,
    flags: Vec<u8>,
}
impl FlatFlags {
    #[cfg(test)]
    pub fn trace<'a>(
        &mut self,
        count: usize,
        anchor: usize,
        row_at: impl Fn(usize) -> Option<Row<'a>>,
    ) {
        self.trace_many(count, &[anchor], row_at);
    }
    pub fn trace_many<'a>(
        &mut self,
        count: usize,
        anchors: &[usize],
        row_at: impl Fn(usize) -> Option<Row<'a>>,
    ) {
        self.offsets.clear();
        self.offsets.reserve(count + 1);
        self.offsets.push(0);
        let mut total = 0;
        for index in 0..count {
            total += row_at(index).map_or(0, |row| row.cells.len());
            self.offsets.push(total);
        }
        self.flags.clear();
        self.flags.resize(total, 0);
        let shared_paths = anchors.len() > 1;
        for &anchor in anchors {
            if shared_paths {
                follow::<true>(count, anchor, &row_at, self);
            } else {
                follow::<false>(count, anchor, &row_at, self);
            }
        }
        if shared_paths {
            for flag in &mut self.flags {
                *flag &= GLYPH | CONNECTOR;
            }
        }
    }
    pub fn row(&self, index: usize) -> &[u8] {
        let Some(pair) = self.offsets.get(index..index.saturating_add(2)) else {
            return &[];
        };
        &self.flags[pair[0]..pair[1]]
    }
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }
    #[cfg(test)]
    pub fn as_ptr(&self) -> *const u8 {
        self.flags.as_ptr()
    }
}
trait FlagRows {
    fn row_mut(&mut self, index: usize) -> &mut [u8];
}
impl FlagRows for FlatFlags {
    fn row_mut(&mut self, index: usize) -> &mut [u8] {
        &mut self.flags[self.offsets[index]..self.offsets[index + 1]]
    }
}
impl FlagRows for Vec<Vec<u8>> {
    fn row_mut(&mut self, index: usize) -> &mut [u8] {
        &mut self[index]
    }
}

pub fn trace(rows: &[Row<'_>], anchor: usize) -> Vec<Vec<u8>> {
    let mut flags: Vec<_> = rows.iter().map(|row| vec![0; row.cells.len()]).collect();
    follow::<false>(
        rows.len(),
        anchor,
        |index| rows.get(index).copied(),
        &mut flags,
    );
    flags
}

fn follow<'a, const SHARED_PATHS: bool>(
    count: usize,
    anchor: usize,
    row_at: impl Fn(usize) -> Option<Row<'a>>,
    flags: &mut impl FlagRows,
) {
    let Some(origin) = row_at(anchor) else {
        return;
    };
    for reverse in [false, true] {
        let mut lane = origin.node_lane;
        let steps = if reverse { anchor + 1 } else { count - anchor };
        for step in 0..steps {
            let index = if reverse {
                anchor - step
            } else {
                anchor + step
            };
            let Some(row) = row_at(index) else {
                break;
            };
            let Some(cell) = row.cells.get(lane) else {
                break;
            };
            let mark = flags.row_mut(index);
            let visited = if SHARED_PATHS {
                if reverse {
                    REVERSE_VISITED
                } else {
                    FORWARD_VISITED
                }
            } else {
                0
            };
            if SHARED_PATHS && mark[lane] & visited != 0 {
                break;
            }
            use CellKind::*;
            match cell.kind() {
                Empty | Horizontal => break,
                Node | MergeNode => {
                    // Only roots need the extra reverse-boundary check. Keep
                    // the common parent-bearing node path to one test.
                    if !row.has_parents {
                        if !reverse {
                            mark[lane] |= GLYPH | visited;
                            break;
                        }
                        // The anchor root is ours; any earlier root is a
                        // disconnected lane occupant with no outgoing edge.
                        if step != 0 {
                            break;
                        }
                    }
                    mark[lane] |= GLYPH | visited;
                }
                Vertical | Cross | JoinFromLeft | JoinFromRight => mark[lane] |= GLYPH,
                EndFromLeft | EndFromRight if !reverse => {
                    mark[lane] |= GLYPH | visited;
                    span(row.cells, mark, lane, row.node_lane);
                    lane = row.node_lane;
                    mark[lane] |= GLYPH;
                    if !row.has_parents {
                        break;
                    }
                }
                // Backward traversal must not enter a previous lane occupant
                // that already terminated here before our branch began.
                EndFromLeft | EndFromRight => break,
                StartToLeft | StartToRight if reverse => {
                    mark[lane] |= visited;
                    span(row.cells, mark, lane, row.node_lane);
                    mark[row.node_lane] |= GLYPH;
                    break;
                }
                StartToLeft | StartToRight => break,
            }
        }
    }
}
fn span(cells: &[GraphCell], flags: &mut [u8], from: usize, to: usize) {
    for col in from.min(to)..from.max(to) {
        flags[col] |= CONNECTOR;
        if cells[col].kind() == CellKind::Horizontal {
            flags[col] |= GLYPH;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::layout::Layout;

    fn id(n: u8) -> gix::ObjectId {
        gix::ObjectId::from_bytes_or_panic(&[n; 20])
    }

    fn assert_path_nodes(definitions: &[(u8, &[u8])], anchor: usize, expected: &[u8]) {
        let mut layout = Layout::new();
        let geometry: Vec<_> = definitions
            .iter()
            .map(|(n, parents)| {
                layout.push(&id(*n), &parents.iter().map(|n| id(*n)).collect::<Vec<_>>())
            })
            .collect();
        let rows: Vec<_> = geometry
            .iter()
            .zip(definitions)
            .map(|(row, (_, parents))| Row {
                cells: &row.cells,
                node_lane: row.node_lane as usize,
                has_parents: !parents.is_empty(),
            })
            .collect();
        let flags = trace(&rows, anchor);
        let mut flat = FlatFlags::default();
        flat.trace_many(rows.len(), &[anchor, anchor], |i| rows.get(i).copied());
        for (i, (commit, _)) in definitions.iter().enumerate() {
            assert_eq!(
                flags[i][rows[i].node_lane] & GLYPH != 0,
                expected.contains(commit),
                "hover: anchor={} commit={commit}",
                definitions[anchor].0
            );
            assert_eq!(
                flat.row(i)[rows[i].node_lane] & GLYPH != 0,
                expected.contains(commit),
                "selection: anchor={} commit={commit}",
                definitions[anchor].0
            );
        }
    }

    #[test]
    fn backward_trace_stops_before_a_previous_branch_ended_in_the_reused_lane() {
        // Branch 8 -> 7 joins 9 -> 6 at 6, freeing lane 1. A different
        // branch (5 -> 1) reuses lane 1 immediately on the following row.
        let definitions: &[(u8, &[u8])] = &[
            (9, &[6]),
            (8, &[7]),
            (7, &[6]),
            (6, &[1]),
            (5, &[1]),
            (4, &[2]),
            (2, &[1]),
            (1, &[]),
        ];
        assert_path_nodes(definitions, 4, &[5, 1]);
        assert_path_nodes(definitions, 1, &[8, 7, 6, 1]);
    }

    #[test]
    fn backward_trace_stops_before_a_disconnected_root_even_when_colors_repeat() {
        let definitions: Vec<_> = (1..=24).rev().map(|n| (n, &[][..])).collect();
        for anchor in 0..definitions.len() {
            assert_path_nodes(&definitions, anchor, &[definitions[anchor].0]);
        }
        assert_path_nodes(
            &[(9, &[]), (8, &[7]), (7, &[]), (6, &[5]), (5, &[])],
            2,
            &[8, 7],
        );
    }

    #[test]
    fn multi_anchor_flat_flags_equal_the_union_of_independent_paths() {
        let mut layout = Layout::new();
        let definitions = [
            (9, vec![7, 6]),
            (8, vec![6]),
            (7, vec![5]),
            (6, vec![4]),
            (5, vec![3]),
            (4, vec![3]),
            (3, vec![2]),
            (2, vec![1]),
            (1, vec![]),
        ];
        let geometry: Vec<_> = definitions
            .iter()
            .map(|(n, parents)| {
                layout.push(&id(*n), &parents.iter().map(|n| id(*n)).collect::<Vec<_>>())
            })
            .collect();
        let rows: Vec<_> = geometry
            .iter()
            .zip(&definitions)
            .map(|(row, (_, parents))| Row {
                cells: &row.cells,
                node_lane: row.node_lane as usize,
                has_parents: !parents.is_empty(),
            })
            .collect();
        let mut flat = FlatFlags::default();
        for a in 0..rows.len() {
            let left = trace(&rows, a);
            for b in 0..rows.len() {
                let right = trace(&rows, b);
                flat.trace_many(rows.len(), &[a, b], |index| rows.get(index).copied());
                for row in 0..rows.len() {
                    for col in 0..rows[row].cells.len() {
                        assert_eq!(
                            flat.row(row)[col],
                            left[row][col] | right[row][col],
                            "anchors={a},{b}, row={row}, col={col}"
                        );
                    }
                }
            }
        }
        flat.trace_many(rows.len(), &[], |index| rows.get(index).copied());
        assert!((0..rows.len()).all(|row| flat.row(row).iter().all(|flag| *flag == 0)));
    }

    #[test]
    fn parallel_branches_and_reused_lanes_do_not_share_the_hover_track() {
        let mut layout = Layout::new();
        let definitions: &[(u8, &[u8])] = &[
            (6, &[4]),
            (5, &[3]),
            (4, &[2]),
            (3, &[1]),
            (2, &[1]),
            (1, &[]),
            (9, &[]),
        ];
        let geometry: Vec<_> = definitions
            .iter()
            .map(|&(n, parents)| {
                layout.push(&id(n), &parents.iter().map(|&n| id(n)).collect::<Vec<_>>())
            })
            .collect();
        let rows: Vec<_> = geometry
            .iter()
            .zip(definitions)
            .map(|(row, (_, parents))| Row {
                cells: &row.cells,
                node_lane: row.node_lane as usize,
                has_parents: !parents.is_empty(),
            })
            .collect();
        let flags = trace(&rows, 0);
        for index in [0, 2, 4, 5] {
            assert_ne!(flags[index][rows[index].node_lane] & GLYPH, 0);
        }
        for index in [1, 3, 6] {
            assert_eq!(flags[index][rows[index].node_lane] & GLYPH, 0);
        }
        assert!(
            trace(&rows, usize::MAX)
                .iter()
                .flatten()
                .all(|&flag| flag == 0)
        );
    }

    #[test]
    fn new_merge_lane_ends_at_its_origin_instead_of_following_the_other_branch() {
        let mut layout = Layout::new();
        let definitions: &[(u8, &[u8])] = &[
            (8, &[7, 6]),
            (7, &[5]),
            (6, &[4]),
            (5, &[3]),
            (4, &[3]),
            (3, &[]),
        ];
        let geometry: Vec<_> = definitions
            .iter()
            .map(|&(n, parents)| {
                layout.push(&id(n), &parents.iter().map(|&n| id(n)).collect::<Vec<_>>())
            })
            .collect();
        let rows: Vec<_> = geometry
            .iter()
            .zip(definitions)
            .map(|(row, (_, parents))| Row {
                cells: &row.cells,
                node_lane: row.node_lane as usize,
                has_parents: !parents.is_empty(),
            })
            .collect();
        let flags = trace(&rows, 2);
        assert_ne!(flags[0][rows[0].node_lane] & GLYPH, 0);
        assert_eq!(flags[1][rows[1].node_lane] & GLYPH, 0);
        assert_ne!(flags[2][rows[2].node_lane] & GLYPH, 0);
        assert_ne!(flags[4][rows[4].node_lane] & GLYPH, 0);
        assert!(flags[0].iter().any(|flag| flag & CONNECTOR != 0));
    }

    #[test]
    #[ignore = "one warm-up and 15 viewport-bounded branch hover samples"]
    fn benchmark_hover() {
        let cells: Vec<Vec<_>> = (0..80)
            .map(|i| {
                vec![
                    GraphCell::new(
                        if i % 3 == 0 {
                            CellKind::Node
                        } else {
                            CellKind::Vertical
                        },
                        0,
                    ),
                    GraphCell::new(
                        if i % 3 == 1 {
                            CellKind::Node
                        } else {
                            CellKind::Vertical
                        },
                        1,
                    ),
                ]
            })
            .collect();
        let rows: Vec<_> = cells
            .iter()
            .enumerate()
            .map(|(i, cells)| Row {
                cells,
                node_lane: usize::from(i % 3 == 1),
                has_parents: true,
            })
            .collect();
        for sample in -1..15 {
            let started = std::time::Instant::now();
            for anchor in 0..1000 {
                std::hint::black_box(trace(&rows, anchor % rows.len()));
            }
            println!(
                "{}",
                serde_json::json!({"sample": sample, "visibleRows": rows.len(),
                "hoverMs": started.elapsed().as_secs_f64() * 1000. / 1000.})
            );
        }
    }
    #[test]
    fn contiguous_flags_match_hover_traces_for_every_anchor_and_reuse_storage() {
        let mut layout = Layout::new();
        let definitions: &[(u8, &[u8])] = &[
            (9, &[7, 6]),
            (8, &[6]),
            (7, &[5]),
            (6, &[4]),
            (5, &[3]),
            (4, &[3]),
            (3, &[2]),
            (2, &[1]),
            (1, &[]),
        ];
        let geometry: Vec<_> = definitions
            .iter()
            .map(|(n, parents)| {
                let parents: Vec<_> = parents.iter().copied().map(id).collect();
                (layout.push(&id(*n), &parents), !parents.is_empty())
            })
            .collect();
        let rows: Vec<_> = geometry
            .iter()
            .map(|(row, parent)| Row {
                cells: &row.cells,
                node_lane: row.node_lane as usize,
                has_parents: *parent,
            })
            .collect();
        let mut flat = FlatFlags::default();
        for anchor in 0..rows.len() {
            let expected = trace(&rows, anchor);
            flat.trace(rows.len(), anchor, |index| rows.get(index).copied());
            for (index, row) in expected.iter().enumerate() {
                assert_eq!(flat.row(index), row);
            }
            let storage = flat.as_ptr();
            flat.trace(rows.len(), anchor, |index| rows.get(index).copied());
            assert_eq!(flat.as_ptr(), storage);
        }
        assert!(flat.row(rows.len()).is_empty());
        flat.trace(0, 0, |_| None);
        assert_eq!(flat.len(), 0);
    }
}
