use super::*;
use crate::graph::hover::{Row, trace};
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Anchor {
    Commit(gix::ObjectId),
    Pending(Option<gix::ObjectId>),
}

#[derive(PartialEq, Eq)]
struct Key {
    anchor: usize,
    range: Range<usize>,
    history_len: usize,
    cells: (usize, usize),
    search_generation: u64,
    // Partial results can replace/relayout the graph without changing generation.
    search: Option<(usize, usize, usize, usize, usize, bool)>,
    pending: Option<(usize, Option<usize>, Option<gix::ObjectId>)>,
}

#[derive(Default)]
pub(super) struct State {
    pub anchor: Option<usize>,
    identity: Option<Anchor>,
    key: Option<Key>,
    first: usize,
    flags: Vec<Vec<u8>>,
}

impl State {
    fn clear(&mut self) {
        self.anchor = None;
        self.identity = None;
        self.key = None;
        self.flags.clear();
    }

    fn identity_at(app: &crate::app::App, row: usize) -> Option<Anchor> {
        if !app.search.active() && app.pending.exists() && row == app.pending.row() {
            return Some(Anchor::Pending(app.pending.snapshot.head));
        }
        app.history_index(row)
            .and_then(|index| app.history.commits.get(index))
            .map(|record| Anchor::Commit(record.id))
    }

    fn hover(&mut self, app: &crate::app::App, row: usize, entered: bool) -> bool {
        if entered {
            let identity = Self::identity_at(app, row);
            if identity.is_none() {
                return false;
            }
            if self.anchor == Some(row) && self.identity == identity {
                return false;
            }
            self.anchor = Some(row);
            self.identity = identity;
            self.key = None;
            true
        } else if self.anchor == Some(row) {
            self.clear();
            true
        } else {
            false
        }
    }

    pub(super) fn diagnostics(&self) -> (usize, usize) {
        (
            self.flags.len(),
            self.flags
                .iter()
                .flatten()
                .filter(|&&flag| flag != 0)
                .count(),
        )
    }

    pub fn update(&mut self, app: &crate::app::App, range: Range<usize>) {
        let Some(anchor) = self.anchor else {
            self.clear();
            return;
        };
        let identity = Self::identity_at(app, anchor);
        // A stationary pointer must not silently transfer its highlight to a
        // different commit when partial search rows or Pending insertion move.
        if identity.is_none() || self.identity.is_some_and(|old| Some(old) != identity) {
            self.clear();
            return;
        }
        self.identity = identity;
        // UniformList can request a one-row measurement before its actual
        // visible range. A chunk that omits the anchor is not a mouse leave.
        if !range.contains(&anchor) {
            self.key = None;
            self.flags.clear();
            return;
        }
        let pending = (!app.search.active() && app.pending.exists()).then_some((
            app.pending.row(),
            app.pending.head_index,
            app.pending.snapshot.head,
        ));
        let key = Key {
            anchor,
            range: range.clone(),
            history_len: app.history.len(),
            cells: (app.history.cells.as_ptr() as usize, app.history.cells.len()),
            search_generation: app.search.generation,
            search: app
                .search
                .result
                .as_ref()
                .filter(|_| app.search.active())
                .map(|r| {
                    (
                        r.rows.as_ptr() as usize,
                        r.graph.as_ptr() as usize,
                        r.rows.len(),
                        r.graph.len(),
                        r.examined,
                        r.done,
                    )
                }),
            pending,
        };
        if self.key.as_ref() == Some(&key) {
            return;
        }
        let pending_geometry = pending.filter(|(row, _, _)| range.contains(row)).map(|_| {
            let head = app
                .pending
                .head_index
                .and_then(|index| app.history.commits.get(index).map(|record| (index, record)));
            let lane = head.map_or(0, |(_, record)| record.node_lane as usize);
            let cells = pending_graph_cells(
                head.map_or(&[], |(index, _)| app.history.row_cells(index)),
                lane,
            );
            (lane, cells, head.is_some())
        });
        let positions: Vec<_> = range
            .clone()
            .filter(|&row| {
                pending.is_some_and(|p| p.0 == row)
                    || app
                        .history_index(row)
                        .is_some_and(|index| index < app.history.len())
            })
            .collect();
        let rows: Vec<_> = positions
            .iter()
            .filter_map(|&row| {
                if pending.is_some_and(|p| p.0 == row) {
                    let (lane, cells, parent) = pending_geometry.as_ref()?;
                    return Some(Row {
                        cells,
                        node_lane: *lane,
                        has_parents: *parent,
                    });
                }
                let index = app.history_index(row)?;
                let record = &app.history.commits[index];
                let (lane, cells) = if app.search.active() {
                    let graph = app.search.result.as_ref()?.graph.get(row)?;
                    (graph.1 as usize, graph.0.as_slice())
                } else {
                    (record.node_lane as usize, app.history.row_cells(index))
                };
                Some(Row {
                    cells,
                    node_lane: lane,
                    has_parents: record.parents.len != 0,
                })
            })
            .collect();
        let hovered = positions
            .iter()
            .position(|&row| row == anchor)
            .unwrap_or(usize::MAX);
        self.flags = vec![Vec::new(); range.len()];
        for (row, flags) in positions.into_iter().zip(trace(&rows, hovered)) {
            self.flags[row - range.start] = flags;
        }
        self.first = range.start;
        self.key = Some(key);
    }

