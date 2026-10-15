//! State and input handling for the full-screen diff view. Drawing lives in
//! `ui::diff`; diff computation runs on worker threads and results arrive
//! through `set_tree` / `set_file_diff` (stats go straight to the `ChangeSet`).

use gix::ObjectId;

use crate::diff::changes::ChangeSet;
use crate::diff::filetree::TreeCursor;
use crate::diff::{FileChange, FileDiff, TreeDiff};
use crate::term::{Button, KeyCode, KeyEvent, MouseEvent, MouseKind};
use crate::ui::diff::DiffAreas;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffFocus {
    Files,
    Diff,
}

/// Work for the diff thread.
#[derive(Debug)]
pub enum DiffReq {
    Tree {
        key: u32,
        old: Option<ObjectId>,
        new: ObjectId,
    },
    File {
        view: u32,
        seq: u32,
        file: usize,
        change: FileChange,
    },
}

/// Horizontal scroll step, in cells.
const HSTEP: usize = 8;

pub struct DiffView {
    pub id: u32,
    pub changes: ChangeSet,
    pub cursor: TreeCursor,

    pub focus: DiffFocus,
    pub sidebar_width: u16,
    pub dragging_splitter: bool,

    /// Change shown in the main panel.
    pub current: Option<usize>,
    pub diff: Option<FileDiff>,
    pub diff_for: Option<usize>,
    req_seq: u32,
    requested_for: Option<usize>,
    pub scroll: usize,
    pub hscroll: usize,
    /// Main panel rows visible, set by the renderer.
    pub rows: usize,
    /// Width of one text half in the main panel, set by the renderer.
    pub half_cols: usize,

    pub close: bool,
    pub needs_redraw: bool,
}

impl DiffView {
    /// Open a view over `changes`, which may already be loaded (handed over
    /// from the details panel together with its cursor).
    pub fn new(id: u32, changes: ChangeSet, cursor: Option<TreeCursor>, sidebar_width: u16) -> Self {
        let mut v = DiffView {
            id,
            changes,
            cursor: cursor.unwrap_or_default(),
            focus: DiffFocus::Files,
            sidebar_width,
            dragging_splitter: false,
            current: None,
            diff: None,
            diff_for: None,
            req_seq: 0,
            requested_for: None,
            scroll: 0,
            hscroll: 0,
            rows: 1,
            half_cols: 40,
            close: false,
            needs_redraw: true,
        };
        if v.changes.loaded {
            v.cursor.clamp(&v.changes.tree);
            v.current = v
                .cursor
                .file(&v.changes.tree)
                .or_else(|| v.cursor.first_file(&v.changes.tree));
        }
        v
    }

    /// Requests that should be sent to the diff worker now.
    pub fn pending(&mut self) -> Vec<DiffReq> {
        let mut out = Vec::new();
        if !self.changes.requested {
            self.changes.requested = true;
            out.push(DiffReq::Tree {
                key: self.changes.key,
                old: self.changes.old,
                new: self.changes.new,
            });
        }
        if let Some(cur) = self.current
            && self.diff_for != Some(cur)
            && self.requested_for != Some(cur)
            && let Some(change) = self.changes.files.get(cur)
        {
            self.req_seq += 1;
            self.requested_for = Some(cur);
            out.push(DiffReq::File {
                view: self.id,
                seq: self.req_seq,
                file: cur,
                change: change.clone(),
            });
        }
        out
    }

    pub fn set_tree(&mut self, result: Result<TreeDiff, String>) {
        self.changes.set_tree(result);
        self.cursor = TreeCursor {
            rows: self.cursor.rows,
            ..TreeCursor::default()
        };
        self.current = self.cursor.first_file(&self.changes.tree);
        self.needs_redraw = true;
    }

    pub fn set_file_diff(&mut self, seq: u32, file: usize, result: Result<FileDiff, String>) {
        if seq != self.req_seq {
            return; // superseded
        }
        self.requested_for = None;
        if self.current != Some(file) {
            return;
        }
        self.diff_for = Some(file);
        self.diff = Some(match result {
            Ok(d) => d,
            Err(e) => FileDiff::message(format!("error: {e}")),
        });
        self.scroll = 0;
        self.needs_redraw = true;
    }

    pub fn current_change(&self) -> Option<&FileChange> {
        self.current.and_then(|i| self.changes.files.get(i))
    }

    fn show(&mut self, file: Option<usize>) {
        if let Some(file) = file
            && self.current != Some(file)
        {
            self.current = Some(file);
            self.diff = None;
            self.diff_for = None;
            self.scroll = 0;
            self.hscroll = 0;
        }
        self.needs_redraw = true;
    }

    fn max_scroll(&self) -> usize {
        self.diff
            .as_ref()
            .map(|d| d.rows.len())
            .unwrap_or(0)
            .saturating_sub(self.rows.max(1))
    }

    fn scroll_by(&mut self, delta: isize) {
        let target = (self.scroll as isize + delta).clamp(0, self.max_scroll() as isize);
        self.scroll = target as usize;
        self.needs_redraw = true;
    }

