//! Incremental lane assignment for a commit stream in topological/date order.
//!
//! Each commit becomes one row. Lanes are columns; a lane "expects" the next
//! commit (a parent) that will appear in it. The algorithm is a pure function of
//! the stream so it behaves identically live and when replayed.

use gix::ObjectId;

/// Kind of glyph in one graph column. Stored as `u8` in the cache.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellKind {
    Empty = 0,
    /// Lane passes straight through this row.
    Vertical = 1,
    /// The commit of this row (single parent or root).
    Node = 2,
    /// The commit of this row, a merge (more than one parent).
    MergeNode = 3,
    /// Horizontal connector between the node and a lane.
    Horizontal = 4,
    /// A vertical lane crossed by a horizontal connector.
    Cross = 5,
    /// A lane to the right of the node ends here, bending toward the node.
    EndFromRight = 6,
    /// A lane to the left of the node ends here, bending toward the node.
    EndFromLeft = 7,
    /// A new lane to the right of the node starts here (second parent of a merge).
    StartToRight = 8,
    /// A new lane to the left of the node starts here.
    StartToLeft = 9,
    /// An existing lane to the right of the node receives a connector from the node.
    JoinFromLeft = 10,
    /// An existing lane to the left of the node receives a connector from the node.
    JoinFromRight = 11,
}

impl CellKind {
    pub fn from_u8(v: u8) -> CellKind {
        match v {
            1 => CellKind::Vertical,
            2 => CellKind::Node,
            3 => CellKind::MergeNode,
            4 => CellKind::Horizontal,
            5 => CellKind::Cross,
            6 => CellKind::EndFromRight,
            7 => CellKind::EndFromLeft,
            8 => CellKind::StartToRight,
            9 => CellKind::StartToLeft,
            10 => CellKind::JoinFromLeft,
            11 => CellKind::JoinFromRight,
            _ => CellKind::Empty,
        }
    }
}

/// One graph column: kind in the high nibble, colour index in the low nibble.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphCell(u8);

impl GraphCell {
    pub const EMPTY: GraphCell = GraphCell(0);

    #[inline]
    pub const fn new(kind: CellKind, color: u8) -> Self {
        GraphCell(((kind as u8) << 4) | (color & 0x0F))
    }
    #[inline]
    pub fn kind(self) -> CellKind {
        CellKind::from_u8(self.0 >> 4)
    }
    #[inline]
    pub const fn color(self) -> u8 {
        self.0 & 0x0F
    }
    #[inline]
    pub const fn raw(self) -> u8 {
        self.0
    }
    #[inline]
    pub fn from_raw(raw: u8) -> Self {
        // Normalise unknown kinds to Empty so cached data can never produce an invalid cell.
        GraphCell::new(CellKind::from_u8(raw >> 4), raw & 0x0F)
    }
}

/// Number of distinct lane colours (see `graph::render::PALETTE`); must fit in a nibble.
pub const COLOR_COUNT: u8 = 10;

#[derive(Debug, Clone, Copy)]
struct Lane {
    expected: ObjectId,
    color: u8,
}

/// One laid-out row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphRow {
    pub node_lane: u16,
    pub cells: Vec<GraphCell>,
}

#[derive(Debug, Default)]
pub struct Layout {
    lanes: Vec<Option<Lane>>,
    next_color: u8,
    // Scratch buffers reused across rows.
    cells: Vec<GraphCell>,
    merging: Vec<usize>,
}

impl Layout {
    pub fn new() -> Self {
        Self::default()
    }

