//! Application state and input handling (no drawing here; see `ui`).

use std::collections::HashMap;

use gix::ObjectId;

use crate::diff::changes::ChangeSet;
use crate::diff::filetree::TreeCursor;
use crate::diffview::{DiffReq, DiffView};
use crate::git::{CommitDetails, RefKind, RefSnapshot};
use crate::model::History;
use crate::term::{Button, KeyCode, KeyEvent, MouseEvent, MouseKind, Rect};
use crate::ui::Areas;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Details,
    /// The changed-files tree at the bottom of the details panel.
    Files,
}

/// Which screen is shown.
pub enum Mode {
    Graph,
    Diff(Box<DiffView>),
}

/// Upper bound on marked commits (a diff needs exactly two).
pub const MAX_MARKS: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadState {
    /// Nothing to show yet.
    Empty,
    /// Showing a cache that matches the current refs; no walk needed.
    Cached,
    /// Showing a stale cache while a fresh walk runs in the background.
    Refreshing,
    /// No cache; rows appear as the walk streams them.
    Loading,
    /// Fresh walk finished.
    Complete,
    Error(String),
}

#[derive(Debug, Clone)]
pub struct Label {
    pub name: String,
    pub kind: RefKind,
}

pub struct App {
    pub repo_name: String,
    pub history: History,
    /// A fresh history being built while `history` (stale cache) stays on screen.
    pub incoming: Option<History>,
    pub refs: Option<RefSnapshot>,
    pub labels: HashMap<ObjectId, Vec<Label>>,
    pub load: LoadState,

    pub selected: usize,
    pub scroll: usize,
    pub focus: Focus,
    pub details: Option<CommitDetails>,
    pub details_scroll: usize,
    pub details_lines: usize,

    /// Width of the details panel in cells.
    pub right_width: u16,
    pub dragging_splitter: bool,

    /// Commits marked for diffing, in marking order (at most `MAX_MARKS`).
    pub marks: Vec<ObjectId>,
    pub mode: Mode,
    /// Sidebar width for the diff view, remembered between views.
    pub sidebar_width: u16,
    next_view_id: u32,
    /// Files changed by the selected commit (vs. its first parent).
    pub details_changes: Option<ChangeSet>,
    pub details_cursor: TreeCursor,
    /// Where the details panel drew the file tree, for hit-testing.
    pub files_area: Rect,
    next_key: u32,

    pub quit: bool,
    pub want_rewalk: bool,
    pub needs_redraw: bool,
    /// Rows visible in the list, updated by the renderer.
    pub list_rows: usize,
}

impl App {
    pub fn new() -> Self {
        App {
            repo_name: String::new(),
            history: History::default(),
            incoming: None,
            refs: None,
            labels: HashMap::new(),
            load: LoadState::Empty,
            selected: 0,
            scroll: 0,
            focus: Focus::List,
            details: None,
            details_scroll: 0,
            details_lines: 0,
            right_width: 0,
            dragging_splitter: false,
            marks: Vec::new(),
            mode: Mode::Graph,
            sidebar_width: 32,
            next_view_id: 1,
            details_changes: None,
            details_cursor: TreeCursor::default(),
            files_area: Rect::default(),
            next_key: 1,
            quit: false,
            want_rewalk: false,
            needs_redraw: true,
            list_rows: 1,
        }
    }

    pub fn selected_id(&self) -> Option<ObjectId> {
        self.history.commits.get(self.selected).map(|c| c.id)
    }

    pub fn is_marked(&self, id: &ObjectId) -> bool {
        self.marks.contains(id)
    }

    /// Mark or unmark a commit; marking a third drops the oldest mark.
    pub fn toggle_mark(&mut self, id: ObjectId) {
        if let Some(i) = self.marks.iter().position(|m| *m == id) {
            self.marks.remove(i);
        } else {
            if self.marks.len() >= MAX_MARKS {
                self.marks.remove(0);
            }
            self.marks.push(id);
        }
        self.needs_redraw = true;
    }

    pub fn clear_marks(&mut self) {
        if !self.marks.is_empty() {
            self.marks.clear();
            self.needs_redraw = true;
        }
    }