    fn hscroll_by(&mut self, delta: isize) {
        let max = self
            .diff
            .as_ref()
            .map(|d| d.max_width)
            .unwrap_or(0)
            .saturating_sub(self.half_cols.max(1) / 2) as isize;
        self.hscroll = (self.hscroll as isize + delta).clamp(0, max.max(0)) as usize;
        self.needs_redraw = true;
    }

    pub fn resize_sidebar(&mut self, delta: i32) {
        self.sidebar_width = (self.sidebar_width as i32 + delta).max(0) as u16;
        self.needs_redraw = true;
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        match (key.code, key.ctrl) {
            (KeyCode::Esc, _) | (KeyCode::Char('q'), _) => self.close = true,
            (KeyCode::Char('c'), true) => self.close = true,
            (KeyCode::Tab, _) => {
                self.focus = match self.focus {
                    DiffFocus::Files => DiffFocus::Diff,
                    DiffFocus::Diff => DiffFocus::Files,
                };
                self.needs_redraw = true;
            }
            (KeyCode::Char('n'), _) => {
                let f = self.cursor.step_file(&self.changes.tree, true);
                self.show(f);
            }
            (KeyCode::Char('p'), _) => {
                let f = self.cursor.step_file(&self.changes.tree, false);
                self.show(f);
            }
            (KeyCode::Char('['), _) => self.resize_sidebar(-2),
            (KeyCode::Char(']'), _) => self.resize_sidebar(2),
            (KeyCode::Char('e'), _) => {
                self.cursor.set_all_expanded(&mut self.changes.tree, true);
                self.needs_redraw = true;
            }
            (KeyCode::Char('c'), false) => {
                self.cursor.set_all_expanded(&mut self.changes.tree, false);
                self.needs_redraw = true;
            }
            _ => match self.focus {
                DiffFocus::Files => self.handle_files_key(key),
                DiffFocus::Diff => self.handle_diff_key(key),
            },
        }
    }