    pub fn row(&self, index: usize) -> Vec<u8> {
        index
            .checked_sub(self.first)
            .and_then(|i| self.flags.get(i))
            .cloned()
            .unwrap_or_default()
    }
}

impl Desktop {
    pub(super) fn hover_commit(&mut self, row: usize, entered: bool, cx: &mut Context<Self>) {
        if self.graph_hover.hover(&self.engine.app, row, entered) {
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::State;
    use crate::app::App;
    use crate::diff::{ChangeKind, FileChange};
    use crate::git::RefSnapshot;
    use crate::graph::hover::GLYPH;
    use crate::graph::layout::{CellKind, GraphCell, Layout};
    use crate::model::CommitRec;
    use crate::pending::Snapshot;
    use crate::search::Matches;

    fn id(n: u8) -> gix::ObjectId {
        gix::ObjectId::from_bytes_or_panic(&[n; 20])
    }

    fn fixture() -> App {
        let mut app = App::new();
        let mut layout = Layout::new();
        let definitions: &[(u8, &[u8])] = &[
            (6, &[4]),
            (5, &[3]),
            (4, &[2]),
            (3, &[1]),
            (2, &[1]),
            (1, &[]),
        ];
        for &(n, parents) in definitions {
            let parents: Vec<_> = parents.iter().copied().map(id).collect();
            let row = layout.push(&id(n), &parents);
            let parents = app.history.push_parents(&parents);
            let cells = app.history.push_cells(&row.cells);
            app.history.commits.push(CommitRec {
                id: id(n),
                parents,
                author_time: n.into(),
                author_offset: 0,
                commit_time: n.into(),
                author: Default::default(),
                subject: Default::default(),
                row: cells,
                node_lane: row.node_lane,
            });
        }
        app.history.complete = true;
        app.set_refs(RefSnapshot {
            head_id: Some(id(5)),
            head_branch: Some("main".into()),
            ..Default::default()
        });
        app.install_pending(Ok(Snapshot {
            head: Some(id(5)),
            branch: Some("main".into()),
            files: vec![FileChange {
                worktree: true,
                remote: None,
                path: "edited.txt".into(),
                old_path: None,
                kind: ChangeKind::Modified,
                old: None,
                new: None,
            }],
            ..Default::default()
        }));
        app
    }

    #[test]
    fn hover_track_crosses_pending_without_highlighting_parallel_pending_node() {
        let app = fixture();
        let pending = app.pending.row();
        assert_eq!(pending, 1);
        let pending_lane = app.history.commits[app.pending.head_index.unwrap()].node_lane as usize;
        let other_lane = app.history.commits[0].node_lane as usize;
        assert_ne!(pending_lane, other_lane);
        let mut state = State::default();
        assert!(state.hover(&app, 0, true));
        state.update(&app, 0..app.graph_len());
        assert_ne!(state.row(pending)[other_lane] & GLYPH, 0);
        assert_eq!(state.row(pending)[pending_lane] & GLYPH, 0);
        assert!(state.hover(&app, pending + 1, true));
        state.update(&app, 0..app.graph_len());
        assert_ne!(state.row(pending)[pending_lane] & GLYPH, 0);
        assert_eq!(state.row(pending)[other_lane] & GLYPH, 0);
        assert!(state.hover(&app, pending + 1, false));
        state.update(&app, 0..app.graph_len());
        assert_eq!(state.diagnostics(), (0, 0));
    }

    #[test]
    fn same_generation_partial_search_relayouts_and_rebinds_only_on_real_hover() {
        let mut app = fixture();
        app.search.text.value = "m: match".into();
        app.search.generation = 9;
        app.search.result = Some(Matches {
            generation: 9,
            rows: vec![0, 1],
            positions: vec![0, 1],
            graph: vec![
                (vec![GraphCell::new(CellKind::Node, 0)], 0, Vec::new()),
                (vec![GraphCell::new(CellKind::Vertical, 0)], 0, Vec::new()),
            ],
            examined: 2,
            total: 6,
            done: false,
            unavailable: 0,
            error: None,
        });
        let mut state = State::default();
        state.hover(&app, 0, true);
        state.update(&app, 0..2);
        assert_ne!(state.row(0)[0] & GLYPH, 0);
        let result = app.search.result.as_mut().unwrap();
        result.graph = vec![
            (
                vec![GraphCell::EMPTY, GraphCell::new(CellKind::Node, 1)],
                1,
                Vec::new(),
            ),
            (
                vec![GraphCell::EMPTY, GraphCell::new(CellKind::Vertical, 1)],
                1,
                Vec::new(),
            ),
        ];
        state.update(&app, 0..2);
        assert_eq!(state.row(0)[0], 0);
        assert_ne!(state.row(0)[1] & GLYPH, 0);
        // Progress can replace the commit under a stationary pointer without
        // changing the query generation or result count.
        app.search.result.as_mut().unwrap().rows.swap(0, 1);
        state.update(&app, 0..2);
        assert!(state.anchor.is_none());
        assert_eq!(state.diagnostics(), (0, 0));
        assert!(state.hover(&app, 0, true));
        state.update(&app, 0..2);
        assert_ne!(state.row(0)[1] & GLYPH, 0);
    }

    #[test]
    fn removing_pending_cannot_transfer_hover_to_a_different_commit_in_same_row() {
        let mut app = fixture();
        let row = app.pending.row() + 1;
        let mut state = State::default();
        state.hover(&app, row, true);
        state.update(&app, 0..app.graph_len());
        app.install_pending(Ok(Snapshot {
            head: Some(id(5)),
            branch: Some("main".into()),
            ..Default::default()
        }));
        state.update(&app, 0..app.graph_len());
        assert!(state.anchor.is_none());
        assert_eq!(state.diagnostics(), (0, 0));
        assert!(state.hover(&app, row - 1, true));
        state.update(&app, 0..app.graph_len());
        assert!(state.diagnostics().1 > 0);
    }

    #[test]
    fn virtual_list_measurement_chunk_does_not_cancel_hover_before_visible_rows() {
        let app = fixture();
        let row = app.pending.row() + 1;
        let lane = app.history.commits[app.pending.head_index.unwrap()].node_lane as usize;
        let mut state = State::default();
        state.hover(&app, row, true);
        state.update(&app, 0..1); // The list's initial row-height measurement.
        assert_eq!(state.anchor, Some(row));
        assert_eq!(state.diagnostics(), (0, 0));
        state.update(&app, 0..app.graph_len());
        assert_eq!(state.anchor, Some(row));
        assert_ne!(state.row(app.pending.row())[lane] & GLYPH, 0);
    }
}