    fn new_key(&mut self) -> u32 {
        let k = self.next_key;
        self.next_key += 1;
        k
    }

    /// First parent of a commit in the loaded history.
    pub fn parent_of(&self, id: &ObjectId) -> Option<ObjectId> {
        let i = self.history.index_of(id)?;
        self.history.parents(i).first().copied()
    }

    /// Keep `details_changes` pointed at the selected commit.
    pub fn sync_details_changes(&mut self) {
        let Some(id) = self.selected_id() else { return };
        if self.details_changes.as_ref().map(|c| c.new) == Some(id) {
            return;
        }
        let key = self.new_key();
        let parent = self.parent_of(&id);
        self.details_changes = Some(ChangeSet::new(key, parent, id));
        self.details_cursor = TreeCursor {
            rows: self.details_cursor.rows,
            ..TreeCursor::default()
        };
        if self.focus == Focus::Files {
            self.focus = Focus::List;
        }
        self.needs_redraw = true;
    }

    /// Tree/file diff work to hand to the worker.
    pub fn pending_requests(&mut self) -> Vec<DiffReq> {
        match &mut self.mode {
            Mode::Diff(view) => view.pending(),
            Mode::Graph => match self.details_changes.as_mut() {
                Some(cs) if !cs.requested => {
                    cs.requested = true;
                    vec![DiffReq::Tree {
                        key: cs.key,
                        old: cs.old,
                        new: cs.new,
                    }]
                }
                _ => Vec::new(),
            },
        }
    }

    /// The change set currently on screen (view or details panel).
    pub fn active_changes_mut(&mut self) -> Option<&mut ChangeSet> {
        match &mut self.mode {
            Mode::Diff(view) => Some(&mut view.changes),
            Mode::Graph => self.details_changes.as_mut(),
        }
    }

    /// Route a worker result to whichever change set has `key`.
    pub fn changes_by_key_mut(&mut self, key: u32) -> Option<&mut ChangeSet> {
        if let Mode::Diff(view) = &mut self.mode
            && view.changes.key == key
        {
            return Some(&mut view.changes);
        }
        self.details_changes.as_mut().filter(|c| c.key == key)
    }

    /// `d`: diff the two marked commits (older on the left), or with fewer
    /// marks the marked/selected commit against its parent.
    pub fn open_diff(&mut self) {
        if self.marks.len() == MAX_MARKS {
            let (a, b) = (self.marks[0], self.marks[1]);
            let time = |id: &ObjectId| self.history.index_of(id).map(|i| self.history.commits[i].commit_time);
            let (old, new) = match (time(&a), time(&b)) {
                (Some(ta), Some(tb)) if tb < ta => (b, a),
                _ => (a, b),
            };
            let key = self.new_key();
            self.show_view(ChangeSet::new(key, Some(old), new), None);
        } else if let Some(id) = self.marks.first().copied().or_else(|| self.selected_id()) {
            self.open_parent_diff(id);
        }
    }

    /// Diff `id` against its first parent, reusing the details panel's change
    /// set (and cursor) when it is the selected commit.
    pub fn open_parent_diff(&mut self, id: ObjectId) {
        if self.details_changes.as_ref().is_some_and(|c| c.new == id) {
            let cs = self.details_changes.take().unwrap();
            let cursor = self.details_cursor;
            self.show_view(cs, Some(cursor));
        } else {
            let key = self.new_key();
            let parent = self.parent_of(&id);
            self.show_view(ChangeSet::new(key, parent, id), None);
        }
    }

    fn show_view(&mut self, changes: ChangeSet, cursor: Option<TreeCursor>) {
        let id = self.next_view_id;
        self.next_view_id += 1;
        self.mode = Mode::Diff(Box::new(DiffView::new(id, changes, cursor, self.sidebar_width)));
        self.needs_redraw = true;
    }

    pub fn close_diff(&mut self) {
        if let Mode::Diff(view) = std::mem::replace(&mut self.mode, Mode::Graph) {
            self.sidebar_width = view.sidebar_width;
            // Hand a parent diff of the selected commit back to the details panel.
            let selected = self.selected_id();
            if Some(view.changes.new) == selected && view.changes.old == selected.and_then(|s| self.parent_of(&s)) {
                self.details_changes = Some(view.changes);
                self.details_cursor = view.cursor;
            }
        }
        self.needs_redraw = true;
    }