    fn handle_files_key(&mut self, key: KeyEvent) {
        let page = self.cursor.rows.saturating_sub(1).max(1) as isize;
        let tree = &mut self.changes.tree;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                let f = self.cursor.move_by(tree, -1);
                self.show(f);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let f = self.cursor.move_by(tree, 1);
                self.show(f);
            }
            KeyCode::PageUp => {
                let f = self.cursor.move_by(tree, -page);
                self.show(f);
            }
            KeyCode::PageDown => {
                let f = self.cursor.move_by(tree, page);
                self.show(f);
            }
            KeyCode::Home | KeyCode::Char('g') => {
                let f = self.cursor.select(tree, 0);
                self.show(f);
            }
            KeyCode::End | KeyCode::Char('G') => {
                let f = self.cursor.select(tree, usize::MAX);
                self.show(f);
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                if !self.cursor.toggle(tree) {
                    self.focus = DiffFocus::Diff;
                }
                self.needs_redraw = true;
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.cursor.left(tree);
                self.needs_redraw = true;
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if self.cursor.right(tree) {
                    self.focus = DiffFocus::Diff;
                } else {
                    let f = self.cursor.file(tree);
                    self.show(f);
                }
                self.needs_redraw = true;
            }
            _ => {}
        }
    }

    fn handle_diff_key(&mut self, key: KeyEvent) {
        let page = self.rows.saturating_sub(1).max(1) as isize;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.scroll_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_by(1),
            KeyCode::PageUp => self.scroll_by(-page),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll_by(page),
            KeyCode::Home | KeyCode::Char('g') => self.scroll_by(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.scroll_by(isize::MAX / 2),
            KeyCode::Left | KeyCode::Char('h') => self.hscroll_by(-(HSTEP as isize)),
            KeyCode::Right | KeyCode::Char('l') => self.hscroll_by(HSTEP as isize),
            _ => {}
        }
    }

    pub fn handle_mouse(&mut self, m: MouseEvent, areas: &DiffAreas) {
        match m.kind {
            MouseKind::Down(Button::Left) | MouseKind::DoubleClick(Button::Left) => {
                if areas.splitter.contains(m.x, m.y) {
                    self.dragging_splitter = true;
                } else if areas.sidebar.contains(m.x, m.y) {
                    self.focus = DiffFocus::Files;
                    let row = self.cursor.scroll + (m.y - areas.sidebar.y) as usize;
                    let tree = &mut self.changes.tree;
                    if row < tree.visible.len() {
                        let f = self.cursor.select(tree, row);
                        self.cursor.toggle(tree);
                        self.show(f);
                    }
                    self.needs_redraw = true;
                } else if areas.main.contains(m.x, m.y) {
                    self.focus = DiffFocus::Diff;
                    self.needs_redraw = true;
                }
            }
            MouseKind::Drag(Button::Left) => {
                if self.dragging_splitter && m.x != self.sidebar_width {
                    self.sidebar_width = m.x;
                    self.needs_redraw = true;
                }
            }
            MouseKind::Up(Button::Left) => self.dragging_splitter = false,
            MouseKind::Wheel(delta) if m.shift => self.hscroll_by(-(delta as isize) * HSTEP as isize),
            MouseKind::HWheel(delta) => self.hscroll_by(delta as isize * HSTEP as isize),
            MouseKind::Wheel(delta) => {
                let lines = -(delta as isize) * 3;
                if areas.sidebar.contains(m.x, m.y) {
                    self.cursor.scroll_view(&self.changes.tree, lines);
                    self.needs_redraw = true;
                } else {
                    self.scroll_by(lines);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::ChangeKind;
    use crate::term::Rect;

    fn id(b: u8) -> ObjectId {
        ObjectId::from_bytes_or_panic(&[b; 20])
    }

    fn change(path: &str) -> FileChange {
        FileChange {
            path: path.to_string(),
            old_path: None,
            kind: ChangeKind::Modified,
            old: None,
            new: None,
        }
    }

    fn view() -> DiffView {
        let mut v = DiffView::new(1, ChangeSet::new(7, Some(id(1)), id(2)), None, 30);
        v.cursor.rows = 10;
        v.rows = 10;
        let reqs = v.pending();
        assert!(matches!(reqs.as_slice(), [DiffReq::Tree { key: 7, .. }]));
        v.set_tree(Ok(TreeDiff {
            files: vec![change("src/a.rs"), change("src/b.rs"), change("top.rs")],
        }));
        v
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            ctrl: false,
            shift: false,
            alt: false,
        }
    }

    #[test]
    fn first_file_is_shown_and_requested_once() {
        let mut v = view();
        // Visible: src, a.rs, b.rs, top.rs → first file is a.rs (index 0).
        assert_eq!(v.cursor.sel, 1);
        assert_eq!(v.current, Some(0));
        let reqs = v.pending();
        assert!(matches!(reqs.as_slice(), [DiffReq::File { file: 0, seq: 1, .. }]));
        assert!(v.pending().is_empty(), "no duplicate request while in flight");
        v.set_file_diff(1, 0, Ok(FileDiff::message("x".into())));
        assert_eq!(v.diff_for, Some(0));
        assert!(v.pending().is_empty());
    }

    #[test]
    fn stale_results_are_dropped() {
        let mut v = view();
        v.pending();
        v.handle_key(key(KeyCode::Char('n'))); // b.rs
        assert_eq!(v.current, Some(1));
        let reqs = v.pending();
        assert!(matches!(reqs.as_slice(), [DiffReq::File { file: 1, seq: 2, .. }]));
        v.set_file_diff(1, 0, Ok(FileDiff::message("old".into())));
        assert!(v.diff.is_none());
        v.set_file_diff(2, 1, Ok(FileDiff::message("new".into())));
        assert_eq!(v.diff_for, Some(1));
    }

    #[test]
    fn preloaded_changes_open_at_the_cursor() {
        let mut cs = ChangeSet::new(3, Some(id(1)), id(2));
        cs.requested = true;
        cs.set_tree(Ok(TreeDiff {
            files: vec![change("src/a.rs"), change("src/b.rs"), change("top.rs")],
        }));
        let cursor = TreeCursor {
            sel: 3,
            scroll: 0,
            rows: 5,
        };
        let mut v = DiffView::new(2, cs, Some(cursor), 30);
        assert_eq!(v.current, Some(2), "top.rs under the cursor");
        let reqs = v.pending();
        assert!(matches!(reqs.as_slice(), [DiffReq::File { file: 2, .. }]), "no tree request");
    }

    #[test]
    fn folder_keys_and_mouse() {
        let mut v = view();
        v.handle_key(key(KeyCode::Home)); // the "src" folder
        v.handle_key(key(KeyCode::Left)); // collapse
        assert_eq!(v.changes.tree.visible.len(), 2);
        v.handle_key(key(KeyCode::Right)); // expand again
        assert_eq!(v.changes.tree.visible.len(), 4);
        v.handle_key(key(KeyCode::Right)); // step into first child
        assert_eq!(v.cursor.sel, 1);
        v.handle_key(key(KeyCode::Enter)); // file: focus the diff
        assert_eq!(v.focus, DiffFocus::Diff);
        let areas = DiffAreas {
            header: Rect::new(0, 0, 100, 1),
            sidebar: Rect::new(0, 1, 30, 10),
            splitter: Rect::new(30, 1, 1, 10),
            main: Rect::new(31, 1, 69, 10),
            status: Rect::new(0, 11, 100, 1),
        };
        let click = |x, y| MouseEvent {
            kind: MouseKind::Down(Button::Left),
            x,
            y,
            ctrl: false,
            shift: false,
        };
        v.handle_mouse(click(5, 1), &areas); // click "src": selects and collapses
        assert_eq!(v.focus, DiffFocus::Files);
        assert_eq!(v.changes.tree.visible.len(), 2);
        v.handle_mouse(click(5, 2), &areas); // "top.rs"
        assert_eq!(v.current, Some(2));
        v.handle_key(key(KeyCode::Esc));
        assert!(v.close);
    }
}