    /// Lay out the next commit. `parents` must only contain parents that will
    /// appear later in the stream (callers drop already-emitted parents).
    pub fn push(&mut self, id: &ObjectId, parents: &[ObjectId]) -> GraphRow {
        // 1. Which lanes expect this commit?
        self.merging.clear();
        for (i, lane) in self.lanes.iter().enumerate() {
            if let Some(l) = lane
                && &l.expected == id
            {
                self.merging.push(i);
            }
        }
        let free_before: Vec<usize> = self
            .lanes
            .iter()
            .enumerate()
            .filter(|(_, l)| l.is_none())
            .map(|(i, _)| i)
            .collect();
        let mut free_iter = free_before.into_iter();

        let node_lane = if let Some(&first) = self.merging.first() {
            first
        } else {
            let color = self.pick_color();
            let slot = free_iter.next();
            self.alloc(
                slot,
                Lane {
                    expected: *id,
                    color,
                },
            )
        };
        let node_color = self.lanes[node_lane].expect("node lane set").color;
        // Lanes other than the first one that expected this commit end here.
        let merging: Vec<usize> = self.merging[1.min(self.merging.len())..].to_vec();

        // 2. Route parents.
        let mut targets: Vec<(usize, bool, u8)> = Vec::new(); // (column, is_new, color)
        let mut first_parent_done = false;
        for p in parents {
            if !first_parent_done {
                first_parent_done = true;
                self.lanes[node_lane] = Some(Lane {
                    expected: *p,
                    color: node_color,
                });
                continue;
            }
            // Existing lane already waiting for this parent?
            if let Some(k) = self
                .lanes
                .iter()
                .position(|l| matches!(l, Some(l) if &l.expected == p))
                && k != node_lane
            {
                let color = self.lanes[k].expect("checked").color;
                if !targets.iter().any(|t| t.0 == k) {
                    targets.push((k, false, color));
                }
                continue;
            }
            let color = self.pick_color();
            let slot = free_iter.next();
            let k = self.alloc(
                slot,
                Lane {
                    expected: *p,
                    color,
                },
            );
            targets.push((k, true, color));
        }
        if parents.is_empty() {
            self.lanes[node_lane] = None;
        }

        // 3. Paint the row.
        let ncols = self.lanes.len();
        self.cells.clear();
        self.cells.resize(ncols, GraphCell::EMPTY);
        for (i, lane) in self.lanes.iter().enumerate() {
            if let Some(l) = lane {
                self.cells[i] = GraphCell::new(CellKind::Vertical, l.color);
            }
        }
        // Lanes that end here were still present before this row (we have not cleared
        // them yet) and are painted as bends; clear them afterwards.
        for &j in &merging {
            let color = self.lanes[j].expect("merging lane").color;
            let kind = if j > node_lane {
                CellKind::EndFromRight
            } else {
                CellKind::EndFromLeft
            };
            self.cells[j] = GraphCell::new(kind, color);
        }
        for &(k, is_new, color) in &targets {
            let kind = match (is_new, k > node_lane) {
                (true, true) => CellKind::StartToRight,
                (true, false) => CellKind::StartToLeft,
                (false, true) => CellKind::JoinFromLeft,
                (false, false) => CellKind::JoinFromRight,
            };
            self.cells[k] = GraphCell::new(kind, color);
        }
        // Horizontal connectors, nearest first so the colour next to the node is the nearest branch.
        let mut spans: Vec<(usize, u8)> = merging
            .iter()
            .map(|&j| (j, self.lanes[j].expect("merging lane").color))
            .chain(targets.iter().map(|&(k, _, c)| (k, c)))
            .collect();
        spans.sort_by_key(|&(col, _)| col.abs_diff(node_lane));
        for (col, color) in spans {
            let (lo, hi) = if col > node_lane {
                (node_lane + 1, col)
            } else {
                (col + 1, node_lane)
            };
            for c in lo..hi {
                let cell = &mut self.cells[c];
                match cell.kind() {
                    CellKind::Empty => *cell = GraphCell::new(CellKind::Horizontal, color),
                    CellKind::Vertical => *cell = GraphCell::new(CellKind::Cross, cell.color()),
                    _ => {}
                }
            }
        }
        self.cells[node_lane] = GraphCell::new(
            if parents.len() > 1 {
                CellKind::MergeNode
            } else {
                CellKind::Node
            },
            node_color,
        );

        // 4. Advance state: free merged lanes, trim trailing empties.
        for &j in &merging {
            self.lanes[j] = None;
        }
        while matches!(self.lanes.last(), Some(None)) {
            self.lanes.pop();
        }

        GraphRow {
            node_lane: node_lane as u16,
            cells: self.cells.clone(),
        }
    }

    /// Number of currently active lanes (for diagnostics).
    #[cfg(test)]
    pub fn active_lanes(&self) -> usize {
        self.lanes.iter().filter(|l| l.is_some()).count()
    }

    fn alloc(&mut self, slot: Option<usize>, lane: Lane) -> usize {
        match slot {
            Some(i) => {
                self.lanes[i] = Some(lane);
                i
            }
            None => {
                self.lanes.push(Some(lane));
                self.lanes.len() - 1
            }
        }
    }

