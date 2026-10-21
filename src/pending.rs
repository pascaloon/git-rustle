//! Pending-change snapshots and the commit form. Git work runs in `git::worktree`.
use crate::diff::FileChange;
use crate::term::{KeyCode, KeyEvent, Rect};
use gix::ObjectId;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub fingerprint: u64,
    pub stash_stamp: Option<u64>,
    pub head: Option<ObjectId>,
    pub branch: Option<String>,
    pub files: Vec<FileChange>,
    pub conflicts: bool,
}

#[derive(Debug, Clone)]
pub struct Request {
    pub head: Option<ObjectId>,
    pub branch: Option<String>,
    pub new_branch: String,
    pub description: String,
    pub files: Vec<FileChange>,
}

#[derive(Debug, Default)]
pub struct Text {
    pub value: String,
    pub cursor: usize,
}
impl Text {
    pub fn edit(&mut self, key: KeyEvent, multiline: bool) {
        let text = &mut self.value;
        let cursor = self.cursor.min(text.len());
        let prev = text[..cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i);
        let next = text[cursor..]
            .chars()
            .next()
            .map_or(cursor, |c| cursor + c.len_utf8());
        self.cursor = match (key.code, key.ctrl) {
            (KeyCode::Left, _) => prev,
            (KeyCode::Right, _) => next,
            (KeyCode::Home, _) => text[..cursor].rfind('\n').map_or(0, |i| i + 1),
            (KeyCode::End, _) => text[cursor..].find('\n').map_or(text.len(), |i| cursor + i),
            (KeyCode::Up, _) | (KeyCode::Down, _) if multiline => {
                let start = text[..cursor].rfind('\n').map_or(0, |i| i + 1);
                let column = text[start..cursor].chars().count();
                let target = if key.code == KeyCode::Up {
                    start
                        .checked_sub(1)
                        .map(|end| (text[..end].rfind('\n').map_or(0, |i| i + 1), end))
                } else {
                    text[cursor..].find('\n').map(|i| {
                        let start = cursor + i + 1;
                        (
                            start,
                            text[start..].find('\n').map_or(text.len(), |j| start + j),
                        )
                    })
                };
                target.map_or(cursor, |(a, b)| {
                    a + text[a..b]
                        .char_indices()
                        .nth(column)
                        .map_or(b - a, |(i, _)| i)
                })
            }
            (KeyCode::Char('u'), true) => {
                text.clear();
                0
            }
            (KeyCode::Backspace, _) => {
                text.replace_range(prev..cursor, "");
                prev
            }
            (KeyCode::Delete, _) => {
                text.replace_range(cursor..next, "");
                cursor
            }
            (KeyCode::Enter, false) if multiline && text.len() < 65536 => {
                text.insert(cursor, '\n');
                cursor + 1
            }
            (KeyCode::Char(c), false)
                if !key.alt && !c.is_control() && text.len() + c.len_utf8() <= 65536 =>
            {
                text.insert(cursor, c);
                cursor + c.len_utf8()
            }
            _ => cursor,
        };
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Areas {
    pub description: Rect,
    pub branch: Rect,
    pub commit: Rect,
    pub check_all: Rect,
    pub uncheck_all: Rect,
}

pub struct Pending {
    pub snapshot: Snapshot,
    pub known: bool,
    pub loading: bool,
    pub want_refresh: bool,
    pub busy: bool,
    pub request: Option<Request>,
    pub selected: bool,
    pub autofocus: bool,
    pub return_to_head: Option<ObjectId>,
    pub head_index: Option<usize>,
    pub searched: usize,
    pub description: Text,
    pub checks: std::collections::HashMap<String, bool>,
    pub branch: Text,
    /// Description, branch, commit, check all, uncheck all.
    pub field: usize,
    pub areas: Areas,
    pub status: String,
    pub error: bool,
}
impl Default for Pending {
    fn default() -> Self {
        Self {
            snapshot: Snapshot::default(),
            known: true,
            loading: false,
            want_refresh: false,
            busy: false,
            request: None,
            selected: false,
            autofocus: false,
            return_to_head: None,
            head_index: None,
            searched: 0,
            description: Text::default(),
            checks: Default::default(),
            branch: Text::default(),
            field: 0,
            areas: Areas::default(),
            status: String::new(),
            error: false,
        }
    }
}
impl Pending {
    pub fn exists(&self) -> bool {
        !self.snapshot.files.is_empty()
    }
    pub fn row(&self) -> usize {
        self.head_index.unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }
    #[test]
    fn description_edits_unicode_and_multiline_without_splitting_codepoints() {
        let mut text = Text::default();
        for ch in "éclair".chars() {
            text.edit(key(KeyCode::Char(ch)), true);
        }
        text.edit(key(KeyCode::Enter), true);
        for ch in "two".chars() {
            text.edit(key(KeyCode::Char(ch)), true);
        }
        text.edit(key(KeyCode::Up), true);
        assert_eq!(text.cursor, "écl".len());
        text.edit(key(KeyCode::Home), true);
        text.edit(key(KeyCode::Right), true);
        text.edit(key(KeyCode::Backspace), true);
        assert_eq!(text.value, "clair\ntwo");
        assert_eq!(text.cursor, 0);
        text.edit(key(KeyCode::Down), true);
        assert_eq!(text.cursor, "clair\n".len());
        text.edit(key(KeyCode::Enter), false);
        assert_eq!(text.value, "clair\ntwo");
    }
}
