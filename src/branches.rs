//! Branch browser state. No filesystem or network work happens here.
use crate::git::{RefKind, RefSnapshot};
use crate::remote;
use crate::term::{KeyCode, KeyEvent};
use gix::ObjectId;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub remote: Option<String>,
    /// Present for an API branch; guards against a reconfigured remote at checkout.
    pub url: Option<String>,
    pub sha: String,
}

impl Target {
    pub fn label(&self) -> String {
        match &self.remote {
            Some(r) => format!("{r}/{}", self.name),
            None => self.name.clone(),
        }
    }
}

pub struct Entry {
    pub target: Target,
    pub current: bool,
    pub source: &'static str,
    search: String,
}

#[derive(Default)]
pub struct Browser {
    pub open: bool,
    pub entries: Vec<Entry>,
    pub visible: Vec<usize>,
    pub selected: usize,
    pub scroll: usize,
    pub rows: usize,
    pub query: String,
    pub loading: bool,
    pub busy: bool,
    pub status: String,
    pub errors: Vec<String>,
    pub want_refresh: Option<bool>,
    pub checkout: Option<Target>,
    pub jump: Option<ObjectId>,
    pub remotes: BTreeMap<String, remote::Update>,
    pub last_check: Option<std::time::Instant>,
}

impl Browser {
    pub fn show(&mut self, refs: Option<&RefSnapshot>) {
        self.open = true;
        self.rebuild(refs);
        if !self.loading
            && (self.remotes.values().any(|u| u.scoped)
                || self
                    .last_check
                    .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(300)))
        {
            self.want_refresh = Some(false);
        }
    }

    pub fn update(&mut self, update: remote::Update, refs: Option<&RefSnapshot>) {
        self.remotes.insert(update.remote.name.clone(), update);
        self.rebuild(refs);
    }

    pub fn rebuild(&mut self, refs: Option<&RefSnapshot>) {
        let keep = self
            .chosen()
            .map(|e| (e.target.remote.clone(), e.target.name.clone()));
        let mut entries = BTreeMap::new();
        if let Some(refs) = refs {
            for r in &refs.refs {
                let target = match r.kind {
                    RefKind::Branch | RefKind::Head
                        if refs.head_branch.as_ref() == Some(&r.name)
                            || r.kind == RefKind::Branch =>
                    {
                        Target {
                            name: r.name.clone(),
                            remote: None,
                            url: None,
                            sha: r.id.to_string(),
                        }
                    }
                    RefKind::Remote => {
                        let Some((remote, name)) = r.name.split_once('/') else {
                            continue;
                        };
                        if name == "HEAD" {
                            continue;
                        }
                        Target {
                            name: name.into(),
                            remote: Some(remote.into()),
                            url: None,
                            sha: r.id.to_string(),
                        }
                    }
                    _ => continue,
                };
                let current =
                    target.remote.is_none() && refs.head_branch.as_ref() == Some(&target.name);
                let source = if target.remote.is_none() {
                    "local"
                } else {
                    "tracking (local)"
                };
                let search = target.label().to_lowercase();
                entries.insert(
                    (target.remote.clone(), target.name.clone()),
                    Entry {
                        target,
                        current,
                        source,
                        search,
                    },
                );
            }
        }
        for (name, update) in &self.remotes {
            // A successful complete list is authoritative, including branch deletions.
            if !update.stale && !update.scoped {
                entries.retain(|(remote, _), _| remote.as_ref() != Some(name));
            }
            for branch in &update.branches {
                let target = Target {
                    name: branch.name.clone(),
                    remote: Some(name.clone()),
                    url: Some(update.remote.url.clone()),
                    sha: branch.sha.clone(),
                };
                let source = if update.stale {
                    "remote · stale cache"
                } else if update.cached {
                    "remote · cached"
                } else {
                    "remote · live"
                };
                let search = target.label().to_lowercase();
                entries.insert(
                    (target.remote.clone(), target.name.clone()),
                    Entry {
                        target,
                        current: false,
                        source,
                        search,
                    },
                );
            }
        }
        self.entries = entries.into_values().collect();
        self.filter();
        if let Some(keep) = keep
            && let Some(i) = self.visible.iter().position(|&i| {
                (
                    self.entries[i].target.remote.clone(),
                    self.entries[i].target.name.clone(),
                ) == keep
            })
        {
            self.selected = i;
        }
        self.ensure_visible();
    }

    fn filter(&mut self) {
        let q = self.query.to_lowercase();
        self.visible = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.search.contains(&q))
            .map(|(i, _)| i)
            .collect();
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
        self.ensure_visible();
    }

    pub fn chosen(&self) -> Option<&Entry> {
        self.visible.get(self.selected).map(|&i| &self.entries[i])
    }

    pub fn ensure_visible(&mut self) {
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        if self.selected >= self.scroll + self.rows.max(1) {
            self.scroll = self.selected + 1 - self.rows.max(1);
        }
        self.scroll = self
            .scroll
            .min(self.visible.len().saturating_sub(self.rows.max(1)));
    }

    pub fn move_by(&mut self, delta: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.visible.len().saturating_sub(1));
        self.ensure_visible();
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        match (key.code, key.ctrl) {
            (KeyCode::Esc, _) => self.open = false,
            (KeyCode::Up, _) => self.move_by(-1),
            (KeyCode::Down, _) => self.move_by(1),
            (KeyCode::PageUp, _) => self.move_by(-(self.rows.max(1) as isize)),
            (KeyCode::PageDown, _) => self.move_by(self.rows.max(1) as isize),
            (KeyCode::Home, _) => {
                self.selected = 0;
                self.ensure_visible();
            }
            (KeyCode::End, _) => {
                self.selected = self.visible.len().saturating_sub(1);
                self.ensure_visible();
            }
            (KeyCode::F(5), _) | (KeyCode::Char('r'), true) if !self.loading && !self.busy => {
                self.want_refresh = Some(true)
            }
            (KeyCode::Enter, _) if !self.busy => {
                if let Some(entry) = self.chosen() {
                    self.checkout = Some(entry.target.clone());
                }
            }
            (KeyCode::Tab, _) => {
                self.jump = self
                    .chosen()
                    .and_then(|e| ObjectId::from_hex(e.target.sha.as_bytes()).ok());
            }
            (KeyCode::Backspace, _) => {
                self.query.pop();
                self.selected = 0;
                self.filter();
            }
            (KeyCode::Char('u'), true) => {
                self.query.clear();
                self.selected = 0;
                self.filter();
            }
            (KeyCode::Char(c), false) if !key.alt && !c.is_control() && self.query.len() < 256 => {
                self.query.push(c);
                self.selected = 0;
                self.filter();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deleted_remote_branches_and_selection() {
        let remote = remote::Remote::parse("origin", "git@github.com:git/git.git", None).unwrap();
        let mut browser = Browser::default();
        let update = |names: &[&str]| remote::Update {
            remote: remote.clone(),
            branches: names
                .iter()
                .map(|n| remote::Branch {
                    name: (*n).into(),
                    sha: "a".repeat(40),
                })
                .collect(),
            scoped: false,
            cached: false,
            stale: false,
        };
        browser.update(update(&["a", "b"]), None);
        browser.selected = 1;
        browser.update(update(&["0", "a", "b"]), None);
        assert_eq!(browser.chosen().unwrap().target.name, "b");
        browser.update(update(&[]), None);
        assert!(browser.entries.is_empty());
    }
}