    fn pick_color(&mut self) -> u8 {
        let start = self.next_color;
        let mut chosen = start;
        for step in 0..COLOR_COUNT {
            let c = (start + step) % COLOR_COUNT;
            let in_use = self
                .lanes
                .iter()
                .any(|l| matches!(l, Some(l) if l.color == c));
            if !in_use {
                chosen = c;
                break;
            }
        }
        self.next_color = (chosen + 1) % COLOR_COUNT;
        chosen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> ObjectId {
        let mut bytes = [0u8; 20];
        bytes[0] = n;
        ObjectId::from_bytes_or_panic(&bytes)
    }

    fn kinds(row: &GraphRow) -> Vec<CellKind> {
        row.cells.iter().map(|c| c.kind()).collect()
    }

    #[test]
    fn linear_history_is_single_lane() {
        let mut l = Layout::new();
        let r1 = l.push(&id(3), &[id(2)]);
        let r2 = l.push(&id(2), &[id(1)]);
        let r3 = l.push(&id(1), &[]);
        for r in [&r1, &r2, &r3] {
            assert_eq!(r.node_lane, 0);
            assert_eq!(kinds(r), vec![CellKind::Node]);
        }
        assert_eq!(l.active_lanes(), 0);
    }

    #[test]
    fn merge_opens_and_closes_a_lane() {
        // 4 = merge(3, 2); 3 -> 1; 2 -> 1; 1 root
        let mut l = Layout::new();
        let m = l.push(&id(4), &[id(3), id(2)]);
        assert_eq!(kinds(&m), vec![CellKind::MergeNode, CellKind::StartToRight]);
        let a = l.push(&id(3), &[id(1)]);
        assert_eq!(kinds(&a), vec![CellKind::Node, CellKind::Vertical]);
        let b = l.push(&id(2), &[id(1)]);
        assert_eq!(b.node_lane, 1);
        assert_eq!(kinds(&b), vec![CellKind::Vertical, CellKind::Node]);
        // Both lanes expect 1; lane 1 merges into lane 0.
        let r = l.push(&id(1), &[]);
        assert_eq!(r.node_lane, 0);
        assert_eq!(kinds(&r), vec![CellKind::Node, CellKind::EndFromRight]);
        assert_eq!(l.active_lanes(), 0);
    }

    #[test]
    fn second_parent_joins_existing_lane() {
        // Tip 5 -> 4; 6 = merge(5?, ...). Build: lane0 expects 2 via 3; merge 4 has parents (1, 2).
        let mut l = Layout::new();
        let _ = l.push(&id(3), &[id(2)]); // lane0 expects 2
        let r = l.push(&id(4), &[id(1), id(2)]); // new lane1 for node 4; second parent 2 already in lane0
        assert_eq!(r.node_lane, 1);
        assert_eq!(
            kinds(&r),
            vec![CellKind::JoinFromRight, CellKind::MergeNode]
        );
    }

    #[test]
    fn octopus_merge_spans_multiple_lanes() {
        let mut l = Layout::new();
        let r = l.push(&id(9), &[id(1), id(2), id(3)]);
        assert_eq!(
            kinds(&r),
            vec![
                CellKind::MergeNode,
                CellKind::StartToRight,
                CellKind::StartToRight
            ]
        );
        assert_eq!(l.active_lanes(), 3);
    }

    #[test]
    fn horizontal_connector_crosses_pass_through_lanes() {
        // lane0: a -> x, lane1: b -> y. Then commit c (new lane2) with second parent in lane0.
        let mut l = Layout::new();
        let _ = l.push(&id(10), &[id(1)]);
        let _ = l.push(&id(11), &[id(2)]);
        let r = l.push(&id(12), &[id(3), id(1)]);
        assert_eq!(r.node_lane, 2);
        assert_eq!(
            kinds(&r),
            vec![
                CellKind::JoinFromRight,
                CellKind::Cross,
                CellKind::MergeNode
            ]
        );
    }

    #[test]
    fn two_roots_and_freed_slot_reuse() {
        let mut l = Layout::new();
        let _ = l.push(&id(5), &[id(4), id(3)]); // lanes: 0->4, 1->3
        let _ = l.push(&id(4), &[]); // root: lane0 freed
        let r = l.push(&id(3), &[]); // lane1 node; trailing empties trimmed afterwards
        assert_eq!(r.node_lane, 1);
        assert_eq!(kinds(&r), vec![CellKind::Empty, CellKind::Node]);
        assert_eq!(l.active_lanes(), 0);
        let r = l.push(&id(2), &[]);
        assert_eq!(r.node_lane, 0);
    }

    #[test]
    fn colors_prefer_unused() {
        let mut l = Layout::new();
        let r = l.push(&id(9), &[id(1), id(2), id(3)]);
        let colors: Vec<u8> = r.cells.iter().map(|c| c.color()).collect();
        assert_eq!(colors.len(), 3);
        assert!(colors[0] != colors[1] && colors[1] != colors[2] && colors[0] != colors[2]);
    }
}
