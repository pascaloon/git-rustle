#![allow(dead_code)] // Legacy adapters remain as regression-test references.
//! Branch browser state and bounded worker-only local activity scoring.
use crate::git::{RefKind, RefSnapshot};
use crate::remote;
use crate::term::{KeyCode, KeyEvent};
use gix::ObjectId;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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

/// A local branch has one configured upstream, or one unambiguous fallback.
/// Keep the ordinary unpaired row pointer-sized and allocation-free.
#[derive(Default)]
pub struct PairedRemote(Option<Box<Target>>);
impl std::ops::Deref for PairedRemote {
    type Target = [Target];
    fn deref(&self) -> &[Target] {
        self.0.as_deref().map(std::slice::from_ref).unwrap_or(&[])
    }
}
impl PairedRemote {
    fn insert(&mut self, target: Target) {
        assert!(
            self.0.is_none(),
            "a local branch can pair with only one upstream"
        );
        self.0 = Some(Box::new(target));
    }
}
pub struct Entry {
    pub target: Target,
    pub paired_remotes: PairedRemote,
    pub current: bool,
    /// A real local branch exists by exact name or tracks this remote branch.
    pub local_available: bool,
    search: String,
    author_search: String,
    pub activity: Option<Activity>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortColumn {
    Name,
    #[default]
    Activity,
    LastCommit,
}

#[derive(Clone, Copy, Debug)]
pub struct Sort {
    pub column: SortColumn,
    pub descending: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Self {
            column: SortColumn::Activity,
            descending: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Activity {
    pub author: String,
    pub author_time: i64,
    pub commit_time: i64,
    pub recent: u32,
    /// Only the last 100 first-parent commits are examined; missing ancestry
    /// also makes the count a lower bound.
    pub limited: bool,
}

pub const WEEK: i64 = 7 * 86_400;

/// Bounded first-parent scoring: merges do not multiply shared side histories.
/// Called on a worker, once per set of local branch tips.
pub fn local_activity(
    repo: &gix::Repository,
    tips: &[Target],
    now: i64,
) -> Vec<(Target, Activity)> {
    let mut known = HashMap::new();
    tips.iter()
        .filter_map(|target| {
            let id = ObjectId::from_hex(target.sha.as_bytes()).ok()?;
            let tip = crate::git::commit_details(repo, &id).ok()?;
            let mut next = Some(id);
            let mut recent = 0;
            let mut visited = HashSet::new();
            for _ in 0..100 {
                let Some(id) = next else { break };
                if !visited.insert(id) {
                    break;
                }
                if let std::collections::hash_map::Entry::Vacant(entry) = known.entry(id) {
                    let Ok(details) = crate::git::commit_details(repo, &id) else {
                        break;
                    };
                    entry.insert(details);
                }
                let c = &known[&id];
                recent += u32::from(c.commit_time >= now - WEEK && c.commit_time <= now);
                next = c.parents.first().copied();
            }
            Some((
                target.clone(),
                Activity {
                    author: tip.author_name,
                    author_time: tip.author_time,
                    commit_time: tip.commit_time,
                    recent,
                    limited: next.is_some(),
                },
            ))
        })
        .collect()
}

#[derive(Clone, Debug)]
pub enum TreeRow {
    Folder {
        path: String,
        name: String,
        depth: usize,
        count: usize,
    },
    Branch {
        visible: usize,
        name: String,
        depth: usize,
    },
}
#[derive(Default)]
struct Folder {
    children: BTreeMap<String, Folder>,
    leaves: Vec<(usize, String)>,
    count: usize,
}
fn merge_remote_rows(entries: &mut Vec<Entry>, refs: Option<&RefSnapshot>) {
    if !entries.iter().any(|e| e.target.remote.is_some()) {
        return;
    }
    let locals: HashMap<_, _> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.target.remote.is_none())
        .map(|(index, e)| (e.target.name.clone(), index))
        .collect();
    let mut exact = HashMap::<(String, String, String), usize>::new();
    for (name, &index) in &locals {
        if let Some((remote, branch)) = refs.and_then(|r| r.upstreams.get(name)) {
            let key = (
                remote.clone(),
                branch.clone(),
                entries[index].target.sha.clone(),
            );
            let chosen = exact.entry(key).or_insert(index);
            if entries[index].current && !entries[*chosen].current
                || entries[index].current == entries[*chosen].current
                    && entries[index].target.name < entries[*chosen].target.name
            {
                *chosen = index;
            }
        }
    }
    let mut fallback_count = HashMap::<usize, usize>::new();
    for entry in entries.iter().filter(|e| e.target.remote.is_some()) {
        if let Some(&index) = locals.get(&entry.target.name)
            && !refs.is_some_and(|r| r.upstreams.contains_key(&entry.target.name))
            && entries[index].target.sha == entry.target.sha
        {
            *fallback_count.entry(index).or_default() += 1;
        }
    }
    let mut pairs = Vec::new();
    for (remote_index, entry) in entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.target.remote.is_some())
    {
        let key = (
            entry.target.remote.clone().unwrap(),
            entry.target.name.clone(),
            entry.target.sha.clone(),
        );
        let local = exact.get(&key).copied().or_else(|| {
            locals.get(&entry.target.name).copied().filter(|index| {
                fallback_count.get(index) == Some(&1)
                    && entries[*index].target.sha == entry.target.sha
            })
        });
        if let Some(local) = local {
            pairs.push((local, remote_index));
        }
    }
    let mut removed = HashSet::new();
    for (local, remote) in pairs {
        let target = entries[remote].target.clone();
        entries[local].search.push(' ');
        entries[local]
            .search
            .push_str(&target.label().to_lowercase());
        entries[local].paired_remotes.insert(target);
        removed.insert(remote);
    }
    let mut index = 0;
    entries.retain(|_| {
        let keep = !removed.contains(&index);
        index += 1;
        keep
    });
}
#[derive(Default)]
pub struct Browser {
    pub open: bool,
    pub entries: Vec<Entry>,
    pub visible: Vec<usize>,
    pub selected: usize,
    pub scroll: usize,
    follow_top: bool,
    pub rows: usize,
    pub query: String,
    pub loading: bool,
    pub busy: bool,
    pub status: String,
    pub errors: Vec<String>,
    pub want_refresh: Option<bool>,
    pub jump: Option<ObjectId>,
    pub preview_targets: Vec<Target>,
    pub preview_requested: bool,
    pub pending_jump: Option<ObjectId>,
    pub activity_cache: HashMap<Target, Activity>,
    pub activity_attempted: HashSet<Target>,
    pub activity_errors: HashMap<Target, String>,
    pub activity_requests: usize,
    pub remotes: BTreeMap<String, remote::Update>,
    /// Cached remote-ref tips, distinct from local branches and their upstreams.
    fetched_tips: HashMap<String, HashMap<String, String>>,
    pub last_check: Option<std::time::Instant>,
    pub sort: Sort,
    pub hierarchy: bool,
    pub tree_rows: Vec<TreeRow>,
    pub collapsed: HashSet<String>,
    pub matching_count: usize,
}

impl Browser {
    /// Exact cached-tip membership, with borrowed lookups during row painting.
    pub fn remote_fetched(&self, target: &Target) -> bool {
        target
            .remote
            .as_ref()
            .and_then(|remote| self.fetched_tips.get(remote))
            .and_then(|branches| branches.get(&target.name))
            .is_some_and(|sha| sha == &target.sha)
    }
    pub fn show(&mut self, refs: Option<&RefSnapshot>) {
        self.reset_to_top();
        self.rebuild(refs);
        self.resume();
    }