    pub fn diff_view_mut(&mut self) -> Option<&mut DiffView> {
        match &mut self.mode {
            Mode::Diff(v) => Some(v),
            Mode::Graph => None,
        }
    }

    fn files_tree_available(&self) -> bool {
        self.details_changes
            .as_ref()
            .is_some_and(|c| c.loaded && !c.files.is_empty())
    }

    fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::List => Focus::Details,
            Focus::Details if self.files_tree_available() => Focus::Files,
            Focus::Details | Focus::Files => Focus::List,
        };
        self.needs_redraw = true;
    }

    pub fn head_id(&self) -> Option<ObjectId> {
        self.refs.as_ref().and_then(|r| r.head_id)
    }

    pub fn set_refs(&mut self, refs: RefSnapshot) {
        let mut labels: HashMap<ObjectId, Vec<Label>> = HashMap::new();
        for (id, entries) in refs.labels_by_commit() {
            labels.insert(
                id,
                entries
                    .into_iter()
                    .map(|r| Label {
                        name: r.name.clone(),
                        kind: r.kind,
                    })
                    .collect(),
            );
        }
        self.labels = labels;
        self.refs = Some(refs);
        self.needs_redraw = true;
    }

    /// Install a history read from the cache.
    pub fn set_cached(&mut self, history: History, fresh: bool) {
        self.history = history;
        self.load = if fresh {
            LoadState::Cached
        } else {
            LoadState::Refreshing
        };
        self.clamp();
        self.needs_redraw = true;
    }

    /// A batch arrived from the walker.
    pub fn push_batch(&mut self, batch: History) {
        let complete = batch.complete;
        match self.load {
            LoadState::Refreshing | LoadState::Cached => {
                // Keep the stale view stable; swap when the fresh walk is complete.
                self.incoming.get_or_insert_with(History::default).append(batch);
                if complete {
                    let fresh = self.incoming.take().unwrap_or_default();
                    let keep = self.selected_id();
                    self.history = fresh;
                    if let Some(id) = keep
                        && let Some(i) = self.history.index_of(&id)
                    {
                        self.selected = i;
                    }
                    self.load = LoadState::Complete;
                    self.clamp();
                }
            }
            _ => {
                self.history.append(batch);
                self.load = if complete {
                    LoadState::Complete
                } else {
                    LoadState::Loading
                };
                self.clamp();
            }
        }
        self.needs_redraw = true;
    }

    pub fn start_walk(&mut self) {
        self.incoming = None;
        self.load = if self.history.is_empty() {
            LoadState::Loading
        } else {
            LoadState::Refreshing
        };
        self.needs_redraw = true;
    }

    pub fn set_error(&mut self, msg: String) {
        self.load = LoadState::Error(msg);
        self.needs_redraw = true;
    }

    pub fn is_walking(&self) -> bool {
        matches!(self.load, LoadState::Loading | LoadState::Refreshing)
    }

    fn clamp(&mut self) {
        let n = self.history.len();
        if n == 0 {
            self.selected = 0;
            self.scroll = 0;
            return;
        }
        if self.selected >= n {
            self.selected = n - 1;
        }
        let max_scroll = n.saturating_sub(self.list_rows.max(1));
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
    }

    pub fn ensure_visible(&mut self) {
        let rows = self.list_rows.max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }
    }

    pub fn select(&mut self, idx: usize) {
        let n = self.history.len();
        if n == 0 {
            return;
        }
        let idx = idx.min(n - 1);
        if idx != self.selected {
            self.selected = idx;
            self.details_scroll = 0;
        }
        self.ensure_visible();
        self.needs_redraw = true;
    }

    fn move_selection(&mut self, delta: isize) {
        let n = self.history.len() as isize;
        if n == 0 {
            return;
        }
        let target = (self.selected as isize + delta).clamp(0, n - 1);
        self.select(target as usize);
    }

    fn scroll_view(&mut self, delta: isize) {
        let n = self.history.len();
        let max_scroll = n.saturating_sub(self.list_rows.max(1));
        let target = (self.scroll as isize + delta).clamp(0, max_scroll as isize);
        self.scroll = target as usize;
        self.needs_redraw = true;
    }

    fn scroll_details(&mut self, delta: isize) {
        let max = self.details_lines.saturating_sub(1);
        let target = (self.details_scroll as isize + delta).clamp(0, max as isize);
        self.details_scroll = target as usize;
        self.needs_redraw = true;
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if let Mode::Diff(view) = &mut self.mode {
            view.handle_key(key);
            let (close, redraw) = (view.close, view.needs_redraw);
            if close {
                self.close_diff();
            }
            self.needs_redraw |= redraw;
            return;
        }
        let page = self.list_rows.saturating_sub(1).max(1) as isize;
        match (key.code, key.ctrl) {
            (KeyCode::Char('q'), _) | (KeyCode::Char('c'), true) => self.quit = true,
            (KeyCode::Esc, _) => {
                if self.marks.is_empty() {
                    self.quit = true;
                } else {
                    self.clear_marks();
                }
            }
            (KeyCode::Char(' '), _) => {
                if let Some(id) = self.selected_id() {
                    self.toggle_mark(id);
                }
            }
            (KeyCode::Delete, _) => self.clear_marks(),
            (KeyCode::Char('d'), false) => self.open_diff(),
            (KeyCode::Tab, _) => self.cycle_focus(),
            (KeyCode::Char('r'), _) if !key.ctrl => self.want_rewalk = true,
            (KeyCode::Char('['), _) => self.resize_panel(-2),
            (KeyCode::Char(']'), _) => self.resize_panel(2),
            _ => match self.focus {
                Focus::List => self.handle_list_key(key, page),
                Focus::Details => self.handle_details_key(key, page),
                Focus::Files => self.handle_files_key(key),
            },
        }
    }

    fn handle_files_key(&mut self, key: KeyEvent) {
        let Some(cs) = self.details_changes.as_mut() else {
            self.focus = Focus::List;
            return;
        };
        let tree = &mut cs.tree;
        let cur = &mut self.details_cursor;
        let page = cur.rows.saturating_sub(1).max(1) as isize;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                cur.move_by(tree, -1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                cur.move_by(tree, 1);
            }
            KeyCode::PageUp => {
                cur.move_by(tree, -page);
            }
            KeyCode::PageDown => {
                cur.move_by(tree, page);
            }
            KeyCode::Home | KeyCode::Char('g') => {
                cur.select(tree, 0);
            }
            KeyCode::End | KeyCode::Char('G') => {
                cur.select(tree, usize::MAX);
            }
            KeyCode::Left | KeyCode::Char('h') => cur.left(tree),
            KeyCode::Right | KeyCode::Char('l') => {
                cur.right(tree);
            }
            KeyCode::Char('e') => cur.set_all_expanded(tree, true),
            KeyCode::Char('c') => cur.set_all_expanded(tree, false),
            KeyCode::Enter => {
                // Folder: fold; file: open the full diff at this file.
                if !cur.toggle(tree)
                    && let Some(id) = self.selected_id()
                {
                    self.open_parent_diff(id);
                }
            }
            _ => {}
        }
        self.needs_redraw = true;
    }

    fn handle_list_key(&mut self, key: KeyEvent, page: isize) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-page),
            KeyCode::PageDown => self.move_selection(page),
            KeyCode::Home | KeyCode::Char('g') => self.select(0),
            KeyCode::End | KeyCode::Char('G') => self.select(usize::MAX),
            KeyCode::Char('H') => {
                if let Some(h) = self.head_id()
                    && let Some(i) = self.history.index_of(&h)
                {
                    self.select(i);
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.focus = Focus::Details;
                self.needs_redraw = true;
            }
            _ => {}
        }
    }

    fn handle_details_key(&mut self, key: KeyEvent, page: isize) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.scroll_details(-1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_details(1),
            KeyCode::PageUp => self.scroll_details(-page),
            KeyCode::PageDown => self.scroll_details(page),
            KeyCode::Home | KeyCode::Char('g') => self.scroll_details(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.scroll_details(isize::MAX / 2),
            KeyCode::Left | KeyCode::Char('h') => {
                self.focus = Focus::List;
                self.needs_redraw = true;
            }
            _ => {}
        }
    }

    pub fn resize_panel(&mut self, delta: i32) {
        let w = (self.right_width as i32 + delta).max(0) as u16;
        self.right_width = w;
        self.needs_redraw = true;
    }

    pub fn handle_mouse(&mut self, m: MouseEvent, areas: &Areas, screen_w: u16, screen_h: u16) {
        if let Mode::Diff(view) = &mut self.mode {
            let da = crate::ui::diff::compute_areas(screen_w, screen_h, view.sidebar_width);
            view.handle_mouse(m, &da);
            self.needs_redraw |= view.needs_redraw;
            return;
        }
        match m.kind {
            MouseKind::Down(Button::Left) | MouseKind::DoubleClick(Button::Left) => {
                if areas.splitter.contains(m.x, m.y) {
                    self.dragging_splitter = true;
                } else if areas.list.contains(m.x, m.y) {
                    self.focus = Focus::List;
                    let idx = self.scroll + (m.y - areas.list.y) as usize;
                    if idx < self.history.len() {
                        self.select(idx);
                        let id = self.history.commits[idx].id;
                        if m.ctrl {
                            // Ctrl+click marks the commit for diffing.
                            self.toggle_mark(id);
                        } else if matches!(m.kind, MouseKind::DoubleClick(_)) {
                            self.sync_details_changes();
                            self.open_parent_diff(id);
                        }
                    }
                    self.needs_redraw = true;
                } else if self.files_area.contains(m.x, m.y) {
                    self.focus = Focus::Files;
                    let row = self.details_cursor.scroll + (m.y - self.files_area.y) as usize;
                    if let Some(cs) = self.details_changes.as_mut()
                        && row < cs.tree.visible.len()
                    {
                        let cur = &mut self.details_cursor;
                        let file = cur.select(&cs.tree, row);
                        let folded = cur.toggle(&mut cs.tree);
                        if !folded
                            && file.is_some()
                            && matches!(m.kind, MouseKind::DoubleClick(_))
                            && let Some(id) = self.selected_id()
                        {
                            self.open_parent_diff(id);
                        }
                    }
                    self.needs_redraw = true;
                } else if areas.details.contains(m.x, m.y) {
                    self.focus = Focus::Details;
                    self.needs_redraw = true;
                }
            }
            MouseKind::Drag(Button::Left) => {
                if self.dragging_splitter {
                    let new_w = screen_w.saturating_sub(m.x).saturating_sub(1);
                    if new_w != self.right_width {
                        self.right_width = new_w;
                        self.needs_redraw = true;
                    }
                }
            }
            MouseKind::Up(Button::Left) => self.dragging_splitter = false,
            MouseKind::Wheel(delta) => {
                let lines = -(delta as isize) * 3;
                if self.files_area.contains(m.x, m.y) {
                    if let Some(cs) = self.details_changes.as_ref() {
                        self.details_cursor.scroll_view(&cs.tree, lines);
                        self.needs_redraw = true;
                    }
                } else if areas.details.contains(m.x, m.y) {
                    self.scroll_details(lines);
                } else {
                    self.scroll_view(lines);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CommitRec;
    use crate::term::Rect;

    fn history(n: usize) -> History {
        let mut h = History::default();
        for i in 0..n {
            let mut bytes = [0u8; 20];
            bytes[0] = (i & 0xff) as u8;
            bytes[1] = (i >> 8) as u8;
            let id = ObjectId::from_bytes_or_panic(&bytes);
            let author = h.push_text(b"a");
            let subject = h.push_text(format!("c{i}").as_bytes());
            let row = h.push_cells(&[]);
            h.commits.push(CommitRec {
                id,
                parents: Span::default(),
                author_time: 0,
                author_offset: 0,
                commit_time: 0,
                author,
                subject,
                row,
                node_lane: 0,
            });
        }
        h.complete = true;
        h
    }
    use crate::model::Span;

    fn app_with(n: usize) -> App {
        let mut app = App::new();
        app.set_cached(history(n), true);
        app.list_rows = 10;
        app
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            ctrl: false,
            shift: false,
            alt: false,
        }
    }

    fn areas() -> Areas {
        Areas {
            header: Rect::new(0, 0, 100, 1),
            list: Rect::new(0, 1, 60, 10),
            splitter: Rect::new(60, 1, 1, 10),
            details: Rect::new(61, 1, 39, 10),
            status: Rect::new(0, 11, 100, 1),
        }
    }

    #[test]
    fn keyboard_navigation_keeps_selection_visible() {
        let mut app = app_with(50);
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.selected, 1);
        app.handle_key(key(KeyCode::PageDown));
        assert_eq!(app.selected, 10);
        assert_eq!(app.scroll, 1);
        app.handle_key(key(KeyCode::End));
        assert_eq!(app.selected, 49);
        assert_eq!(app.scroll, 40);
        app.handle_key(key(KeyCode::Home));
        assert_eq!((app.selected, app.scroll), (0, 0));
        app.handle_key(key(KeyCode::Char('q')));
        assert!(app.quit);
    }

    #[test]
    fn mouse_click_selects_and_wheel_scrolls() {
        let mut app = app_with(50);
        let a = areas();
        let click = |x, y, kind| MouseEvent {
            kind,
            x,
            y,
            ctrl: false,
            shift: false,
        };
        app.handle_mouse(click(10, 4, MouseKind::Down(Button::Left)), &a, 100, 12);
        assert_eq!(app.selected, 3);
        app.handle_mouse(click(10, 4, MouseKind::Wheel(-1)), &a, 100, 12);
        assert_eq!(app.scroll, 3);
        app.handle_mouse(click(10, 4, MouseKind::Down(Button::Left)), &a, 100, 12);
        assert_eq!(app.selected, 6);
        // Wheel over the details panel scrolls the panel, not the list.
        app.details_lines = 40;
        app.handle_mouse(click(70, 4, MouseKind::Wheel(-1)), &a, 100, 12);
        assert_eq!(app.scroll, 3);
        assert_eq!(app.details_scroll, 3);
    }

    #[test]
    fn splitter_drag_resizes_panel() {
        let mut app = app_with(5);
        let a = areas();
        let ev = |x, kind| MouseEvent {
            kind,
            x,
            y: 5,
            ctrl: false,
            shift: false,
        };
        app.handle_mouse(ev(60, MouseKind::Down(Button::Left)), &a, 100, 12);
        assert!(app.dragging_splitter);
        app.handle_mouse(ev(50, MouseKind::Drag(Button::Left)), &a, 100, 12);
        assert_eq!(app.right_width, 49);
        app.handle_mouse(ev(50, MouseKind::Up(Button::Left)), &a, 100, 12);
        assert!(!app.dragging_splitter);
    }

    #[test]
    fn marks_and_diff_mode() {
        let mut app = app_with(10);
        // No marks: `d` diffs the selected commit against its parent.
        app.handle_key(key(KeyCode::Char('d')));
        match &app.mode {
            Mode::Diff(v) => {
                assert_eq!(v.changes.new, app.history.commits[0].id);
                assert_eq!(v.changes.old, None, "test history has no parents");
            }
            Mode::Graph => panic!("expected a parent diff"),
        }
        app.handle_key(key(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Graph));

        app.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(app.marks.len(), 1);
        // One mark: `d` diffs the marked commit, even if the selection moved.
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Char('d')));
        match &app.mode {
            Mode::Diff(v) => assert_eq!(v.changes.new, app.history.commits[0].id),
            Mode::Graph => panic!("expected a parent diff"),
        }
        app.handle_key(key(KeyCode::Esc));
        app.handle_key(key(KeyCode::Down));
        // Ctrl+click marks the clicked row.
        let a = areas();
        app.handle_mouse(
            MouseEvent {
                kind: MouseKind::Down(Button::Left),
                x: 5,
                y: a.list.y + 4,
                ctrl: true,
                shift: false,
            },
            &a,
            100,
            12,
        );
        assert_eq!(app.selected, 4);
        assert_eq!(app.marks.len(), 2);
        assert!(app.is_marked(&app.history.commits[0].id));
        assert!(app.is_marked(&app.history.commits[4].id));
        // A third mark replaces the oldest.
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(app.marks.len(), 2);
        assert!(!app.is_marked(&app.history.commits[0].id));
        // Space on a marked commit unmarks it.
        app.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(app.marks.len(), 1);
        app.handle_key(key(KeyCode::Char(' ')));

        app.handle_key(key(KeyCode::Char('d')));
        assert!(matches!(app.mode, Mode::Diff(_)));
        // Keys go to the diff view now; Esc returns to the graph without quitting.
        app.handle_key(key(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Graph));
        assert!(!app.quit);
        // Esc with marks clears them; a second Esc quits.
        app.handle_key(key(KeyCode::Esc));
        assert!(app.marks.is_empty() && !app.quit);
        app.handle_key(key(KeyCode::Esc));
        assert!(app.quit);
    }

    #[test]
    fn double_click_opens_parent_diff_of_that_row() {
        let mut app = app_with(10);
        let a = areas();
        app.handle_mouse(
            MouseEvent {
                kind: MouseKind::DoubleClick(Button::Left),
                x: 5,
                y: a.list.y + 6,
                ctrl: false,
                shift: false,
            },
            &a,
            100,
            12,
        );
        assert_eq!(app.selected, 6);
        match &app.mode {
            Mode::Diff(v) => assert_eq!(v.changes.new, app.history.commits[6].id),
            Mode::Graph => panic!("expected a parent diff"),
        }
    }

    #[test]
    fn details_tree_hands_over_to_the_diff_view_and_back() {
        use crate::diff::{ChangeKind, FileChange, TreeDiff};
        let mut app = app_with(10);
        app.sync_details_changes();
        let reqs = app.pending_requests();
        assert!(matches!(reqs.as_slice(), [DiffReq::Tree { old: None, .. }]));
        assert!(app.pending_requests().is_empty());
        let cs_key = app.details_changes.as_ref().unwrap().key;
        let change = |p: &str| FileChange {
            path: p.to_string(),
            old_path: None,
            kind: ChangeKind::Added,
            old: None,
            new: None,
        };
        app.changes_by_key_mut(cs_key).unwrap().set_tree(Ok(TreeDiff {
            files: vec![change("src/a.rs"), change("b.rs")],
        }));
        app.details_cursor.rows = 5;
        // Tab: list → details → files.
        app.handle_key(key(KeyCode::Tab));
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Files);
        app.handle_key(key(KeyCode::Down)); // a.rs
        app.handle_key(key(KeyCode::Down)); // b.rs
        app.handle_key(key(KeyCode::Enter));
        let Mode::Diff(v) = &app.mode else { panic!("expected diff view") };
        assert_eq!(v.changes.key, cs_key, "change set reused, no recomputation");
        assert_eq!(v.current, Some(1), "opened at b.rs");
        assert!(app.details_changes.is_none());
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.details_changes.as_ref().map(|c| c.key), Some(cs_key), "handed back");
        assert_eq!(app.details_cursor.sel, 2);
        // Selecting another commit starts a fresh set.
        app.handle_key(key(KeyCode::Tab)); // back to the list
        assert_eq!(app.focus, Focus::List);
        app.handle_key(key(KeyCode::Down));
        app.sync_details_changes();
        assert_ne!(app.details_changes.as_ref().unwrap().key, cs_key);
    }

    #[test]
    fn stale_cache_is_swapped_only_when_walk_completes() {
        let mut app = app_with(5);
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Down));
        let keep = app.selected_id().unwrap();
        app.start_walk();
        assert_eq!(app.load, LoadState::Refreshing);
        // Fresh walk: 10 commits, with a new one on top so the old ids shift by one.
        let mut fresh = history(11);
        fresh.commits.rotate_right(1);
        fresh.complete = false;
        let mut first = History::default();
        first.append(fresh);
        first.complete = false;
        app.push_batch(first);
        assert_eq!(app.history.len(), 5, "stale view stays until completion");
        let tail = History {
            complete: true,
            ..Default::default()
        };
        app.push_batch(tail);
        assert_eq!(app.history.len(), 11);
        assert_eq!(app.load, LoadState::Complete);
        assert_eq!(app.selected_id(), Some(keep), "selection follows the commit id");
    }
}
