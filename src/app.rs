//! Application state and input handling (no drawing here; see `ui`).

use std::collections::HashMap;

use gix::ObjectId;

use crate::git::{CommitDetails, RefKind, RefSnapshot};
use crate::model::History;
use crate::term::{Button, KeyCode, KeyEvent, MouseEvent, MouseKind};
use crate::ui::Areas;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Details,
}

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
            quit: false,
            want_rewalk: false,
            needs_redraw: true,
            list_rows: 1,
        }
    }

    pub fn selected_id(&self) -> Option<ObjectId> {
        self.history.commits.get(self.selected).map(|c| c.id)
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
        let page = self.list_rows.saturating_sub(1).max(1) as isize;
        match (key.code, key.ctrl) {
            (KeyCode::Char('q'), _) | (KeyCode::Esc, _) | (KeyCode::Char('c'), true) => {
                self.quit = true
            }
            (KeyCode::Tab, _) => {
                self.focus = match self.focus {
                    Focus::List => Focus::Details,
                    Focus::Details => Focus::List,
                };
                self.needs_redraw = true;
            }
            (KeyCode::Char('r'), _) if !key.ctrl => self.want_rewalk = true,
            (KeyCode::Char('['), _) => self.resize_panel(-2),
            (KeyCode::Char(']'), _) => self.resize_panel(2),
            _ => match self.focus {
                Focus::List => self.handle_list_key(key, page),
                Focus::Details => self.handle_details_key(key, page),
            },
        }
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

    pub fn handle_mouse(&mut self, m: MouseEvent, areas: &Areas, screen_w: u16) {
        match m.kind {
            MouseKind::Down(Button::Left) | MouseKind::DoubleClick(Button::Left) => {
                if areas.splitter.contains(m.x, m.y) {
                    self.dragging_splitter = true;
                } else if areas.list.contains(m.x, m.y) {
                    self.focus = Focus::List;
                    let idx = self.scroll + (m.y - areas.list.y) as usize;
                    if idx < self.history.len() {
                        self.select(idx);
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
                if areas.details.contains(m.x, m.y) {
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
        app.handle_mouse(click(10, 4, MouseKind::Down(Button::Left)), &a, 100);
        assert_eq!(app.selected, 3);
        app.handle_mouse(click(10, 4, MouseKind::Wheel(-1)), &a, 100);
        assert_eq!(app.scroll, 3);
        app.handle_mouse(click(10, 4, MouseKind::Down(Button::Left)), &a, 100);
        assert_eq!(app.selected, 6);
        // Wheel over the details panel scrolls the panel, not the list.
        app.details_lines = 40;
        app.handle_mouse(click(70, 4, MouseKind::Wheel(-1)), &a, 100);
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
        app.handle_mouse(ev(60, MouseKind::Down(Button::Left)), &a, 100);
        assert!(app.dragging_splitter);
        app.handle_mouse(ev(50, MouseKind::Drag(Button::Left)), &a, 100);
        assert_eq!(app.right_width, 49);
        app.handle_mouse(ev(50, MouseKind::Up(Button::Left)), &a, 100);
        assert!(!app.dragging_splitter);
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