    /// Reopening a tab must upgrade the startup's scoped discovery as well.
    pub fn resume(&mut self) {
        self.open = true;
        if !self.loading
            && (!self.errors.is_empty()
                || self.remotes.values().any(|u| u.scoped)
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

    pub fn merge_scoped(
        &self,
        mut update: remote::Update,
        scope: &HashSet<String>,
    ) -> remote::Update {
        if let Some(previous) = self
            .remotes
            .get(&update.remote.name)
            .filter(|old| old.remote == update.remote)
        {
            update.branches.extend(
                previous
                    .branches
                    .iter()
                    .filter(|b| !scope.contains(&b.name))
                    .cloned(),
            );
            update.scoped = previous.scoped;
        }
        update.branches.sort_by(|a, b| a.name.cmp(&b.name));
        update
    }

    pub fn rebuild(&mut self, refs: Option<&RefSnapshot>) {
        for tips in self.fetched_tips.values_mut() {
            tips.clear();
        }
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
                        let sha = r.id.to_string();
                        if let Some(tips) = self.fetched_tips.get_mut(remote) {
                            tips.insert(name.into(), sha.clone());
                        } else {
                            let mut tips = HashMap::new();
                            tips.insert(name.into(), sha.clone());
                            self.fetched_tips.insert(remote.into(), tips);
                        }
                        Target {
                            name: name.into(),
                            remote: Some(remote.into()),
                            url: None,
                            sha,
                        }
                    }
                    _ => continue,
                };
                let current =
                    target.remote.is_none() && refs.head_branch.as_ref() == Some(&target.name);
                let search = target.label().to_lowercase();
                entries.insert(
                    (target.remote.clone(), target.name.clone()),
                    Entry {
                        target,
                        paired_remotes: PairedRemote::default(),
                        current,
                        local_available: true,
                        search,
                        author_search: String::new(),
                        activity: None,
                    },
                );
            }
        }
        self.fetched_tips.retain(|_, tips| !tips.is_empty());
        // No extra index work for the ordinary local-only case. Names are
        // borrowed from the immutable snapshot, never folded or copied.
        let has_remotes = !self.remotes.is_empty()
            || entries
                .last_key_value()
                .is_some_and(|((remote, _), _)| remote.is_some());
        let local_names: HashSet<&str> = if has_remotes {
            refs.into_iter()
                .flat_map(|refs| &refs.refs)
                .filter(|r| {
                    r.kind == RefKind::Branch
                        || (r.kind == RefKind::Head
                            && refs.is_some_and(|s| s.head_branch.as_ref() == Some(&r.name)))
                })
                .map(|r| r.name.as_str())
                .collect()
        } else {
            HashSet::new()
        };
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
                let search = target.label().to_lowercase();
                entries.insert(
                    (target.remote.clone(), target.name.clone()),
                    Entry {
                        target,
                        paired_remotes: PairedRemote::default(),
                        current: false,
                        local_available: false,
                        search,
                        author_search: String::new(),
                        activity: None,
                    },
                );
            }
        }
        self.entries = entries.into_values().collect();
        let mut by_sha: HashMap<&str, &Activity> =
            HashMap::with_capacity(self.activity_cache.len());
        for (target, activity) in &self.activity_cache {
            let keep = by_sha.entry(&target.sha).or_insert(activity);
            if activity.recent > keep.recent {
                *keep = activity;
            }
        }
        for e in &mut self.entries {
            if let Some(remote) = &e.target.remote {
                e.local_available = local_names.contains(e.target.name.as_str())
                    || refs
                        .and_then(|s| s.tracking.get(remote))
                        .is_some_and(|names| names.contains(&e.target.name));
            }
            e.activity = self
                .activity_cache
                .get(&e.target)
                .or_else(|| by_sha.get(e.target.sha.as_str()).copied())
                .cloned();
        }
        merge_remote_rows(&mut self.entries, refs);
        self.sort_entries();
        self.filter();
        if self.follow_top {
            self.selected = 0;
            self.scroll = 0;
        } else if let Some(keep) = keep
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

    pub fn set_sort(&mut self, column: SortColumn) {
        let keep = self
            .chosen()
            .map(|e| (e.target.remote.clone(), e.target.name.clone()));
        self.sort = Sort {
            column,
            descending: if self.sort.column == column {
                !self.sort.descending
            } else {
                column != SortColumn::Name
            },
        };
        self.sort_entries();
        self.filter();
        if let Some((remote, name)) = keep
            && let Some(index) = self.visible.iter().position(|&i| {
                self.entries[i].target.remote == remote && self.entries[i].target.name == name
            })
        {
            self.selected = index;
        }
        self.follow_top = false;
        self.ensure_visible();
    }

    fn sort_entries(&mut self) {
        let sort = self.sort;
        if sort.column == SortColumn::Activity && sort.descending {
            // Keep the existing default hot path free of per-comparison mode
            // dispatch. Header sorts are only paid for when explicitly chosen.
            self.entries.sort_by(|a, b| {
                let rank = |e: &Entry| e.activity.as_ref().map(|a| (a.recent, a.commit_time));
                rank(b)
                    .cmp(&rank(a))
                    .then_with(|| a.target.remote.cmp(&b.target.remote))
                    .then_with(|| a.target.name.cmp(&b.target.name))
            });
            return;
        }
        self.entries.sort_by(|a, b| {
            let order = match sort.column {
                SortColumn::Name => a.target.name.cmp(&b.target.name),
                SortColumn::Activity => {
                    let rank = |e: &Entry| e.activity.as_ref().map(|a| (a.recent, a.commit_time));
                    rank(a).cmp(&rank(b))
                }
                SortColumn::LastCommit => {
                    let rank = |e: &Entry| e.activity.as_ref().map(|a| a.commit_time);
                    rank(a).cmp(&rank(b))
                }
            };
            let order = if sort.descending {
                order.reverse()
            } else {
                order
            };
            order
                .then_with(|| a.target.remote.cmp(&b.target.remote))
                .then_with(|| a.target.name.cmp(&b.target.name))
        });
    }

    pub fn filter(&mut self) {
        let q = self.query.to_lowercase();
        let filtered: Vec<usize> = self
            .entries
            .iter_mut()
            .enumerate()
            .filter_map(|(i, e)| {
                // Empty/name-matching queries never need an author index.
                if e.search.contains(&q) {
                    return Some(i);
                }
                if e.author_search.is_empty()
                    && let Some(activity) = e.activity.as_ref().filter(|a| !a.author.is_empty())
                {
                    e.author_search = activity.author.to_lowercase();
                }
                e.author_search.contains(&q).then_some(i)
            })
            .collect();
        self.matching_count = filtered.len();
        if self.hierarchy {
            self.build_tree(filtered);
        } else {
            self.visible = filtered;
            self.tree_rows.clear();
        }
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
        self.ensure_visible();
    }

    pub fn set_hierarchy(&mut self, hierarchy: bool) {
        let keep = self.chosen().map(|e| e.target.clone());
        self.hierarchy = hierarchy;
        self.filter();
        if let Some(keep) = keep
            && let Some(index) = self
                .visible
                .iter()
                .position(|&i| self.entries[i].target == keep)
        {
            self.selected = index;
        }
        self.ensure_visible();
    }
    pub fn toggle_folder(&mut self, path: String) {
        if !self.collapsed.remove(&path) {
            self.collapsed.insert(path);
        }
        self.filter();
    }
    fn build_tree(&mut self, filtered: Vec<usize>) {
        let mut root = Folder::default();
        for index in filtered {
            let label = self.entries[index].target.label();
            let parts: Vec<_> = label.splitn(33, '/').collect();
            let mut folder = &mut root;
            folder.count += 1;
            for part in &parts[..parts.len().saturating_sub(1)] {
                folder = folder.children.entry((*part).to_string()).or_default();
                folder.count += 1;
            }
            folder
                .leaves
                .push((index, parts.last().unwrap_or(&"").to_string()));
        }
        self.visible.clear();
        self.tree_rows.clear();
        fn emit(
            node: Folder,
            path: &str,
            depth: usize,
            collapsed: &HashSet<String>,
            searching: bool,
            visible: &mut Vec<usize>,
            rows: &mut Vec<TreeRow>,
        ) {
            for (name, child) in node.children {
                let path = if path.is_empty() {
                    name.clone()
                } else {
                    format!("{path}/{name}")
                };
                rows.push(TreeRow::Folder {
                    path: path.clone(),
                    name,
                    depth,
                    count: child.count,
                });
                if searching || !collapsed.contains(&path) {
                    emit(child, &path, depth + 1, collapsed, searching, visible, rows);
                }
            }
            for (index, name) in node.leaves {
                rows.push(TreeRow::Branch {
                    visible: visible.len(),
                    name,
                    depth,
                });
                visible.push(index);
            }
        }
        emit(
            root,
            "",
            0,
            &self.collapsed,
            !self.query.is_empty(),
            &mut self.visible,
            &mut self.tree_rows,
        );
    }
    pub fn display_count(&self) -> usize {
        if self.hierarchy {
            self.tree_rows.len()
        } else {
            self.visible.len()
        }
    }
    pub fn chosen(&self) -> Option<&Entry> {
        self.visible.get(self.selected).map(|&i| &self.entries[i])
    }

    pub fn install_activity(
        &mut self,
        values: Vec<(Target, Activity)>,
        refs: Option<&RefSnapshot>,
    ) {
        self.activity_cache.extend(values);
        self.rebuild(refs);
    }

    pub fn next_remote_activity(&mut self) -> Option<(Target, remote::Remote)> {
        let entry = self
            .chosen()
            .into_iter()
            .chain(
                self.visible
                    .iter()
                    .skip(self.scroll)
                    .take(self.rows.max(1))
                    .map(|&i| &self.entries[i]),
            )
            .chain(self.entries.iter().filter(|_| self.activity_requests < 100))
            .find(|e| {
                e.target.url.is_some()
                    && e.activity.is_none()
                    && !self.activity_attempted.contains(&e.target)
            })?;
        let target = entry.target.clone();
        let remote = self.remotes.get(target.remote.as_ref()?)?.remote.clone();
        self.activity_attempted.insert(target.clone());
        self.activity_requests += 1;
        Some((target, remote))
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

    fn reset_to_top(&mut self) {
        self.follow_top = true;
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn select(&mut self, index: usize) {
        self.follow_top = false;
        self.selected = index.min(self.visible.len().saturating_sub(1));
        self.ensure_visible();
    }

    pub fn move_by(&mut self, delta: isize) {
        self.select(self.selected.saturating_add_signed(delta));
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        match (key.code, key.ctrl) {
            (KeyCode::Esc, _) => self.open = false,
            (KeyCode::Up, _) => self.move_by(-1),
            (KeyCode::Down, _) => self.move_by(1),
            (KeyCode::PageUp, _) => self.move_by(-(self.rows.max(1) as isize)),
            (KeyCode::PageDown, _) => self.move_by(self.rows.max(1) as isize),
            (KeyCode::Home, _) => {
                self.reset_to_top();
                self.ensure_visible();
            }
            (KeyCode::End, _) => {
                self.select(usize::MAX);
            }
            (KeyCode::F(5), _) | (KeyCode::Char('r'), true) if !self.loading && !self.busy => {
                self.want_refresh = Some(true)
            }
            (KeyCode::Tab, _) => {
                self.jump = self
                    .chosen()
                    .and_then(|e| ObjectId::from_hex(e.target.sha.as_bytes()).ok());
            }
            (KeyCode::Backspace, _) => {
                self.query.pop();
                self.reset_to_top();
                self.filter();
            }
            (KeyCode::Char('u'), true) => {
                self.query.clear();
                self.reset_to_top();
                self.filter();
            }
            (KeyCode::Char(c), false) if !key.alt && !c.is_control() && self.query.len() < 256 => {
                self.query.push(c);
                self.reset_to_top();
                self.filter();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{RefEntry, RefKind};

    #[test]
    fn sort_headers_override_order_toggle_and_preserve_selected_identity() {
        let refs = RefSnapshot {
            refs: ["zeta", "alpha", "middle"]
                .into_iter()
                .enumerate()
                .map(|(i, name)| RefEntry {
                    name: name.into(),
                    id: ObjectId::from_hex(format!("{:040x}", i + 1).as_bytes()).unwrap(),
                    kind: RefKind::Branch,
                })
                .collect(),
            ..Default::default()
        };
        let mut browser = Browser {
            rows: 1,
            ..Default::default()
        };
        browser.rebuild(Some(&refs));
        let values = browser
            .entries
            .iter()
            .map(|e| {
                (
                    e.target.clone(),
                    Activity {
                        author: "Owner".into(),
                        author_time: 1,
                        commit_time: if e.target.name == "middle" { 30 } else { 10 },
                        recent: if e.target.name == "zeta" { 5 } else { 1 },
                        limited: false,
                    },
                )
            })
            .collect();
        browser.install_activity(values, Some(&refs));
        browser.select(1);
        let chosen = browser.chosen().unwrap().target.clone();
        browser.entries.reverse(); // A header always overrides arbitrary current ordering.
        browser.filter();
        let index = browser
            .visible
            .iter()
            .position(|&i| browser.entries[i].target == chosen)
            .unwrap();
        browser.select(index);
        browser.set_sort(SortColumn::Name);
        assert_eq!(browser.entries[0].target.name, "alpha");
        assert!(!browser.sort.descending);
        assert_eq!(browser.chosen().unwrap().target, chosen);
        browser.set_sort(SortColumn::Name);
        assert_eq!(browser.entries[0].target.name, "zeta");
        browser.set_sort(SortColumn::LastCommit);
        assert_eq!(browser.entries[0].target.name, "middle");
        browser.set_sort(SortColumn::LastCommit);
        assert_eq!(browser.entries[0].target.name, "alpha");
        browser.set_sort(SortColumn::Activity);
        assert_eq!(browser.entries[0].target.name, "zeta");
        browser.query = "Owner".into();
        browser.filter();
        browser.set_sort(SortColumn::Activity);
        assert_eq!(browser.entries[0].target.name, "alpha");
        assert_eq!(browser.visible.len(), 3);
        assert_eq!(browser.chosen().unwrap().target, chosen);
        browser.rebuild(Some(&refs));
        assert!(!browser.sort.descending);
        assert_eq!(browser.entries[0].target.name, "alpha");
        assert_eq!(browser.chosen().unwrap().target, chosen);
    }

    #[test]
    fn remote_availability_requires_exact_local_name_or_upstream_not_objects() {
        let id = ObjectId::from_hex(b"1111111111111111111111111111111111111111").unwrap();
        let mut refs = RefSnapshot {
            head_branch: Some("local-work".into()),
            head_id: Some(id),
            refs: [
                ("local-work", RefKind::Head),
                ("same", RefKind::Branch),
                ("origin/same", RefKind::Remote),
                ("origin/remote-only", RefKind::Remote),
                ("origin/tracked", RefKind::Remote),
                ("origin/Same", RefKind::Remote),
            ]
            .into_iter()
            .map(|(name, kind)| RefEntry {
                name: name.into(),
                id,
                kind,
            })
            .collect(),
            ..Default::default()
        };
        refs.tracking
            .insert("origin".into(), HashSet::from(["tracked".into()]));
        let mut browser = Browser::default();
        browser.rebuild(Some(&refs));
        let available = |name: &str| {
            browser
                .entries
                .iter()
                .find(|e| {
                    e.target.label() == name || e.paired_remotes.iter().any(|r| r.label() == name)
                })
                .unwrap()
                .local_available
        };
        assert!(available("local-work") && available("same"));
        assert!(available("origin/same") && available("origin/tracked"));
        let same = browser
            .entries
            .iter()
            .find(|e| e.target.label() == "same")
            .unwrap();
        assert_eq!(
            same.paired_remotes
                .iter()
                .map(Target::label)
                .collect::<Vec<_>>(),
            ["origin/same"]
        );
        assert!(
            !browser
                .entries
                .iter()
                .any(|e| e.target.label() == "origin/same")
        );
        assert!(!available("origin/remote-only") && !available("origin/Same"));
        assert!(
            browser
                .entries
                .iter()
                .find(|e| e.target.name == "local-work")
                .unwrap()
                .current
        );
        refs.tracking.clear();
        browser.rebuild(Some(&refs));
        assert!(
            !browser
                .entries
                .iter()
                .find(|e| e.target.label() == "origin/tracked")
                .unwrap()
                .local_available
        );
    }

    #[test]
    fn reopening_scoped_discovery_requests_complete_list_without_losing_selection() {
        let remote = remote::Remote::parse("origin", "https://github.com/git/git", None).unwrap();
        let mut browser = Browser::default();
        browser.show(Some(&owner_refs()));
        browser.select(1);
        let chosen = browser.chosen().unwrap().target.clone();
        browser.last_check = Some(std::time::Instant::now());
        browser.remotes.insert(
            "origin".into(),
            remote::Update {
                remote,
                branches: Vec::new(),
                scoped: true,
                cached: false,
                stale: false,
            },
        );
        browser.want_refresh = None;
        browser.open = false;
        browser.resume();
        assert_eq!(browser.want_refresh, Some(false));
        assert_eq!(browser.chosen().unwrap().target, chosen);
        browser.want_refresh = None;
        browser.loading = true;
        browser.resume();
        assert!(browser.want_refresh.is_none());
        browser.loading = false;
        browser.remotes.get_mut("origin").unwrap().scoped = false;
        browser.resume();
        assert!(browser.want_refresh.is_none());
        browser.errors.push("another remote failed".into());
        browser.resume();
        assert_eq!(browser.want_refresh, Some(false));
        browser.want_refresh = None;
        browser.rebuild(Some(&owner_refs()));
        assert!(browser.want_refresh.is_none()); // A render/rebuild never retries automatically.
    }

    #[test]
    #[ignore = "explicit header sort measurement at 1,000/10,000 entries"]
    fn benchmark_header_sort() {
        for entries in [1000, 10_000] {
            let refs = RefSnapshot {
                refs: (0..entries)
                    .map(|i| RefEntry {
                        name: format!("branch-{i:05}"),
                        id: ObjectId::from_hex(format!("{:040x}", i + 1).as_bytes()).unwrap(),
                        kind: RefKind::Branch,
                    })
                    .collect(),
                ..Default::default()
            };
            let mut browser = Browser {
                rows: 34,
                ..Default::default()
            };
            browser.rebuild(Some(&refs));
            let values = browser
                .entries
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    (
                        e.target.clone(),
                        Activity {
                            author: "Owner".into(),
                            author_time: 1,
                            commit_time: (i % 97) as i64,
                            recent: (i % 101) as u32,
                            limited: false,
                        },
                    )
                })
                .collect();
            browser.install_activity(values, Some(&refs));
            browser.select(entries / 2);
            let chosen = browser.chosen().unwrap().target.clone();
            for sample in -1..15 {
                let mut times = Vec::new();
                for column in [
                    SortColumn::Name,
                    SortColumn::Activity,
                    SortColumn::LastCommit,
                ] {
                    let start = std::time::Instant::now();
                    for _ in 0..20 {
                        browser.set_sort(column);
                        std::hint::black_box(&browser.visible);
                    }
                    times.push(start.elapsed().as_secs_f64() * 1000.0 / 20.0);
                    assert_eq!(browser.chosen().unwrap().target, chosen);
                }
                if sample >= 0 {
                    println!(
                        "{}",
                        serde_json::json!({"sample":sample,"entries":entries,"nameMs":times[0],"activityMs":times[1],"lastCommitMs":times[2]})
                    );
                }
            }
        }
    }

    // Test-only prototype: both variants share the same bounded worker body.
    // The production Activity layout/ranking remain unchanged until the paired
    // measurements establish that weighting has no cost.
    fn activity_prototype<const WEIGHTED: bool>(
        repo: &gix::Repository,
        tips: &[Target],
        now: i64,
    ) -> Vec<(Target, Activity, u32)> {
        let mut known = HashMap::new();
        tips.iter()
            .filter_map(|target| {
                let id = ObjectId::from_hex(target.sha.as_bytes()).ok()?;
                let tip = crate::git::commit_details(repo, &id).ok()?;
                let mut next = Some(id);
                let mut recent = 0;
                let mut score = 0;
                let mut visited = HashSet::new();
                for _ in 0..100 {
                    let Some(id) = next else { break };
                    if !visited.insert(id) {
                        break;
                    }
                    if let std::collections::hash_map::Entry::Vacant(entry) = known.entry(id) {
                        let Ok(details) = crate::git::commit_details(repo, &id) else {
                            break;
                        };
                        entry.insert(details);
                    }
                    let c = &known[&id];
                    let in_week = c.commit_time >= now - WEEK && c.commit_time <= now;
                    recent += u32::from(in_week);
                    if WEIGHTED && in_week {
                        score += (c.commit_time - (now - WEEK) + 1) as u32;
                    }
                    next = c.parents.first().copied();
                }
                Some((
                    target.clone(),
                    Activity {
                        author: tip.author_name,
                        author_time: tip.author_time,
                        commit_time: tip.commit_time,
                        recent,
                        limited: next.is_some(),
                    },
                    score,
                ))
            })
            .collect()
    }

    #[test]
    #[ignore = "paired old-count/recency prototype; does not change production ranking"]
    fn benchmark_activity_recency_prototype() {
        let path = std::path::PathBuf::from(
            std::env::var_os("GIT_RUSTLE_BENCH_REPO").expect("repository required"),
        );
        let repo = crate::git::open(&path).unwrap();
        let bytes =
            crate::git::command::checked(&path, &["rev-list", "--all", "--max-count=10000"])
                .unwrap();
        let ids: Vec<_> = std::str::from_utf8(&bytes).unwrap().lines().collect();
        assert!(
            ids.len() >= 1000,
            "recency prototype requires at least 1,000 commits"
        );
        let now =
            crate::git::commit_details(&repo, &ObjectId::from_hex(ids[0].as_bytes()).unwrap())
                .unwrap()
                .commit_time;
        for distribution in ["shared", "neighbors", "spread"] {
            let tips: Vec<_> = (0..1000)
                .map(|n| Target {
                    name: format!("prototype-{n:04}"),
                    remote: None,
                    url: None,
                    sha: ids[match distribution {
                        "shared" => 0,
                        "neighbors" => n,
                        _ => n * (ids.len() - 1) / 999,
                    }]
                    .into(),
                })
                .collect();
            for sample in -1..15 {
                let mut old_ms = 0.;
                let mut weighted_ms = 0.;
                let mut old = Vec::new();
                let mut weighted = Vec::new();
                for is_weighted in if sample % 2 == 0 {
                    [true, false]
                } else {
                    [false, true]
                } {
                    let start = std::time::Instant::now();
                    let values = if is_weighted {
                        activity_prototype::<true>(&repo, &tips, now)
                    } else {
                        activity_prototype::<false>(&repo, &tips, now)
                    };
                    let ms = start.elapsed().as_secs_f64() * 1000.;
                    if is_weighted {
                        weighted_ms = ms;
                        weighted = values;
                    } else {
                        old_ms = ms;
                        old = values;
                    }
                }
                assert_eq!(old.len(), 1000);
                assert_eq!(weighted.len(), old.len());
                for ((target, a, _), (other, b, _)) in old.iter().zip(&weighted) {
                    assert_eq!(target, other);
                    assert_eq!(
                        (
                            a.author.as_str(),
                            a.author_time,
                            a.commit_time,
                            a.recent,
                            a.limited
                        ),
                        (
                            b.author.as_str(),
                            b.author_time,
                            b.commit_time,
                            b.recent,
                            b.limited
                        )
                    );
                }
                let score: u64 = weighted.iter().map(|(_, _, score)| u64::from(*score)).sum();
                if sample >= 0 {
                    println!(
                        "{}",
                        serde_json::json!({"sample":sample,"entries":1000,"distribution":distribution,"oldCountMs":old_ms,"weightedMs":weighted_ms,"weightChecksum":score})
                    );
                }
            }
        }
    }

    fn owner_refs() -> RefSnapshot {
        RefSnapshot {
            refs: ["feature/ui", "maintenance"]
                .into_iter()
                .enumerate()
                .map(|(n, name)| RefEntry {
                    name: name.into(),
                    id: ObjectId::from_hex(format!("{:040x}", n + 1).as_bytes()).unwrap(),
                    kind: RefKind::Branch,
                })
                .collect(),
            ..RefSnapshot::default()
        }
    }

    fn owner_activity(author: &str) -> Activity {
        Activity {
            author: author.into(),
            author_time: 1,
            commit_time: 1,
            recent: 1,
            limited: false,
        }
    }

    #[test]
    fn filter_matches_branch_name_or_latest_author_without_combining_fields() {
        let refs = owner_refs();
        let mut browser = Browser::default();
        browser.rebuild(Some(&refs));
        let target = browser
            .entries
            .iter()
            .find(|e| e.target.name == "feature/ui")
            .unwrap()
            .target
            .clone();
        browser.install_activity(vec![(target, owner_activity("Zoë Ångström"))], Some(&refs));
        for query in ["FEATURE/UI", "zOË", "ÅNGSTRÖM"] {
            browser.query = query.into();
            browser.filter();
            assert_eq!(browser.visible.len(), 1);
            assert_eq!(browser.chosen().unwrap().target.name, "feature/ui");
        }
        browser.query = "ui Zoë".into();
        browser.filter();
        assert!(
            browser.visible.is_empty(),
            "query must match within a name or author"
        );
        browser.query.clear();
        browser.filter();
        assert_eq!(browser.visible.len(), 2);
    }

    #[test]
    fn active_author_query_updates_when_activity_arrives_and_ref_tip_changes() {
        let mut refs = owner_refs();
        let mut browser = Browser {
            query: "ALICE".into(),
            ..Browser::default()
        };
        browser.rebuild(Some(&refs));
        assert!(browser.visible.is_empty());
        let original = browser
            .entries
            .iter()
            .find(|e| e.target.name == "feature/ui")
            .unwrap()
            .target
            .clone();
        browser.install_activity(
            vec![(original, owner_activity("Alice Example"))],
            Some(&refs),
        );
        assert_eq!(browser.chosen().unwrap().target.name, "feature/ui");
        refs.refs[0].id = ObjectId::from_hex(b"3333333333333333333333333333333333333333").unwrap();
        browser.rebuild(Some(&refs));
        assert!(
            browser.visible.is_empty(),
            "previous tip's author must not leak into new tip"
        );
        let updated = browser
            .entries
            .iter()
            .find(|e| e.target.name == "feature/ui")
            .unwrap()
            .target
            .clone();
        browser.install_activity(vec![(updated, owner_activity("Bob Example"))], Some(&refs));
        assert!(browser.visible.is_empty());
        browser.query = "bOB".into();
        browser.filter();
        assert_eq!(
            browser.chosen().unwrap().activity.as_ref().unwrap().author,
            "Bob Example"
        );
    }

    #[test]
    fn author_filter_uses_displayed_shared_tip_activity_for_remote_entries() {
        let mut refs = owner_refs();
        let mut browser = Browser::default();
        browser.rebuild(Some(&refs));
        let target = browser
            .entries
            .iter()
            .find(|e| e.target.name == "feature/ui")
            .unwrap()
            .target
            .clone();
        browser.install_activity(vec![(target, owner_activity("Shared Author"))], Some(&refs));
        refs.refs.push(RefEntry {
            name: "origin/feature/ui".into(),
            id: refs.refs[0].id,
            kind: RefKind::Remote,
        });
        browser.query = "shared AUTHOR".into();
        browser.rebuild(Some(&refs));
        assert_eq!(browser.visible.len(), 1);
        let paired = browser.chosen().unwrap();
        assert_eq!(paired.target.name, "feature/ui");
        assert!(paired.target.remote.is_none());
        assert_eq!(
            paired
                .paired_remotes
                .iter()
                .map(Target::label)
                .collect::<Vec<_>>(),
            ["origin/feature/ui"]
        );
        assert!(
            browser
                .visible
                .iter()
                .all(|&i| browser.entries[i].activity.as_ref().unwrap().author == "Shared Author")
        );
        let primary = browser.chosen().unwrap().target.clone();
        browser.query = "origin/feature/ui".into();
        browser.filter();
        assert_eq!(browser.visible.len(), 1);
        assert_eq!(browser.chosen().unwrap().target, primary);
    }

    #[test]
    #[ignore = "cached branch-name and latest-author filtering performance"]
    fn benchmark_owner_filter() {
        let mut report = Vec::new();
        for count in [1000, 10000] {
            let refs = RefSnapshot {
                refs: (0..count)
                    .map(|n| RefEntry {
                        name: format!("feature/{n:05}"),
                        id: ObjectId::from_hex(format!("{:040x}", n + 1).as_bytes()).unwrap(),
                        kind: RefKind::Branch,
                    })
                    .collect(),
                ..RefSnapshot::default()
            };
            let mut browser = Browser::default();
            browser.rebuild(Some(&refs));
            let activity: Vec<_> = browser
                .entries
                .iter()
                .enumerate()
                .map(|(n, e)| {
                    (
                        e.target.clone(),
                        owner_activity(&format!("Owner {:02}", n % 100)),
                    )
                })
                .collect();
            browser.install_activity(activity.clone(), Some(&refs));
            let mut samples = Vec::new();
            for sample in 0..16 {
                let values = activity.clone();
                let started = std::time::Instant::now();
                browser.install_activity(values, Some(&refs));
                let install_ms = started.elapsed().as_secs_f64() * 1000.;
                let mut filters = Vec::new();
                let mut first_author_ms = 0.;
                for (query, expected) in [
                    ("FEATURE/", count),
                    ("OWNER 49", count / 100),
                    ("unknown owner", 0),
                    ("", count),
                ] {
                    browser.query = query.into();
                    if query == "OWNER 49" {
                        let started = std::time::Instant::now();
                        browser.filter();
                        first_author_ms = started.elapsed().as_secs_f64() * 1000.;
                        assert_eq!(browser.visible.len(), expected);
                    }
                    let started = std::time::Instant::now();
                    for _ in 0..100 {
                        browser.filter();
                        assert_eq!(std::hint::black_box(browser.visible.len()), expected);
                    }
                    filters.push(started.elapsed().as_secs_f64() * 1000. / 100.);
                }
                if sample > 0 {
                    samples.push(serde_json::json!({"sample": sample - 1, "installMs": install_ms,
                    "nameMs": filters[0], "firstAuthorMs": first_author_ms, "authorMs": filters[1], "missingMs": filters[2], "clearMs": filters[3]}));
                }
            }
            for mut sample in samples {
                sample["entries"] = count.into();
                report.push(sample);
            }
        }
        for sample in report {
            println!("{}", serde_json::to_string(&sample).unwrap());
        }
    }

    #[test]
    fn opening_stays_at_top_during_activity_updates_until_the_user_navigates() {
        let refs = RefSnapshot {
            refs: (1..=20)
                .map(|i| RefEntry {
                    name: format!("branch-{i:02}"),
                    id: ObjectId::from_hex(format!("{i:040x}").as_bytes()).unwrap(),
                    kind: RefKind::Branch,
                })
                .collect(),
            ..Default::default()
        };
        let mut browser = Browser {
            rows: 3,
            ..Default::default()
        };
        browser.show(Some(&refs));
        let scores = |entries: &[Entry], reverse: bool| {
            entries
                .iter()
                .map(|e| {
                    let rank = e.target.name[7..].parse::<u32>().unwrap();
                    (
                        e.target.clone(),
                        Activity {
                            author: "Author".into(),
                            author_time: 1000,
                            commit_time: 1000,
                            recent: if reverse { 21 - rank } else { rank },
                            limited: false,
                        },
                    )
                })
                .collect()
        };
        browser.install_activity(scores(&browser.entries, false), Some(&refs));
        assert_eq!(browser.chosen().unwrap().target.name, "branch-20");
        assert_eq!((browser.selected, browser.scroll), (0, 0));
        browser.move_by(4);
        let chosen = browser.chosen().unwrap().target.clone();
        browser.install_activity(scores(&browser.entries, true), Some(&refs));
        assert_eq!(browser.chosen().unwrap().target, chosen);
        assert!(browser.scroll > 0);
        browser.open = false;
        browser.show(Some(&refs));
        assert_eq!(browser.chosen().unwrap().target.name, "branch-01");
        assert_eq!((browser.selected, browser.scroll), (0, 0));
        browser.install_activity(scores(&browser.entries, false), Some(&refs));
        assert_eq!((browser.selected, browser.scroll), (0, 0));
        // Mouse clicks use select(), and must retain that branch during updates too.
        browser.select(5);
        let clicked = browser.chosen().unwrap().target.clone();
        browser.rebuild(Some(&refs));
        assert_eq!(browser.chosen().unwrap().target, clicked);
    }

    #[test]
    fn activity_orders_branches_and_preserves_selection_filter_and_rendering() {
        let id = |s: &str| ObjectId::from_hex(s.repeat(40).as_bytes()).unwrap();
        let refs = RefSnapshot {
            refs: vec![
                RefEntry {
                    name: "alphabetical".into(),
                    id: id("a"),
                    kind: RefKind::Branch,
                },
                RefEntry {
                    name: "busy".into(),
                    id: id("b"),
                    kind: RefKind::Branch,
                },
                RefEntry {
                    name: "latest".into(),
                    id: id("c"),
                    kind: RefKind::Branch,
                },
            ],
            ..Default::default()
        };
        let mut browser = Browser {
            rows: 10,
            ..Default::default()
        };
        browser.rebuild(Some(&refs));
        let chosen = browser.chosen().unwrap().target.clone();
        let values = browser
            .entries
            .iter()
            .map(|e| {
                (
                    e.target.clone(),
                    Activity {
                        author: "Graph Author With Long Name".into(),
                        author_time: 1000,
                        commit_time: if e.target.name == "latest" {
                            2000
                        } else {
                            1000
                        },
                        recent: if e.target.name == "alphabetical" {
                            1
                        } else {
                            4
                        },
                        limited: false,
                    },
                )
            })
            .collect();
        browser.install_activity(values, Some(&refs));
        assert_eq!(
            browser
                .entries
                .iter()
                .map(|e| e.target.name.as_str())
                .collect::<Vec<_>>(),
            ["latest", "busy", "alphabetical"]
        );
        assert_eq!(browser.chosen().unwrap().target, chosen);
        let mut screen = crate::term::Screen::new(120, 20);
        crate::ui::branches::draw(&mut browser, &mut screen, "test", 1000 + 3 * 3600);
        let text: String = screen.test_cells().iter().map(|c| c.ch).collect();
        assert!(text.contains("Graph Author With Long Name"));
        assert!(!text.contains("[local]"));
        assert!(!text.contains("Activity:"));
        assert!(text.contains("3h"));
        assert!(text.contains("4 /wk"));
        assert!(
            screen
                .test_cells()
                .iter()
                .any(|c| c.ch == 'G' && c.style.fg == crate::ui::theme::FG_AUTHOR)
        );
        assert!(
            screen
                .test_cells()
                .iter()
                .any(|c| c.ch == '3' && c.style.fg == crate::ui::theme::FG_DATE)
        );
        browser.query = "busy".into();
        browser.rebuild(Some(&refs));
        assert_eq!(browser.visible.len(), 1);
        assert_eq!(browser.chosen().unwrap().target.name, "busy");
        for w in [20, 49, 50, 69, 70] {
            crate::ui::branches::draw(
                &mut browser,
                &mut crate::term::Screen::new(w, 10),
                "test",
                1000,
            );
        }
        let remote = remote::Remote::parse("origin", "https://github.com/git/git", None).unwrap();
        for (cached, stale) in [(false, false), (true, false), (true, true)] {
            browser.update(
                remote::Update {
                    remote: remote.clone(),
                    branches: vec![remote::Branch {
                        name: "busy".into(),
                        sha: id("b").to_string(),
                    }],
                    scoped: false,
                    cached,
                    stale,
                },
                Some(&refs),
            );
            crate::ui::branches::draw(&mut browser, &mut screen, "test", 1000);
            let text: String = screen.test_cells().iter().map(|c| c.ch).collect();
            // The local primary row owns its exact same-tip remote alias. The
            // terminal renderer shows the primary label; native rows show both.
            assert!(text.contains("busy"));
            assert_eq!(browser.visible.len(), 1);
            let paired = browser.chosen().unwrap();
            assert!(paired.target.remote.is_none());
            assert_eq!(paired.target.name, "busy");
            assert_eq!(
                paired
                    .paired_remotes
                    .iter()
                    .map(Target::label)
                    .collect::<Vec<_>>(),
                ["origin/busy"]
            );
            assert!(!text.contains("cached") && !text.contains("stale") && !text.contains("live"));
        }
    }

    #[test]
    #[ignore = "explicit large-repository branch performance measurement"]
    fn benchmark_browser() {
        let path = std::path::PathBuf::from(
            std::env::var_os("GIT_RUSTLE_BENCH_REPO").expect("repository required"),
        );
        let repo = crate::git::open(&path).unwrap();
        let mut refs = crate::git::snapshot_refs(&repo).unwrap();
        if std::env::var("GIT_RUSTLE_BENCH_BRANCHES").as_deref() == Ok("1000") {
            let ids =
                crate::git::command::checked(&path, &["rev-list", "--all", "--max-count=1000"])
                    .unwrap();
            refs.refs = String::from_utf8(ids)
                .unwrap()
                .lines()
                .enumerate()
                .map(|(i, id)| RefEntry {
                    name: format!("synthetic-{i:04}"),
                    id: ObjectId::from_hex(id.as_bytes()).unwrap(),
                    kind: RefKind::Branch,
                })
                .collect();
            assert_eq!(refs.refs.len(), 1000);
        }
        let mut browser = Browser {
            rows: 34,
            ..Default::default()
        };
        browser.rebuild(Some(&refs));
        let tips = browser
            .entries
            .iter()
            .map(|e| e.target.clone())
            .collect::<Vec<_>>();
        let now = crate::ui::time::now_unix();
        let mut samples = Vec::new();
        for n in 0..16 {
            let start = std::time::Instant::now();
            let values = local_activity(&repo, &tips, now);
            let score_ms = start.elapsed().as_secs_f64() * 1000.0;
            let start = std::time::Instant::now();
            browser.install_activity(values, Some(&refs));
            let sort_ms = start.elapsed().as_secs_f64() * 1000.0;
            let mut screen = crate::term::Screen::new(120, 40);
            let start = std::time::Instant::now();
            for _ in 0..200 {
                browser.move_by(1);
                if browser.selected + 1 == browser.visible.len() {
                    browser.move_by(-(browser.visible.len() as isize));
                }
                crate::ui::branches::draw(&mut browser, &mut screen, "benchmark", now);
                std::hint::black_box(screen.test_cells());
            }
            let scroll_ms = start.elapsed().as_secs_f64() * 1000.0 / 200.0;
            if n > 0 {
                samples.push(serde_json::json!({"sample":n-1,"activityMs":score_ms,"installSortMs":sort_ms,"scrollDrawMs":scroll_ms}));
            }
        }
        println!(
            "{} branch tips; one warm-up, 15 samples: {}",
            tips.len(),
            serde_json::to_string(&samples).unwrap()
        );
    }

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

#[cfg(test)]
mod hierarchy_tests {
    use super::*;
    use crate::git::RefEntry;
    fn refs() -> RefSnapshot {
        let id = ObjectId::from_hex(b"1111111111111111111111111111111111111111").unwrap();
        RefSnapshot {
            refs: [
                ("fixes/task-123", RefKind::Branch),
                ("fixes/other", RefKind::Branch),
                ("topic", RefKind::Branch),
                ("origin/renamed", RefKind::Remote),
            ]
            .into_iter()
            .map(|(name, kind)| RefEntry {
                name: name.into(),
                id,
                kind,
            })
            .collect(),
            upstreams: BTreeMap::from([(
                "fixes/task-123".into(),
                ("origin".into(), "renamed".into()),
            )]),
            ..RefSnapshot::default()
        }
    }
    #[test]
    fn fetched_remote_icons_require_exact_cached_tip_not_tracking_configuration() {
        let mut refs = refs();
        refs.upstreams
            .insert("topic".into(), ("origin".into(), "server-only".into()));
        let mut browser = Browser::default();
        browser.rebuild(Some(&refs));
        let mut target = Target {
            name: "renamed".into(),
            remote: Some("origin".into()),
            url: None,
            sha: "1111111111111111111111111111111111111111".into(),
        };
        assert!(browser.remote_fetched(&target));
        target.sha = "2222222222222222222222222222222222222222".into();
        assert!(
            !browser.remote_fetched(&target),
            "An advanced server tip has not been fetched"
        );
        target.name = "server-only".into();
        target.sha = "1111111111111111111111111111111111111111".into();
        assert!(
            !browser.remote_fetched(&target),
            "A configured local upstream does not create a cached remote ref"
        );
        target.name = "renamed".into();
        browser.rebuild(None);
        assert!(
            !browser.remote_fetched(&target),
            "Rebuild must not retain removed snapshot refs"
        );
    }
    #[test]
    fn exact_upstream_same_tip_coalesces_but_distinct_branches_stay_visible() {
        let refs = refs();
        let mut browser = Browser::default();
        browser.rebuild(Some(&refs));
        assert_eq!(browser.entries.len(), 3);
        let local = browser
            .entries
            .iter()
            .find(|e| e.target.name == "fixes/task-123")
            .unwrap();
        assert!(local.target.remote.is_none());
        assert_eq!(local.paired_remotes.len(), 1);
        browser.query = "origin/renamed".into();
        browser.filter();
        assert_eq!(browser.visible.len(), 1);
        assert_eq!(browser.chosen().unwrap().target.name, "fixes/task-123");
    }
    #[test]
    fn paired_alias_is_pointer_sized_and_multiple_tracking_locals_prefer_current() {
        assert_eq!(
            std::mem::size_of::<PairedRemote>(),
            std::mem::size_of::<usize>()
        );
        assert!(PairedRemote::default().is_empty());
        let mut refs = refs();
        refs.upstreams
            .insert("topic".into(), ("origin".into(), "renamed".into()));
        refs.head_branch = Some("topic".into());
        refs.head_id = Some(refs.refs[0].id);
        let mut browser = Browser::default();
        browser.rebuild(Some(&refs));
        let paired: Vec<_> = browser
            .entries
            .iter()
            .filter(|e| !e.paired_remotes.is_empty())
            .collect();
        assert_eq!(paired.len(), 1);
        assert_eq!(paired[0].target.name, "topic");
        assert!(paired[0].current);
        assert_eq!(paired[0].paired_remotes[0].label(), "origin/renamed");
        assert!(
            browser
                .entries
                .iter()
                .any(|e| e.target.name == "fixes/task-123" && e.paired_remotes.is_empty())
        );
    }
    #[test]
    fn configured_upstream_pairs_once_and_competing_remote_stays_independent() {
        let mut refs = refs();
        refs.refs.push(RefEntry {
            name: "other/renamed".into(),
            id: refs.refs[0].id,
            kind: RefKind::Remote,
        });
        let mut browser = Browser::default();
        browser.rebuild(Some(&refs));
        let local = browser
            .entries
            .iter()
            .find(|e| e.target.name == "fixes/task-123")
            .unwrap();
        assert_eq!(local.paired_remotes.len(), 1);
        assert_eq!(local.paired_remotes[0].label(), "origin/renamed");
        assert!(
            browser
                .entries
                .iter()
                .any(|e| e.target.label() == "other/renamed" && e.paired_remotes.is_empty())
        );
    }
    #[test]
    fn shared_upstream_noncurrent_owner_is_lexical_regardless_of_ref_order() {
        let mut refs = refs();
        refs.upstreams
            .insert("topic".into(), ("origin".into(), "renamed".into()));
        let mut browser = Browser::default();
        for _ in 0..2 {
            browser.rebuild(Some(&refs));
            let paired: Vec<_> = browser
                .entries
                .iter()
                .filter(|e| !e.paired_remotes.is_empty())
                .collect();
            assert_eq!(paired.len(), 1);
            assert_eq!(paired[0].target.name, "fixes/task-123");
            assert!(!paired[0].current);
            refs.refs.reverse();
        }
    }
    #[test]
    fn changed_upstream_tip_and_ambiguous_fallback_are_not_coalesced() {
        let mut refs = refs();
        refs.refs.last_mut().unwrap().id =
            ObjectId::from_hex(b"2222222222222222222222222222222222222222").unwrap();
        let mut browser = Browser::default();
        browser.rebuild(Some(&refs));
        assert_eq!(browser.entries.len(), 4);
        refs.upstreams.clear();
        refs.refs.last_mut().unwrap().name = "origin/topic".into();
        refs.refs.last_mut().unwrap().id = refs.refs[0].id;
        refs.refs.push(RefEntry {
            name: "other/topic".into(),
            id: refs.refs[0].id,
            kind: RefKind::Remote,
        });
        browser.rebuild(Some(&refs));
        assert_eq!(browser.entries.len(), 5);
    }
    #[test]
    fn hierarchy_collapses_keyboard_rows_and_search_reveals_matching_children() {
        let mut browser = Browser::default();
        browser.rebuild(Some(&refs()));
        browser.set_hierarchy(true);
        assert_eq!(browser.visible.len(), 3);
        assert!(
            browser
                .tree_rows
                .iter()
                .any(|r| matches!(r,TreeRow::Folder{path,..} if path=="fixes"))
        );
        browser.toggle_folder("fixes".into());
        assert_eq!(browser.visible.len(), 1);
        browser.query = "task-123".into();
        browser.filter();
        assert_eq!(browser.visible.len(), 1);
        assert_eq!(browser.chosen().unwrap().target.name, "fixes/task-123");
        browser.query.clear();
        browser.filter();
        assert_eq!(browser.visible.len(), 1);
        browser.set_hierarchy(false);
        assert_eq!(browser.visible.len(), 3);
    }
    #[test]
    #[ignore = "one warmup and fifteen cached branch projection samples per size"]
    fn benchmark_branch_projection() {
        for entries in [1000, 10000] {
            let id = ObjectId::from_hex(b"1111111111111111111111111111111111111111").unwrap();
            let mut refs = RefSnapshot::default();
            for i in 0..entries {
                let name = format!("team-{}/task-{i}", i % 20);
                refs.refs.push(RefEntry {
                    name: name.clone(),
                    id,
                    kind: RefKind::Branch,
                });
                refs.refs.push(RefEntry {
                    name: format!("origin/{name}"),
                    id,
                    kind: RefKind::Remote,
                });
                refs.upstreams.insert(name.clone(), ("origin".into(), name));
            }
            for sample in -1..15 {
                let mut browser = Browser::default();
                let start = std::time::Instant::now();
                browser.rebuild(Some(&refs));
                let merge_ms = start.elapsed().as_secs_f64() * 1000.;
                assert_eq!(browser.entries.len(), entries);
                let start = std::time::Instant::now();
                browser.set_hierarchy(true);
                let hierarchy_ms = start.elapsed().as_secs_f64() * 1000.;
                let start = std::time::Instant::now();
                browser.toggle_folder("team-0".into());
                let collapse_ms = start.elapsed().as_secs_f64() * 1000.;
                println!(
                    "{}",
                    serde_json::json!({"sample":sample,"entries":entries,"mergeMs":merge_ms,"hierarchyMs":hierarchy_ms,"collapseMs":collapse_ms})
                );
            }
        }
    }
}
