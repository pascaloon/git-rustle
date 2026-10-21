#![allow(dead_code)] // Legacy adapters remain as regression-test references.
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
    /// Provider tip that differs from the locally stored remote-tracking ref.
    pub remote_only: bool,
}

pub struct App {
    pub search: crate::search::Search,
    pub pending: crate::pending::Pending,
    pub local_history: Option<std::sync::Arc<History>>,
    pub ghosts: HashMap<ObjectId, CommitDetails>,
    pub ghost_sources: HashMap<ObjectId, crate::remote::Remote>,
    pub ghost_branches: Vec<(ObjectId, String)>,
    pub ghost_tags: Vec<(ObjectId, String)>,
    pub ghost_status: String,
    pub settings: crate::settings::Settings,
    pub branches: crate::branches::Browser,
    pub checkout_confirmation: Option<crate::checkout::Confirmation>,
    pub checkout_request: Option<crate::checkout::Request>,
    pub checkout_notice: Option<Result<String, String>>,
    pub repo_name: String,
    pub history: History,
    /// A fresh history being built while `history` (stale cache) stays on screen.
    pub incoming: Option<History>,
    pub refs: Option<RefSnapshot>,
    pub labels: HashMap<ObjectId, Vec<Label>>,
    labels_revision: u64,
    provider_search_labels: HashMap<ObjectId, Vec<String>>,
    pub load: LoadState,
    /// Next row to inspect while waiting for the initial HEAD selection.
    startup_head_search: Option<usize>,

    pub selected: usize,
    pub scroll: usize,
    pub focus: Focus,
    pub details: Option<CommitDetails>,
    /// A workflow inspector can inspect a commit outside the graph's loaded range.
    inspection: Option<(ObjectId, Option<ObjectId>)>,
    pub details_scroll: usize,
    pub details_lines: usize,

    /// Width of the details panel in cells.
    pub right_width: u16,
    pub dragging_splitter: bool,

    /// Commits marked for diffing, in marking order (at most `MAX_MARKS`).
    pub marks: Vec<ObjectId>,
    pub mode: Mode,
    pub parked_diff: Option<Box<DiffView>>,
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
    pub want_refresh: bool,
    pub needs_redraw: bool,
    /// Rows visible in the list, updated by the renderer.
    pub list_rows: usize,
}

impl App {
    pub fn new() -> Self {
        App {
            search: crate::search::Search::default(),
            pending: crate::pending::Pending::default(),
            local_history: None,
            ghosts: HashMap::new(),
            ghost_sources: HashMap::new(),
            ghost_branches: Vec::new(),
            ghost_tags: Vec::new(),
            ghost_status: String::new(),
            settings: crate::settings::Settings::default(),
            branches: crate::branches::Browser::default(),
            checkout_confirmation: None,
            checkout_request: None,
            checkout_notice: None,
            repo_name: String::new(),
            history: History::default(),
            incoming: None,
            refs: None,
            labels: HashMap::new(),
            labels_revision: 0,
            provider_search_labels: HashMap::new(),
            load: LoadState::Empty,
            startup_head_search: Some(0),
            selected: 0,
            scroll: 0,
            focus: Focus::List,
            details: None,
            inspection: None,
            details_scroll: 0,
            details_lines: 0,
            right_width: 0,
            dragging_splitter: false,
            marks: Vec::new(),
            mode: Mode::Graph,
            parked_diff: None,
            sidebar_width: 32,
            next_view_id: 1,
            details_changes: None,
            details_cursor: TreeCursor::default(),
            files_area: Rect::default(),
            next_key: 1,
            quit: false,
            want_rewalk: false,
            want_refresh: false,
            needs_redraw: true,
            list_rows: 1,
        }
    }

    pub fn selected_id(&self) -> Option<ObjectId> {
        if self.pending.selected || (self.search.active() && self.graph_len() == 0) {
            return None;
        }
        self.history.commits.get(self.selected).map(|c| c.id)
    }

    pub fn inspected_id(&self) -> Option<ObjectId> {
        self.inspection.map(|(id, _)| id)
    }

    pub fn details_id(&self) -> Option<ObjectId> {
        self.inspected_id().or_else(|| self.selected_id())
    }

    pub fn inspect_commit(&mut self, target: Option<(ObjectId, Option<ObjectId>)>) {
        if self.inspection == target {
            return;
        }
        self.inspection = target;
        self.details = None;
        self.details_changes = None;
        self.sync_details_changes();
        self.needs_redraw = true;
    }

    pub fn clear_search(&mut self) {
        self.search.text.value.clear();
        self.search.text.cursor = 0;
        self.search.editing = false;
        self.search.changed();
        self.ensure_visible();
        self.needs_redraw = true;
    }

    pub fn labels_revision(&self) -> u64 {
        self.labels_revision
    }

    /// Provider badges are search metadata, never synthetic Git references.
    pub fn set_provider_search_labels(
        &mut self,
        mut labels: HashMap<ObjectId, Vec<String>>,
    ) -> bool {
        labels.retain(|_, values| {
            values.retain(|value| !value.is_empty());
            values.sort_unstable();
            values.dedup();
            !values.is_empty()
        });
        if self.provider_search_labels == labels {
            return false;
        }
        self.provider_search_labels = labels;
        self.search.invalidate();
        self.needs_redraw = true;
        true
    }

    pub fn search_request(&mut self) -> Option<crate::search::Request> {
        if !self.search.wanted {
            return None;
        }
        self.search.wanted = false;
        if self.pending.selected {
            self.remember_pending_checks();
            self.pending.selected = false;
        }
        let input = self.search.input.get_or_insert_with(|| {
            let mut labels: HashMap<ObjectId, Vec<String>> = self
                .labels
                .iter()
                .map(|(id, labels)| (*id, labels.iter().map(|l| l.name.clone()).collect()))
                .collect();
            for (id, values) in &self.provider_search_labels {
                labels
                    .entry(*id)
                    .or_default()
                    .extend(values.iter().cloned());
            }
            std::sync::Arc::new(crate::search::Input {
                history: self.history.clone(),
                labels,
                ghosts: self.ghosts.clone(),
                sources: self.ghost_sources.clone(),
            })
        });
        Some(crate::search::Request {
            generation: self.search.generation,
            text: self.search.text.value.clone(),
            input: input.clone(),
        })
    }

    pub fn install_search(&mut self, result: crate::search::Matches) -> bool {
        if result.generation != self.search.generation || !self.search.active() {
            return false;
        }
        let first = self.search.result.is_none();
        self.search.busy = !result.done;
        let keep = result
            .positions
            .get(self.selected)
            .is_some_and(|&p| p != usize::MAX);
        self.search.result = Some(result);
        if !keep {
            self.select_graph(0);
        }
        self.clamp();
        if first {
            self.ensure_visible();
        }
        self.needs_redraw = true;
        true
    }

    pub fn graph_len(&self) -> usize {
        if self.search.active() {
            return self.search.result.as_ref().map_or(0, |r| r.rows.len());
        }
        self.history.len() + usize::from(self.pending.exists())
    }
    pub fn graph_selected(&self) -> usize {
        if self.search.active() {
            return self
                .search
                .result
                .as_ref()
                .and_then(|r| r.positions.get(self.selected))
                .copied()
                .filter(|&p| p != usize::MAX)
                .unwrap_or(0);
        }
        if self.pending.selected {
            self.pending.row()
        } else {
            self.selected
                + usize::from(self.pending.exists() && self.selected >= self.pending.row())
        }
    }
    pub fn history_index(&self, row: usize) -> Option<usize> {
        if self.search.active() {
            return self.search.result.as_ref()?.rows.get(row).copied();
        }
        if !self.pending.exists() {
            Some(row)
        } else if row == self.pending.row() {
            None
        } else {
            Some(row - usize::from(row > self.pending.row()))
        }
    }
    /// User navigation cancels deferred startup focus, including late status results.
    pub fn begin_navigation(&mut self) {
        self.startup_head_search = None;
        self.pending.autofocus = false;
    }

    /// Focus pending changes when present, otherwise follow HEAD as history arrives.
    pub fn go_to_head(&mut self) {
        self.begin_navigation();
        self.settings.open = false;
        self.branches.open = false;
        self.park_diff();
        self.clear_search();
        self.pending.autofocus = !self.pending.known;
        self.startup_head_search = Some(0);
        self.focus_startup_head(true);
        self.focus = Focus::List;
        self.needs_redraw = true;
    }
    pub fn select_graph(&mut self, row: usize) {
        if self.graph_len() == 0 {
            return;
        }
        let row = row.min(self.graph_len().saturating_sub(1));
        if let Some(index) = self.history_index(row) {
            self.select(index);
        } else {
            self.pending.selected = true;
            self.details = None;
            self.ensure_visible();
            self.needs_redraw = true;
        }
    }
    fn update_pending_row(&mut self, replaced: bool) {
        if let Some(id) = self.pending.return_to_head
            && let Some(index) = self.history.index_of(&id)
        {
            self.pending.return_to_head = None;
            self.select(index);
        }
        if !self.pending.exists() {
            return;
        }
        let previous_row = self.pending.row();
        let offset = previous_row.saturating_sub(self.scroll);
        let was_visible =
            previous_row >= self.scroll && previous_row < self.scroll + self.list_rows.max(1);
        if replaced {
            self.pending.head_index = None;
            self.pending.searched = 0;
        }
        if self.pending.head_index.is_none() {
            let start = self.pending.searched.min(self.history.len());
            if let Some(head) = self.head_id() {
                self.pending.head_index = self.history.commits[start..]
                    .iter()
                    .position(|c| c.id == head)
                    .map(|i| start + i);
            }
            self.pending.searched = self.history.len();
        }
        if self.pending.selected && previous_row != self.pending.row() && was_visible {
            self.scroll = self.pending.row().saturating_sub(offset);
        }
    }
    pub fn install_pending(&mut self, result: Result<crate::pending::Snapshot, String>) {
        if self.pending.known && result.as_ref().is_ok_and(|s| *s == self.pending.snapshot) {
            self.pending.loading = false;
            return;
        }
        self.pending.known = true;
        self.pending.loading = false;
        match result {
            Ok(snapshot) => {
                self.pending.snapshot = snapshot;
                let paths: std::collections::HashSet<_> = self
                    .pending
                    .snapshot
                    .files
                    .iter()
                    .map(|f| f.path.as_str())
                    .collect();
                self.pending
                    .checks
                    .retain(|path, _| paths.contains(path.as_str()));
                if !self.pending.exists() && self.pending.selected {
                    self.pending.selected = false;
                    self.pending.return_to_head = self.pending.snapshot.head;
                }
                if self.pending.selected && matches!(self.mode, Mode::Graph) {
                    self.rebuild_pending_changes();
                }
                self.update_pending_row(true);
                if self.pending.autofocus {
                    self.pending.autofocus = false;
                    if self.pending.exists() {
                        self.startup_head_search = None;
                        self.pending.selected = true;
                        self.ensure_visible();
                    }
                }
                self.focus_startup_head(true);
            }
            Err(error) => {
                self.pending.autofocus = false;
                self.pending.status = format!("Cannot read pending changes: {error}");
                self.pending.error = true;
                self.focus_startup_head(true);
            }
        }
        self.clamp();
        self.needs_redraw = true;
    }
    fn rebuild_pending_changes(&mut self) {
        let mut previous = self.pending.checks.clone();
        previous.extend(
            self.details_changes
                .as_ref()
                .filter(|cs| cs.checked.is_some())
                .map(|cs| {
                    cs.files
                        .iter()
                        .enumerate()
                        .map(|(i, f)| (f.path.clone(), cs.checked.as_ref().unwrap()[i]))
                        .collect::<HashMap<_, _>>()
                })
                .unwrap_or_default(),
        );
        let mut changes = ChangeSet::new(
            self.new_key(),
            self.pending.snapshot.head,
            ObjectId::null(gix::hash::Kind::Sha1),
        );
        changes.requested = true;
        changes.set_tree(Ok(crate::diff::TreeDiff {
            files: self.pending.snapshot.files.clone(),
        }));
        changes.enable_checks();
        for (i, file) in changes.files.iter().enumerate() {
            if let Some(checked) = previous.get(&file.path) {
                changes.checked.as_mut().unwrap()[i] = *checked;
            }
        }
        changes.recompute_checks();
        self.pending.checks = changes
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.path.clone(), changes.checked.as_ref().unwrap()[i]))
            .collect();
        self.details_changes = Some(changes);
        self.details_cursor
            .clamp(&self.details_changes.as_ref().unwrap().tree);
    }

    fn remember_pending_checks(&mut self) {
        if let Some(cs) = &self.details_changes
            && let Some(checks) = &cs.checked
        {
            self.pending.checks = cs
                .files
                .iter()
                .enumerate()
                .map(|(i, f)| (f.path.clone(), checks[i]))
                .collect();
        }
    }
    pub fn request_commit(&mut self) {
        if self.pending.busy
            || self.pending.request.is_some()
            || self.branches.busy
            || self.checkout_request.is_some()
        {
            return;
        }
        self.sync_details_changes();
        let error = if self.pending.description.value.trim().is_empty() {
            Some("Enter a commit description")
        } else if self.pending.snapshot.conflicts {
            Some("Resolve file conflicts before committing")
        } else if self
            .details_changes
            .as_ref()
            .is_none_or(|cs| cs.checked_count() == 0)
        {
            Some("Check at least one file to commit")
        } else {
            None
        };
        if let Some(error) = error {
            self.pending.status = error.into();
            self.pending.error = true;
            self.needs_redraw = true;
            return;
        }
        let cs = self.details_changes.as_ref().unwrap();
        let files = cs
            .files
            .iter()
            .zip(cs.checked.as_ref().unwrap())
            .filter(|(_, yes)| **yes)
            .map(|(f, _)| f.clone())
            .collect();
        self.pending.request = Some(crate::pending::Request {
            head: self.pending.snapshot.head,
            branch: self.pending.snapshot.branch.clone(),
            new_branch: self.pending.branch.value.clone(),
            description: self.pending.description.value.clone(),
            files,
        });
        self.needs_redraw = true;
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
        if let Some((inspected, parent)) = self.inspection
            && inspected == *id
        {
            return parent;
        }
        let i = self.history.index_of(id)?;
        self.history.parents(i).first().copied()
    }

    fn diff_source(
        &self,
        old: Option<ObjectId>,
        new: ObjectId,
    ) -> Option<Box<crate::diff::changes::RemoteSource>> {
        let new_source = self.ghost_sources.get(&new);
        let old_source = old.as_ref().and_then(|id| self.ghost_sources.get(id));
        Some(Box::new(crate::diff::changes::RemoteSource {
            remote: new_source.or(old_source)?.clone(),
            old_remote: old_source.cloned(),
            compare: new_source.is_none() || old != self.parent_of(&new),
        }))
    }

    fn new_changes(&mut self, old: Option<ObjectId>, new: ObjectId) -> ChangeSet {
        let mut changes = ChangeSet::new(self.new_key(), old, new);
        changes.remote_source = self.diff_source(old, new);
        changes
    }

    fn refresh_diff_sources(&mut self) {
        let target = |cs: &ChangeSet| (cs.key, cs.old, cs.new);
        let targets = [
            self.details_changes.as_ref().map(target),
            match &self.mode {
                Mode::Diff(view) => Some(target(&view.changes)),
                Mode::Graph => None,
            },
            self.parked_diff.as_ref().map(|view| target(&view.changes)),
        ];
        for (key, old, new) in targets.into_iter().flatten() {
            let Some(source) = self.diff_source(old, new) else {
                // An open tab must outlive the graph's temporary local-only state.
                continue;
            };
            let changes = self.changes_by_key_mut(key).unwrap();
            if changes.remote_source.as_ref() == Some(&source) {
                continue;
            }
            changes.remote_source = Some(source);
            if changes.requested && (!changes.loaded || changes.error.is_some()) {
                // Ignore any late local-worker result and retry through the API.
                let next_key = self.new_key();
                let changes = self.changes_by_key_mut(key).unwrap();
                changes.key = next_key;
                changes.requested = false;
                changes.loaded = false;
                changes.error = None;
            }
        }
    }

    /// Keep `details_changes` pointed at the selected commit.
    pub fn sync_details_changes(&mut self) {
        if self.inspection.is_none() && self.pending.selected {
            if matches!(self.mode, Mode::Diff(_)) {
                return;
            }
            if self
                .details_changes
                .as_ref()
                .is_none_or(|cs| cs.checked.is_none())
            {
                self.rebuild_pending_changes();
            }
            return;
        }
        let Some(id) = self.details_id() else { return };
        if self.details_changes.as_ref().map(|c| c.new) == Some(id) {
            return;
        }
        let parent = self.parent_of(&id);
        self.details_changes = Some(self.new_changes(parent, id));
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
        // A hidden Diff tab still accepts results and finishes its current request.
        let mut requests = self
            .parked_diff
            .as_mut()
            .map_or_else(Vec::new, |v| v.pending());
        if self.inspection.is_none()
            && matches!(self.mode, Mode::Graph)
            && self.search.active()
            && self.graph_len() == 0
        {
            return requests;
        }
        requests.extend(match &mut self.mode {
            Mode::Diff(view) => view.pending(),
            Mode::Graph => match self.details_changes.as_mut() {
                Some(cs) if !cs.requested => {
                    cs.requested = true;
                    vec![DiffReq::tree(cs)]
                }
                _ => Vec::new(),
            },
        });
        requests
    }

    /// The change set currently on screen (view or details panel).
    pub fn active_changes_mut(&mut self) -> Option<&mut ChangeSet> {
        if self.inspection.is_none()
            && matches!(self.mode, Mode::Graph)
            && self.search.active()
            && self.graph_len() == 0
        {
            return None;
        }
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
        if let Some(view) = &mut self.parked_diff
            && view.changes.key == key
        {
            return Some(&mut view.changes);
        }
        self.details_changes.as_mut().filter(|c| c.key == key)
    }

    /// `d`: diff the two marked commits (older on the left), or with fewer
    /// marks the marked/selected commit against its parent.
    pub fn open_diff(&mut self) {
        if let Some(id) = self.inspected_id() {
            self.open_parent_diff(id);
            return;
        }
        if self.pending.selected {
            self.sync_details_changes();
            self.remember_pending_checks();
            if let Some(changes) = self.details_changes.take() {
                self.show_view(changes, Some(self.details_cursor));
            }
            return;
        }
        let ids = if self.marks.is_empty() {
            self.selected_id().into_iter().collect()
        } else {
            self.marks.clone()
        };
        self.open_selected_diff(&ids);
    }

    /// Compare one commit with its parent, or exactly two selected commits.
    pub fn open_selected_diff(&mut self, ids: &[ObjectId]) {
        if ids.len() == 2 {
            let (a, b) = (ids[0], ids[1]);
            let time = |id: &ObjectId| {
                self.history
                    .index_of(id)
                    .map(|i| (self.history.commits[i].commit_time, std::cmp::Reverse(i)))
            };
            let (old, new) = match (time(&a), time(&b)) {
                (Some(ta), Some(tb)) if tb < ta => (b, a),
                _ => (a, b),
            };
            let changes = self.new_changes(Some(old), new);
            self.show_view(changes, None);
        } else if let [id] = ids {
            self.open_parent_diff(*id);
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
            let parent = self.parent_of(&id);
            let changes = self.new_changes(parent, id);
            self.show_view(changes, None);
        }
    }

    pub fn open_commit_pair(&mut self, old: ObjectId, new: ObjectId) {
        let changes = self.new_changes(Some(old), new);
        self.show_view(changes, None);
    }

    fn show_view(&mut self, changes: ChangeSet, cursor: Option<TreeCursor>) {
        self.parked_diff = None;
        let id = self.next_view_id;
        self.next_view_id += 1;
        self.mode = Mode::Diff(Box::new(DiffView::new(
            id,
            changes,
            cursor,
            self.sidebar_width,
        )));
        self.needs_redraw = true;
    }

    pub fn close_diff(&mut self) {
        if let Mode::Diff(view) = std::mem::replace(&mut self.mode, Mode::Graph) {
            self.sidebar_width = view.sidebar_width;
            // Hand a parent diff of the selected commit back to the details panel.
            let selected = self.selected_id();
            if (self.pending.selected && view.changes.checked.is_some())
                || (Some(view.changes.new) == selected
                    && view.changes.old == selected.and_then(|s| self.parent_of(&s)))
            {
                self.details_changes = Some(view.changes);
                self.details_cursor = view.cursor;
            }
        }
        self.needs_redraw = true;
    }

    pub fn diff_view_mut(&mut self) -> Option<&mut DiffView> {
        match &mut self.mode {
            Mode::Diff(v) => Some(v),
            Mode::Graph => self.parked_diff.as_deref_mut(),
        }
    }

    pub fn park_diff(&mut self) {
        if matches!(self.mode, Mode::Diff(_)) {
            let Mode::Diff(view) = std::mem::replace(&mut self.mode, Mode::Graph) else {
                unreachable!()
            };
            self.parked_diff = Some(view);
        }
    }

    pub fn resume_diff(&mut self) {
        if let Some(view) = self.parked_diff.take() {
            self.mode = Mode::Diff(view);
        }
    }

    fn files_tree_available(&self) -> bool {
        self.details_changes
            .as_ref()
            .is_some_and(|c| c.loaded && !c.files.is_empty())
    }

    fn cycle_focus(&mut self) {
        if self.pending.selected {
            match self.focus {
                Focus::List => {
                    self.focus = Focus::Details;
                    self.pending.field = 0;
                }
                Focus::Details if self.pending.field < 4 => self.pending.field += 1,
                Focus::Details => self.focus = Focus::Files,
                Focus::Files => self.focus = Focus::List,
            }
            self.needs_redraw = true;
            return;
        }
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

    fn focus_startup_head(&mut self, replaced: bool) {
        let Some(searched) = self.startup_head_search else {
            return;
        };
        if self.pending.exists() {
            self.startup_head_search = None;
            self.pending.selected = true;
            self.ensure_visible();
            self.needs_redraw = true;
            return;
        }
        let Some(refs) = self.refs.as_ref() else {
            return;
        };
        let Some(head) = refs.head_id else {
            self.startup_head_search = None;
            return;
        };
        let start = if replaced {
            0
        } else {
            searched.min(self.history.len())
        };
        if let Some(offset) = self.history.commits[start..]
            .iter()
            .position(|commit| commit.id == head)
        {
            self.startup_head_search = None;
            self.select(start + offset);
        } else {
            self.startup_head_search = Some(self.history.len());
        }
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
                        remote_only: false,
                    })
                    .collect(),
            );
        }
        if let Some(id) = refs.head_id {
            let head_labels = labels.entry(id).or_default();
            // Keep HEAD explicit and first even when the checked-out branch has
            // a long name. Detached snapshots already contain this label.
            if !head_labels
                .iter()
                .any(|label| label.kind == RefKind::Head && label.name == "HEAD")
            {
                head_labels.insert(
                    0,
                    Label {
                        name: "HEAD".into(),
                        kind: RefKind::Head,
                        remote_only: false,
                    },
                );
            }
        }
        self.labels = labels;
        self.refs = Some(refs);
        if self.branches.open || !self.branches.entries.is_empty() {
            self.branches.rebuild(self.refs.as_ref());
        }
        self.remote_labels();
        self.update_pending_row(true);
        self.focus_startup_head(true);
        self.needs_redraw = true;
    }

    pub fn remote_labels(&mut self) {
        self.search.invalidate();
        self.labels_revision = self.labels_revision.wrapping_add(1);
        // Replace only API overlays. Fetched remote-tracking labels stay on their
        // actual local objects, even when a provider reports a different tip.
        self.labels.retain(|_, labels| {
            labels.retain(|label| !label.remote_only);
            !labels.is_empty()
        });
        for (name, update) in &self.branches.remotes {
            for branch in &update.branches {
                if let Ok(id) = ObjectId::from_hex(branch.sha.as_bytes()) {
                    let label_name = format!("{name}/{}", branch.name);
                    if self.ghosts.contains_key(&id)
                        && !self
                            .ghost_branches
                            .iter()
                            .any(|(tip, name)| *tip == id && *name == label_name)
                    {
                        continue;
                    }
                    let labels = self.labels.entry(id).or_default();
                    // When fetched and live tips agree, the real ref badge suffices.
                    if labels
                        .iter()
                        .any(|label| label.kind == RefKind::Remote && label.name == label_name)
                    {
                        continue;
                    }
                    labels.push(Label {
                        name: label_name,
                        kind: RefKind::Remote,
                        remote_only: true,
                    });
                }
            }
        }
        for (id, name) in &self.ghost_tags {
            let labels = self.labels.entry(*id).or_default();
            if !labels
                .iter()
                .any(|l| l.name == *name && l.kind == RefKind::Tag)
            {
                labels.push(Label {
                    name: name.clone(),
                    kind: RefKind::Tag,
                    remote_only: false,
                });
            }
        }
        self.needs_redraw = true;
    }

    pub fn install_preview(&mut self, preview: crate::remote::history::Preview) {
        let startup_pending = self.startup_head_search.is_some();
        let keep = (!self.pending.selected)
            .then(|| self.history.commits.get(self.selected).map(|c| c.id))
            .flatten();
        let offset = self.graph_selected().saturating_sub(self.scroll);
        let was_top = self.scroll == 0;
        if let Some(history) = preview.history {
            self.history = history;
        } else if let Some(local) = &self.local_history
            && (!self.ghosts.is_empty() || self.history.len() != local.len())
        {
            self.history = (**local).clone();
        }
        self.ghosts = preview.ghosts;
        self.ghost_sources = preview.sources;
        self.refresh_diff_sources();
        self.ghost_branches = preview.branches;
        self.ghost_tags = preview.tags;
        self.ghost_status = if preview.errors.is_empty() {
            if self.ghosts.is_empty() {
                String::new()
            } else {
                format!("{} ghost commits (remote only)", self.ghosts.len())
            }
        } else {
            format!("Remote preview incomplete: {}", preview.errors.join(" | "))
        };
        self.details = None;
        if let Some(refs) = self.refs.clone() {
            self.set_refs(refs);
        }
        if let Some(id) = keep
            && (!startup_pending || self.startup_head_search.is_some())
            && let Some(i) = self.history.index_of(&id)
        {
            self.selected = i;
            let row = self.graph_selected();
            self.scroll = if was_top && row < self.list_rows {
                0
            } else {
                row.saturating_sub(offset)
            };
        }
        self.clamp();
        self.needs_redraw = true;
    }

    pub fn finish_branch_jump(&mut self) {
        if let Some(id) = self.branches.pending_jump.take() {
            if let Some(index) = self.history.index_of(&id) {
                self.clear_search();
                self.mode = Mode::Graph;
                self.focus = Focus::List;
                self.select(index);
                self.branches.open = false;
                self.branches.status.clear();
            } else {
                self.branches.status =
                    format!("Could not locate remote tip. {}", self.ghost_status);
            }
        }
    }

    /// Install a history read from the cache.
    pub fn set_cached(&mut self, history: History, fresh: bool) {
        self.search.invalidate();
        self.local_history = None;
        self.ghosts.clear();
        self.ghost_sources.clear();
        self.ghost_branches.clear();
        self.ghost_tags.clear();
        self.history = history;
        self.load = if fresh {
            LoadState::Cached
        } else {
            LoadState::Refreshing
        };
        self.clamp();
        self.update_pending_row(true);
        self.focus_startup_head(true);
        self.needs_redraw = true;
    }

    /// A batch arrived from the walker.
    pub fn push_batch(&mut self, batch: History) {
        if batch.complete {
            self.local_history = None;
            self.ghosts.clear();
            self.ghost_sources.clear();
            self.ghost_branches.clear();
            self.ghost_tags.clear();
        }
        let complete = batch.complete;
        match self.load {
            LoadState::Refreshing | LoadState::Cached => {
                // Keep the stale view stable; swap when the fresh walk is complete.
                self.incoming
                    .get_or_insert_with(History::default)
                    .append(batch);
                if complete {
                    self.search.invalidate();
                    let fresh = self.incoming.take().unwrap_or_default();
                    let keep = (!self.pending.selected)
                        .then(|| self.history.commits.get(self.selected).map(|c| c.id))
                        .flatten();
                    self.history = fresh;
                    if let Some(id) = keep
                        && let Some(i) = self.history.index_of(&id)
                    {
                        self.selected = i;
                    }
                    self.load = LoadState::Complete;
                    self.clamp();
                    self.update_pending_row(true);
                    self.focus_startup_head(true);
                }
            }
            _ => {
                self.search.invalidate();
                self.history.append(batch);
                self.load = if complete {
                    LoadState::Complete
                } else {
                    LoadState::Loading
                };
                self.clamp();
                self.update_pending_row(false);
                self.focus_startup_head(false);
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
        let n = self.graph_len();
        if n == 0 {
            // An empty filter must not erase the underlying selection. It is
            // also temporarily empty while refreshing the same query.
            self.selected = self.selected.min(self.history.len().saturating_sub(1));
            self.scroll = 0;
            return;
        }
        if self.selected >= self.history.len() {
            self.selected = self.history.len().saturating_sub(1);
        }
        let max_scroll = n.saturating_sub(self.list_rows.max(1));
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
    }

    pub fn ensure_visible(&mut self) {
        let rows = self.list_rows.max(1);
        let selected = self.graph_selected();
        if selected < self.scroll {
            self.scroll = selected;
        } else if selected >= self.scroll + rows {
            self.scroll = selected + 1 - rows;
        }
    }

    pub fn select(&mut self, idx: usize) {
        if self.pending.selected {
            self.remember_pending_checks();
        }
        self.pending.selected = false;
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
        let n = self.graph_len() as isize;
        if n == 0 {
            return;
        }
        let target = (self.graph_selected() as isize + delta).clamp(0, n - 1);
        self.select_graph(target as usize);
    }

    fn scroll_view(&mut self, delta: isize) {
        let n = self.graph_len();
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

    pub fn confirm_checkout(&mut self, choices: Vec<crate::checkout::Request>) {
        if self.pending.busy || self.pending.request.is_some() {
            return;
        }
        if self.branches.busy || self.checkout_request.is_some() || choices.is_empty() {
            return;
        }
        self.branches.pending_jump = None;
        self.dragging_splitter = false;
        self.checkout_notice = None;
        self.checkout_confirmation = Some(crate::checkout::Confirmation::new(choices));
        self.needs_redraw = true;
    }

    pub fn confirm_graph_checkout(&mut self) {
        if let Some(id) = self.selected_id() {
            self.confirm_commit_checkout(id);
        }
    }

    pub fn confirm_commit_checkout(&mut self, id: ObjectId) {
        if self.branches.busy || self.checkout_request.is_some() {
            return;
        }
        let sha = id.to_string();
        // The browser is lazy and may never have been opened in this session.
        self.branches.rebuild(self.refs.as_ref());
        let branches = self
            .branches
            .entries
            .iter()
            .filter(|e| e.target.sha == sha)
            .map(|e| e.target.clone())
            .collect::<Vec<_>>();
        let mut branches = crate::checkout::branch_choices(branches, self.refs.as_ref());
        let current = self.refs.as_ref().and_then(|r| r.head_branch.as_ref());
        branches.sort_by_key(|t| (t.remote.is_some(), current != Some(&t.name), t.label()));
        let mut choices = branches
            .into_iter()
            .map(|target| {
                if target.remote.is_some() && self.ghosts.contains_key(&id) {
                    crate::checkout::Request::FetchBranch(target)
                } else {
                    crate::checkout::Request::Branch(target)
                }
            })
            .collect::<Vec<_>>();
        if choices.is_empty() {
            choices.push(crate::checkout::Request::Commit {
                id,
                remote: self.ghost_sources.get(&id).cloned(),
            });
        } else if choices.len() == 1 {
            if !self.pending.busy && self.pending.request.is_none() {
                self.checkout_confirmation = None;
                self.checkout_request = choices.pop();
                self.needs_redraw = true;
            }
            return;
        }
        self.confirm_checkout(choices);
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        self.startup_head_search = None;
        self.pending.autofocus = false;
        self.pending.return_to_head = None;
        if (self.pending.busy || self.pending.request.is_some())
            && (matches!(key.code, KeyCode::Esc | KeyCode::Char('q'))
                || (key.ctrl && key.code == KeyCode::Char('c')))
        {
            self.pending.status = "Wait for the commit to finish before exiting".into();
            self.needs_redraw = true;
            return;
        }
        if let Some(dialog) = &mut self.checkout_confirmation {
            match key.code {
                KeyCode::Enter if dialog.ready && !key.ctrl && !key.alt => {
                    self.checkout_request = Some(dialog.chosen().clone());
                    self.checkout_confirmation = None;
                }
                KeyCode::Esc => self.checkout_confirmation = None,
                KeyCode::Char('c') if key.ctrl => self.checkout_confirmation = None,
                KeyCode::Up => dialog.move_by(-1),
                KeyCode::Down => dialog.move_by(1),
                KeyCode::Home => dialog.selected = 0,
                KeyCode::End => dialog.selected = dialog.choices.len() - 1,
                _ => {}
            }
            self.needs_redraw = true;
            return;
        }
        if self.settings.open {
            self.settings.handle_key(key);
            self.needs_redraw = true;
            return;
        }
        if !self.branches.open && matches!(self.mode, Mode::Graph) && self.search.editing {
            match key.code {
                KeyCode::Esc => self.clear_search(),
                KeyCode::F(5) => {
                    self.want_refresh = true;
                    self.pending.want_refresh = true;
                    self.branches.want_refresh = Some(true);
                }
                KeyCode::Char('c') if key.ctrl => self.quit = true,
                KeyCode::Enter | KeyCode::Tab => {
                    self.search.editing = false;
                    self.focus = Focus::List;
                }
                KeyCode::Up => self.move_selection(-1),
                KeyCode::Down => self.move_selection(1),
                KeyCode::PageUp => self.move_selection(-(self.list_rows.max(1) as isize)),
                KeyCode::PageDown => self.move_selection(self.list_rows.max(1) as isize),
                _ => {
                    let before = self.search.text.value.clone();
                    self.search.text.edit(key, false);
                    if self.search.text.value != before {
                        self.search.changed();
                        self.scroll = 0;
                        if !self.search.active() {
                            self.ensure_visible();
                        }
                    }
                }
            }
            self.needs_redraw = true;
            return;
        }
        if !self.branches.busy
            && self.checkout_request.is_none()
            && (key.code == KeyCode::F(2)
                || (!self.branches.open
                    && !(self.pending.selected && self.focus == Focus::Details)
                    && !key.ctrl
                    && key.code == KeyCode::Char('s')))
        {
            self.settings.show();
            self.dragging_splitter = false;
            self.needs_redraw = true;
            return;
        }
        if key.ctrl && key.code == KeyCode::Char('c') {
            if self.branches.busy {
                self.branches.status =
                    "Checkout is running; wait for completion before quitting".into();
                self.needs_redraw = true;
            } else {
                self.quit = true;
            }
            return;
        }
        if self.branches.open {
            if key.code == KeyCode::Enter && !key.ctrl && !key.alt {
                if let Some(target) = self.branches.chosen().map(|e| e.target.clone()) {
                    self.confirm_checkout(vec![crate::checkout::Request::Branch(target)]);
                }
                return;
            }
            self.branches.handle_key(key);
            if !self.branches.open {
                self.branches.pending_jump = None;
            }
            if let Some(id) = self.branches.jump.take() {
                self.branches.pending_jump = None;
                if let Some(index) = self.history.index_of(&id) {
                    self.mode = Mode::Graph;
                    self.focus = Focus::List;
                    self.select(index);
                    self.branches.open = false;
                } else if let Some(target) = self.branches.chosen().map(|e| e.target.clone()) {
                    if target
                        .remote
                        .as_ref()
                        .is_some_and(|name| self.branches.remotes.contains_key(name))
                    {
                        self.branches
                            .preview_targets
                            .retain(|t| t.remote != target.remote || t.name != target.name);
                        self.branches.status =
                            format!("Loading ghost history for {}…", target.label());
                        self.branches.preview_targets.push(target);
                        self.branches.pending_jump = Some(id);
                        self.branches.preview_requested = true;
                    } else {
                        self.branches.status = "Remote preview needs a supported API connection; check F2 settings or retry after discovery.".into();
                    }
                }
            }
            self.needs_redraw = true;
            return;
        }
        if let Mode::Diff(view) = &mut self.mode {
            view.handle_key(key);
            let (close, redraw) = (view.close, view.needs_redraw);
            if close {
                self.close_diff();
            }
            self.needs_redraw |= redraw;
            return;
        }
        if self.pending.selected {
            if key.ctrl && key.code == KeyCode::Enter {
                self.request_commit();
                return;
            }
            if key.code == KeyCode::Tab {
                if key.shift {
                    match self.focus {
                        Focus::List => self.focus = Focus::Files,
                        Focus::Files => {
                            self.focus = Focus::Details;
                            self.pending.field = 4;
                        }
                        Focus::Details if self.pending.field > 0 => self.pending.field -= 1,
                        Focus::Details => self.focus = Focus::List,
                    }
                    self.needs_redraw = true;
                } else {
                    self.cycle_focus();
                }
                return;
            }
            if key.code == KeyCode::Esc && self.focus != Focus::List {
                self.focus = Focus::List;
                self.needs_redraw = true;
                return;
            }
            if self.focus == Focus::Details && key.code != KeyCode::F(5) {
                if !self.pending.busy {
                    match self.pending.field {
                        0 => self.pending.description.edit(key, true),
                        1 => self.pending.branch.edit(key, false),
                        2 if matches!(key.code, KeyCode::Enter | KeyCode::Char(' ')) => {
                            self.request_commit()
                        }
                        3 | 4 if matches!(key.code, KeyCode::Enter | KeyCode::Char(' ')) => {
                            if let Some(cs) = &mut self.details_changes {
                                cs.set_all_checked(self.pending.field == 3);
                            }
                        }
                        _ => {}
                    }
                }
                self.needs_redraw = true;
                return;
            }
            if self.focus == Focus::Files && key.code == KeyCode::Char(' ') {
                if let Some(cs) = &mut self.details_changes
                    && let Some(row) = cs.tree.visible.get(self.details_cursor.sel)
                {
                    cs.toggle_checked(row.node);
                }
                self.needs_redraw = true;
                return;
            }
            if key.code == KeyCode::Enter {
                if self.focus == Focus::List {
                    self.focus = Focus::Details;
                    self.pending.field = 0;
                } else {
                    let folder = self
                        .details_changes
                        .as_mut()
                        .is_some_and(|cs| self.details_cursor.toggle(&mut cs.tree));
                    if !folder {
                        self.open_diff();
                    }
                }
                self.needs_redraw = true;
                return;
            }
        }
        let page = self.list_rows.saturating_sub(1).max(1) as isize;
        match (key.code, key.ctrl) {
            (KeyCode::Char('/'), false) => {
                self.search.editing = true;
                self.focus = Focus::List;
                self.needs_redraw = true;
            }
            (KeyCode::Char('b'), false) => {
                self.branches.show(self.refs.as_ref());
                self.want_refresh = true;
                self.needs_redraw = true;
            }
            (KeyCode::Char('q'), _) => {
                if self.branches.busy {
                    self.branches.open = true;
                    self.needs_redraw = true;
                } else {
                    self.quit = true;
                }
            }
            (KeyCode::Esc, _) => {
                if self.search.active() {
                    self.clear_search();
                } else if self.branches.busy {
                    self.branches.open = true;
                    self.needs_redraw = true;
                } else if self.marks.is_empty() {
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
            (KeyCode::Enter, false) if self.focus != Focus::Files && !key.alt => {
                self.confirm_graph_checkout()
            }
            (KeyCode::Tab, _) => self.cycle_focus(),
            (KeyCode::F(5), false) => {
                self.want_refresh = true;
                self.pending.want_refresh = true;
                self.branches.want_refresh = Some(true);
            }
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
            KeyCode::Home | KeyCode::Char('g') => self.select_graph(0),
            KeyCode::End | KeyCode::Char('G') => self.select_graph(usize::MAX),
            KeyCode::Char('H') => {
                if let Some(h) = self.head_id()
                    && let Some(i) = self.history.index_of(&h)
                {
                    if self.search.active()
                        && !self
                            .search
                            .result
                            .as_ref()
                            .is_some_and(|r| r.positions.get(i).is_some_and(|&p| p != usize::MAX))
                    {
                        self.clear_search();
                    }
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
        if !matches!(m.kind, MouseKind::Move | MouseKind::Up(_)) {
            self.startup_head_search = None;
            self.pending.autofocus = false;
            self.pending.return_to_head = None;
        }
        if self.checkout_confirmation.is_some() {
            return;
        }
        if self.settings.open {
            crate::ui::settings::handle_mouse(&mut self.settings, m, screen_w, screen_h);
            self.needs_redraw = true;
            return;
        }
        if self.branches.open {
            match m.kind {
                MouseKind::Wheel(delta) => self.branches.move_by(-(delta as isize) * 3),
                MouseKind::Down(Button::Left)
                    if m.y >= 2 && (m.y as usize) < self.branches.rows + 2 =>
                {
                    self.branches
                        .select(self.branches.scroll + m.y as usize - 2);
                }
                _ => {}
            }
            self.needs_redraw = true;
            return;
        }
        if let Mode::Diff(view) = &mut self.mode {
            let da = crate::ui::diff::compute_areas(screen_w, screen_h, view.sidebar_width);
            view.handle_mouse(m, &da);
            self.needs_redraw |= view.needs_redraw;
            return;
        }
        if matches!(m.kind, MouseKind::Down(Button::Left))
            && (self.search.editing || self.search.active())
        {
            self.search.editing = areas.header.contains(m.x, m.y);
            self.needs_redraw = true;
        }
        if self.pending.selected
            && areas.details.contains(m.x, m.y)
            && matches!(
                m.kind,
                MouseKind::Down(Button::Left) | MouseKind::DoubleClick(Button::Left)
            )
        {
            let a = self.pending.areas;
            if a.description.contains(m.x, m.y) {
                self.focus = Focus::Details;
                self.pending.field = 0;
                self.pending.description.cursor = self.pending.description.value.len();
            } else if a.branch.contains(m.x, m.y) {
                self.focus = Focus::Details;
                self.pending.field = 1;
                self.pending.branch.cursor = self.pending.branch.value.len();
            } else if a.commit.contains(m.x, m.y) {
                self.request_commit();
            } else if a.check_all.contains(m.x, m.y) || a.uncheck_all.contains(m.x, m.y) {
                if let Some(cs) = &mut self.details_changes {
                    cs.set_all_checked(a.check_all.contains(m.x, m.y));
                }
            } else if self.files_area.contains(m.x, m.y) {
                self.focus = Focus::Files;
                let row = self.details_cursor.scroll + (m.y - self.files_area.y) as usize;
                if let Some(cs) = &mut self.details_changes
                    && let Some(visible) = cs.tree.visible.get(row).copied()
                {
                    self.details_cursor.select(&cs.tree, row);
                    let checkbox_x = self.files_area.x
                        + 1
                        + (visible.depth * 2).min(self.files_area.w.saturating_sub(10));
                    if m.x >= checkbox_x && m.x < checkbox_x + 3 {
                        cs.toggle_checked(visible.node);
                    } else if cs.tree.nodes[visible.node].is_folder() {
                        self.details_cursor.toggle(&mut cs.tree);
                    } else if matches!(m.kind, MouseKind::DoubleClick(_)) {
                        self.open_diff();
                    }
                }
            }
            self.needs_redraw = true;
            return;
        }
        match m.kind {
            MouseKind::Down(Button::Left) | MouseKind::DoubleClick(Button::Left) => {
                if areas.splitter.contains(m.x, m.y) {
                    self.dragging_splitter = true;
                } else if areas.list.contains(m.x, m.y) {
                    self.focus = Focus::List;
                    let idx = self.scroll + (m.y - areas.list.y) as usize;
                    if idx < self.graph_len() {
                        self.select_graph(idx);
                        if self.pending.selected {
                            self.sync_details_changes();
                            if matches!(m.kind, MouseKind::DoubleClick(_)) {
                                self.open_diff();
                            }
                            self.needs_redraw = true;
                            return;
                        }
                        let id = self.history.commits[self.selected].id;
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

    fn dirty_snapshot(head: ObjectId) -> crate::pending::Snapshot {
        crate::pending::Snapshot {
            head: Some(head),
            branch: Some("main".into()),
            files: ["src/a.rs", "src/b.rs", "root.txt"]
                .into_iter()
                .map(|path| crate::diff::FileChange {
                    worktree: true,
                    remote: None,
                    path: path.into(),
                    old_path: None,
                    kind: crate::diff::ChangeKind::Modified,
                    old: None,
                    new: None,
                })
                .collect(),
            ..Default::default()
        }
    }
    #[test]
    fn pending_startup_has_priority_and_navigation_maps_back_to_real_commits() {
        let mut app = App::new();
        app.list_rows = 10;
        app.pending.known = false;
        app.pending.autofocus = true;
        app.set_cached(history(80), true);
        let head = app.history.commits[20].id;
        app.set_refs(RefSnapshot {
            head_id: Some(head),
            ..Default::default()
        });
        assert_eq!(app.selected_id(), Some(head)); // HEAD is available before slow status work.
        app.install_pending(Ok(dirty_snapshot(head)));
        assert!(app.pending.selected);
        assert_eq!(app.graph_selected(), 20);
        assert_eq!(app.graph_len(), 81);
        assert_eq!(app.history.len(), 80);
        assert_eq!(app.history_index(20), None);
        assert_eq!(app.history_index(21), Some(20));
        assert!(app.scroll <= 20 && 20 < app.scroll + app.list_rows);
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.selected_id(), Some(head));
        app.handle_key(key(KeyCode::Up));
        assert!(app.pending.selected);
        app.handle_key(key(KeyCode::Char('H')));
        assert_eq!(app.selected_id(), Some(head));
        app.handle_key(key(KeyCode::End));
        assert_eq!(app.selected, 79);
        let mut early = App::new();
        early.pending.autofocus = true;
        early.set_cached(history(80), true);
        early.handle_key(key(KeyCode::Down));
        early.install_pending(Ok(dirty_snapshot(head)));
        assert!(!early.pending.selected);
        assert_eq!(early.selected, 1);
    }
    #[test]
    fn go_to_head_prefers_pending_clears_filters_and_keeps_the_draft() {
        let mut app = app_with(80);
        let head = app.history.commits[20].id;
        app.set_refs(RefSnapshot {
            head_id: Some(head),
            ..Default::default()
        });
        app.install_pending(Ok(dirty_snapshot(head)));
        app.pending.description.value = "Keep this draft".into();
        app.search.text.value = "unrelated filter".into();
        app.search.changed();
        app.branches.open = true;
        app.go_to_head();
        assert!(app.pending.selected);
        assert_eq!(app.graph_selected(), app.pending.row());
        assert!(!app.search.active() && !app.branches.open);
        assert_eq!(app.pending.description.value, "Keep this draft");
        app.install_pending(Ok(crate::pending::Snapshot {
            head: Some(head),
            ..Default::default()
        }));
        app.go_to_head();
        assert!(!app.pending.selected);
        assert_eq!(app.selected_id(), Some(head));
    }

    #[test]
    fn explicit_go_to_head_finishes_when_later_history_arrives() {
        let head = history(100).commits[70].id;
        for stale in [false, true] {
            let mut app = App::new();
            app.list_rows = 10;
            app.pending.known = true;
            app.set_refs(RefSnapshot {
                head_id: Some(head),
                ..Default::default()
            });
            if stale {
                app.set_cached(history(20), false);
            }
            app.start_walk();
            let mut first = history(40);
            first.complete = false;
            app.push_batch(first);
            app.begin_navigation(); // Cancel startup focus before explicitly requesting HEAD.
            assert!(app.startup_head_search.is_none());
            app.go_to_head();
            assert_eq!(app.startup_head_search, Some(if stale { 20 } else { 40 }));
            assert_ne!(app.selected_id(), Some(head));

            let mut tail = history(100);
            tail.commits.drain(..40);
            app.push_batch(tail);
            assert_eq!(app.selected_id(), Some(head));
            assert!(app.scroll <= 70 && 70 < app.scroll + app.list_rows);
            assert!(app.startup_head_search.is_none());
            assert!(!app.pending.autofocus);
        }
    }

    #[test]
    fn navigation_cancels_explicit_head_waiting_for_later_history() {
        let head = history(100).commits[70].id;
        for keyboard in [false, true] {
            let mut app = App::new();
            app.set_refs(RefSnapshot {
                head_id: Some(head),
                ..Default::default()
            });
            app.start_walk();
            let mut first = history(40);
            first.complete = false;
            app.push_batch(first);
            app.go_to_head();
            assert!(app.startup_head_search.is_some());
            if keyboard {
                app.handle_key(key(KeyCode::Down));
            } else {
                // The desktop's actual Select action cancels deferred focus first.
                app.begin_navigation();
                app.select_graph(3);
            }
            let selected = app.selected_id();
            assert!(app.startup_head_search.is_none() && !app.pending.autofocus);
            let mut tail = history(100);
            tail.commits.drain(..40);
            app.push_batch(tail);
            assert_eq!(app.selected_id(), selected);
        }
    }

    #[test]
    fn explicit_go_to_head_waits_for_first_pending_status_until_navigation() {
        for navigate in [false, true] {
            let mut app = app_with(80);
            app.pending.known = false;
            let head = app.history.commits[20].id;
            app.set_refs(RefSnapshot {
                head_id: Some(head),
                ..Default::default()
            });
            app.go_to_head();
            assert_eq!(app.selected_id(), Some(head));
            assert!(app.startup_head_search.is_none() && app.pending.autofocus);
            if navigate {
                app.begin_navigation();
                app.select_graph(3);
            }
            let selected = app.selected_id();
            app.install_pending(Ok(dirty_snapshot(head)));
            assert_eq!(app.pending.selected, !navigate);
            if navigate {
                assert_eq!(app.selected_id(), selected);
            } else {
                assert_eq!(app.graph_selected(), app.pending.row());
            }
            assert!(!app.pending.autofocus);
        }

        let mut app = app_with(80);
        let head = app.history.commits[20].id;
        app.set_refs(RefSnapshot {
            head_id: Some(head),
            ..Default::default()
        });
        app.install_pending(Err("status unavailable".into()));
        app.go_to_head();
        assert_eq!(app.selected_id(), Some(head));
        assert!(!app.pending.autofocus);
        // A known failed status does not turn this click into ongoing follow mode.
        app.install_pending(Ok(dirty_snapshot(head)));
        assert!(!app.pending.selected);
    }

    #[test]
    fn provider_badges_refresh_active_search_without_changing_git_refs() {
        let mut app = app_with(2);
        let id = app.history.commits[0].id;
        app.search.text.value = "MR !42".into();
        app.search.changed();
        let old = app.search_request().unwrap();
        let labels = HashMap::from([(id, vec!["MR !42".into()])]);
        assert!(app.set_provider_search_labels(labels.clone()));
        let generation = app.search.generation;
        assert!(!app.set_provider_search_labels(labels));
        assert_eq!(generation, app.search.generation);
        let current = app.search_request().unwrap();
        assert_ne!(old.generation, current.generation);
        assert_eq!(current.input.labels[&id], ["MR !42"]);
        assert!(!old.input.labels.contains_key(&id));
        assert!(!app.labels.contains_key(&id));
        assert!(app.set_provider_search_labels(HashMap::new()));
        assert!(!app.search_request().unwrap().input.labels.contains_key(&id));
    }

    #[test]
    fn pending_refresh_does_not_recenter_after_manual_graph_scrolling() {
        let mut app = app_with(80);
        let head = app.history.commits[20].id;
        app.set_refs(RefSnapshot {
            head_id: Some(head),
            ..Default::default()
        });
        app.pending.autofocus = true;
        app.install_pending(Ok(dirty_snapshot(head)));
        app.handle_mouse(
            MouseEvent {
                kind: MouseKind::Wheel(-10),
                x: 5,
                y: 3,
                ctrl: false,
                shift: false,
            },
            &areas(),
            100,
            12,
        );
        let scroll = app.scroll;
        assert!(scroll > app.pending.row());
        let mut changed = dirty_snapshot(head);
        changed.fingerprint = 1;
        app.install_pending(Ok(changed));
        assert_eq!(app.scroll, scroll);
    }
    #[test]
    fn pending_composer_keeps_text_checks_and_diff_selection_without_global_shortcuts() {
        let mut app = app_with(4);
        let head = app.history.commits[1].id;
        app.set_refs(RefSnapshot {
            head_id: Some(head),
            ..Default::default()
        });
        app.pending.autofocus = true;
        app.install_pending(Ok(dirty_snapshot(head)));
        app.sync_details_changes();
        app.handle_key(key(KeyCode::Tab));
        for ch in "Describe b s q é".chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Char('x')));
        assert_eq!(app.pending.description.value, "Describe b s q é\nx");
        assert!(!app.settings.open && !app.branches.open && !app.quit);
        app.handle_key(key(KeyCode::Tab));
        for ch in "feature/commit".chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        app.focus = Focus::Files;
        app.details_cursor.sel = 0;
        app.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(app.details_changes.as_ref().unwrap().checked_count(), 1);
        assert!(app.marks.is_empty());
        app.handle_key(key(KeyCode::Char('d')));
        assert!(matches!(app.mode, Mode::Diff(_)));
        if let Mode::Diff(view) = &mut app.mode {
            assert!(view.changes.files.iter().all(|f| f.worktree));
            view.cursor.select(&view.changes.tree, 0);
            view.handle_key(key(KeyCode::Char(' ')));
            assert_eq!(view.changes.checked_count(), 3);
        }
        app.handle_key(key(KeyCode::Esc));
        assert!(app.pending.selected);
        assert_eq!(app.details_changes.as_ref().unwrap().checked_count(), 3);
        app.details_changes
            .as_mut()
            .unwrap()
            .checked
            .as_mut()
            .unwrap()[1] = false;
        app.details_changes.as_mut().unwrap().recompute_checks();
        app.select(0);
        app.sync_details_changes();
        app.select_graph(app.pending.row());
        app.sync_details_changes();
        assert_eq!(app.details_changes.as_ref().unwrap().checked_count(), 2);
        let mut screen = crate::term::Screen::new(120, 34);
        let areas = crate::ui::compute_areas(120, 34, 45);
        crate::ui::draw(&mut app, &mut screen, &areas, 0);
        let text: String = screen.test_cells().iter().map(|c| c.ch).collect();
        for required in [
            "Commit changes",
            "Description",
            "New branch",
            "Modified files (2/3)",
            "Check all",
            "Uncheck all",
            "[-]",
            "[ ]",
        ] {
            assert!(text.contains(required), "Missing {required}");
        }
        let mut submit = key(KeyCode::Enter);
        submit.ctrl = true;
        app.handle_key(submit);
        let request = app.pending.request.as_ref().unwrap();
        assert_eq!(request.files.len(), 2);
        assert_eq!(request.new_branch, "feature/commit");
        assert_eq!(request.description, "Describe b s q é\nx");
        app.pending.request = None;
        let button = app.pending.areas.commit;
        app.handle_mouse(
            MouseEvent {
                kind: MouseKind::Down(Button::Left),
                x: button.x + 1,
                y: button.y,
                ctrl: false,
                shift: false,
            },
            &areas,
            120,
            34,
        );
        assert_eq!(app.pending.request.as_ref().unwrap().files.len(), 2);
    }
    #[test]
    fn removing_pending_node_selects_new_head_when_its_walk_arrives() {
        let mut app = app_with(4);
        let head = app.history.commits[1].id;
        app.pending.autofocus = true;
        app.install_pending(Ok(dirty_snapshot(head)));
        let next = history(5).commits[4].id;
        app.install_pending(Ok(crate::pending::Snapshot {
            head: Some(next),
            ..Default::default()
        }));
        assert!(!app.pending.selected);
        assert_eq!(app.pending.return_to_head, Some(next));
        app.start_walk();
        app.push_batch(history(5));
        assert_eq!(app.selected_id(), Some(next));
        assert!(app.pending.return_to_head.is_none());
    }

    #[test]
    fn startup_selects_head_with_cache_and_refs_in_either_order_only_once() {
        let head = history(100).commits[70].id;
        for refs_first in [false, true] {
            let mut app = App::new();
            app.list_rows = 10;
            let refs = RefSnapshot {
                head_id: Some(head),
                ..Default::default()
            };
            if refs_first {
                app.set_refs(refs);
                app.set_cached(history(100), true);
            } else {
                app.set_cached(history(100), true);
                app.set_refs(refs);
            }
            assert_eq!(app.selected_id(), Some(head));
            assert!(app.scroll <= 70 && 70 < app.scroll + app.list_rows);
            assert!(app.startup_head_search.is_none());
            // Later ref refreshes do not turn startup focus into ongoing follow mode.
            app.set_refs(RefSnapshot {
                head_id: Some(app.history.commits[90].id),
                head_branch: Some("other".into()),
                ..Default::default()
            });
            assert_eq!(app.selected_id(), Some(head));
        }
    }

    #[test]
    fn startup_finds_head_in_later_batches_and_after_a_stale_cache_swap() {
        let head = history(100).commits[70].id;
        for stale in [false, true] {
            let mut app = App::new();
            app.list_rows = 10;
            app.set_refs(RefSnapshot {
                head_id: Some(head),
                ..Default::default()
            });
            if stale {
                app.set_cached(history(20), false);
            }
            app.start_walk();
            let mut first = history(40);
            first.complete = false;
            app.push_batch(first);
            assert_eq!(app.startup_head_search, Some(if stale { 20 } else { 40 }));
            assert_ne!(app.selected_id(), Some(head));
            let mut tail = history(100);
            tail.commits.drain(..40);
            app.push_batch(tail);
            assert_eq!(app.selected_id(), Some(head));
            assert!(app.scroll <= 70 && 70 < app.scroll + app.list_rows);
        }
    }

    #[test]
    fn startup_focus_respects_early_input_but_ignores_mouse_motion() {
        let head = history(100).commits[70].id;
        for action in 0..3 {
            let mut app = app_with(100);
            let mouse = |kind| MouseEvent {
                kind,
                x: 5,
                y: 3,
                ctrl: false,
                shift: false,
            };
            match action {
                0 => app.handle_key(key(KeyCode::Down)),
                1 => app.handle_mouse(mouse(MouseKind::Down(Button::Left)), &areas(), 100, 12),
                _ => app.handle_mouse(mouse(MouseKind::Move), &areas(), 100, 12),
            }
            let selected = app.selected_id();
            app.set_refs(RefSnapshot {
                head_id: Some(head),
                ..Default::default()
            });
            assert_eq!(
                app.selected_id(),
                if action == 2 { Some(head) } else { selected }
            );
        }
    }

    #[test]
    fn head_badge_is_visible_attached_detached_and_after_ref_updates() {
        use crate::git::RefEntry;
        let mut app = app_with(3);
        let first = app.history.commits[0].id;
        let last = app.history.commits[2].id;
        let branch = "a-very-long-checked-out-branch-name";
        app.set_refs(RefSnapshot {
            head_id: Some(first),
            head_branch: Some(branch.into()),
            refs: vec![RefEntry {
                name: branch.into(),
                id: first,
                kind: RefKind::Head,
            }],
            ..Default::default()
        });
        let labels = &app.labels[&first];
        assert_eq!(labels[0].name, "HEAD");
        assert_eq!(labels[1].name, branch);
        // The badge is display metadata, not a new branch or checkout target.
        assert_eq!(app.refs.as_ref().unwrap().refs.len(), 1);
        app.branches.show(app.refs.as_ref());
        assert_eq!(app.branches.entries.len(), 1);
        app.branches.open = false;
        app.select(1);
        for width in [20, 50, 120] {
            let mut screen = crate::term::Screen::new(width, 3);
            crate::ui::list::draw(&mut app, &mut screen, Rect::new(0, 0, width, 3), 0);
            let row = &screen.test_cells()[..width as usize];
            let text: String = row.iter().map(|cell| cell.ch).collect();
            let pos = row
                .windows(4)
                .position(|cells| cells.iter().map(|cell| cell.ch).eq("HEAD".chars()));
            let pos = pos.unwrap_or_else(|| panic!("Missing HEAD at width {width}: {text}"));
            for cell in &row[pos..pos + 4] {
                assert_eq!(cell.style.bg, crate::ui::theme::FG_HEAD);
                assert_ne!(cell.style.attrs & crate::term::screen::BOLD, 0);
            }
            let other_row: String = screen.test_cells()[width as usize..width as usize * 2]
                .iter()
                .map(|cell| cell.ch)
                .collect();
            assert!(!other_row.contains("HEAD"));
        }
        let detached = RefSnapshot {
            head_id: Some(last),
            refs: vec![RefEntry {
                name: "HEAD".into(),
                id: last,
                kind: RefKind::Head,
            }],
            ..Default::default()
        };
        app.set_refs(detached.clone());
        app.set_refs(detached);
        assert!(!app.labels.contains_key(&first));
        assert_eq!(app.labels[&last].len(), 1);
        assert_eq!(app.labels[&last][0].name, "HEAD");
        app.select(2);
        let mut screen = crate::term::Screen::new(80, 3);
        crate::ui::list::draw(&mut app, &mut screen, Rect::new(0, 0, 80, 3), 0);
        let text: String = screen.test_cells()[160..240]
            .iter()
            .map(|cell| cell.ch)
            .collect();
        assert_eq!(text.matches("HEAD").count(), 1);
        app.set_refs(RefSnapshot::default());
        assert!(app.labels.is_empty()); // Unborn HEAD has no commit to label.
    }

    #[test]
    fn provider_labels_preserve_fetched_refs_and_mark_only_different_live_tips() {
        use crate::git::RefEntry;
        for url in [
            "https://github.com/test/repo",
            "https://gitlab.com/test/repo",
        ] {
            let mut app = app_with(3);
            let live = app.history.commits[0].id;
            let fetched = app.history.commits[1].id;
            let refs = RefSnapshot {
                refs: vec![
                    RefEntry {
                        name: "origin/main".into(),
                        id: fetched,
                        kind: RefKind::Remote,
                    },
                    RefEntry {
                        name: "origin/unlisted".into(),
                        id: fetched,
                        kind: RefKind::Remote,
                    },
                ],
                ..Default::default()
            };
            app.set_refs(refs.clone());
            let mut update = crate::remote::Update {
                remote: crate::remote::Remote::parse("origin", url, None).unwrap(),
                branches: vec![crate::remote::Branch {
                    name: "main".into(),
                    sha: live.to_string(),
                }],
                scoped: false,
                cached: false,
                stale: false,
            };
            app.branches.update(update.clone(), app.refs.as_ref());
            // The live tip is already a local object: cloud state must describe
            // the branch's provider origin, not whether the commit is a ghost.
            for _ in 0..2 {
                app.remote_labels();
                let labels = &app.labels[&fetched];
                assert_eq!(labels.len(), 2);
                assert!(labels.iter().all(|l| !l.remote_only));
                let cloud = &app.labels[&live];
                assert_eq!(cloud.len(), 1);
                assert_eq!(cloud[0].name, "origin/main");
                assert_eq!(cloud[0].kind, RefKind::Remote);
                assert!(cloud[0].remote_only);
                assert_eq!(app.refs.as_ref().unwrap().refs[0].id, fetched);
            }
            // A local fetch converges with the live tip: leave just the real ref.
            let mut refreshed = refs.clone();
            refreshed.refs[0].id = live;
            app.set_refs(refreshed);
            assert_eq!(app.labels[&live].len(), 1);
            assert!(!app.labels[&live][0].remote_only);
            assert_eq!(app.labels[&fetched][0].name, "origin/unlisted");
            // A provider can also move back to an already fetched object.
            update.branches[0].sha = fetched.to_string();
            app.branches.update(update.clone(), app.refs.as_ref());
            app.remote_labels();
            assert!(app.labels[&fetched].iter().any(|l| l.remote_only));
            assert!(!app.labels[&live][0].remote_only);
            // Removing an API branch removes only its overlay, even for a full
            // provider listing. Fetched refs stay until Git itself removes them.
            update.branches.clear();
            app.branches.update(update, app.refs.as_ref());
            app.remote_labels();
            assert!(app.labels.values().flatten().all(|l| !l.remote_only));
            assert_eq!(app.labels[&live][0].name, "origin/main");
            assert_eq!(app.labels[&fetched][0].name, "origin/unlisted");
            app.branches.remotes.clear();
            app.set_refs(refs);
            assert!(!app.labels.contains_key(&live));
        }
    }

    #[test]
    fn late_remote_metadata_retries_pending_and_failed_diffs_without_accepting_old_results() {
        for failed in [false, true] {
            for tab in 0..3 {
                let mut app = app_with(3);
                let id = app.selected_id().unwrap();
                app.sync_details_changes();
                if tab > 0 {
                    app.open_diff();
                }
                if tab == 2 {
                    app.park_diff();
                }
                let request = app.pending_requests().remove(0);
                let DiffReq::Tree {
                    key, remote: None, ..
                } = request
                else {
                    panic!("expected local request")
                };
                if failed {
                    app.changes_by_key_mut(key)
                        .unwrap()
                        .set_tree(Err("Object could not be found".into()));
                }
                let remote =
                    crate::remote::Remote::parse("origin", "https://github.com/test/repo", None)
                        .unwrap();
                app.install_preview(crate::remote::history::Preview {
                    history: None,
                    ghosts: Default::default(),
                    sources: [(id, remote.clone())].into_iter().collect(),
                    branches: Vec::new(),
                    tags: Vec::new(),
                    errors: Vec::new(),
                });
                // Replacing the key prevents a late local error from overwriting
                // a remote result, including on a parked tab or details panel.
                assert!(app.changes_by_key_mut(key).is_none());
                let request = app.pending_requests().remove(0);
                let DiffReq::Tree {
                    key: next,
                    remote: Some(source),
                    ..
                } = request
                else {
                    panic!("expected API retry")
                };
                assert_ne!(key, next);
                assert_eq!(source, remote);
                assert!(app.changes_by_key_mut(next).unwrap().error.is_none());
            }
        }
    }

    #[test]
    #[ignore = "explicit large-repository graph label performance measurement"]
    fn benchmark_graph_labels() {
        let path = std::path::PathBuf::from(
            std::env::var_os("GIT_RUSTLE_BENCH_REPO").expect("repository required"),
        );
        let repo = crate::git::open(&path).unwrap();
        let refs = crate::git::snapshot_refs(&repo).unwrap();
        let mut history = History::default();
        crate::git::walk(&repo, &refs.tips, &mut |chunk| {
            history.append(chunk);
            true
        })
        .unwrap();
        let mut app = App::new();
        app.set_cached(history, true);
        let head = app.history.index_of(&refs.head_id.unwrap()).unwrap();
        let now = crate::ui::time::now_unix();
        let mut samples = Vec::new();
        for n in 0..16 {
            let snapshot = refs.clone();
            let start = std::time::Instant::now();
            app.set_refs(snapshot);
            let labels_ms = start.elapsed().as_secs_f64() * 1000.0;
            let mut screen = crate::term::Screen::new(120, 40);
            let area = Rect::new(0, 0, 120, 38);
            let start = std::time::Instant::now();
            for step in 0..200 {
                app.select((head + step % 40).min(app.history.len() - 1));
                screen.clear();
                crate::ui::list::draw(&mut app, &mut screen, area, now);
                std::hint::black_box(screen.test_cells());
            }
            let draw_ms = start.elapsed().as_secs_f64() * 1000.0 / 200.0;
            if n > 0 {
                samples.push(serde_json::json!({"sample":n-1,"installLabelsMs":labels_ms,"selectDrawMs":draw_ms}));
            }
        }
        println!(
            "{} commits; one warm-up, 15 samples: {}",
            app.history.len(),
            serde_json::to_string(&samples).unwrap()
        );
    }

    #[test]
    fn graph_f5_refreshes_local_and_remote_refs_in_each_panel() {
        for focus in [Focus::List, Focus::Details, Focus::Files] {
            let mut app = app_with(3);
            app.focus = focus;
            app.handle_key(key(KeyCode::Char('r')));
            assert!(!app.want_refresh);
            assert_eq!(app.branches.want_refresh, None);
            app.focus = focus;
            app.handle_key(key(KeyCode::F(5)));
            assert!(app.want_refresh);
            assert_eq!(app.branches.want_refresh, Some(true));
        }
        let mut app = app_with(3);
        app.branches.show(None);
        app.handle_key(key(KeyCode::F(5)));
        assert_eq!(app.branches.want_refresh, Some(true));
    }

    #[test]
    fn graph_checkout_is_modal_frozen_and_requires_a_visible_confirmation() {
        use crate::checkout::Request;
        let mut app = app_with(3);
        let id = app.selected_id().unwrap();
        let refs = RefSnapshot {
            head_id: Some(id),
            head_branch: Some("main".into()),
            refs: vec![
                crate::git::RefEntry {
                    name: "other".into(),
                    id,
                    kind: RefKind::Branch,
                },
                crate::git::RefEntry {
                    name: "main".into(),
                    id,
                    kind: RefKind::Branch,
                },
                crate::git::RefEntry {
                    name: "origin/main".into(),
                    id,
                    kind: RefKind::Remote,
                },
            ],
            ..Default::default()
        };
        app.set_refs(refs);
        app.toggle_mark(id);
        app.handle_key(key(KeyCode::Enter));
        assert!(app.checkout_request.is_none());
        let dialog = app.checkout_confirmation.as_ref().unwrap();
        assert_eq!(dialog.choices.len(), 2);
        assert!(
            matches!(dialog.chosen(),Request::Branch(t) if t.name == "main" && t.remote.is_none())
        );
        // Buffered input cannot confirm before the dialog is visible.
        app.handle_key(key(KeyCode::Enter));
        assert!(app.checkout_request.is_none());
        for code in [
            KeyCode::Char('b'),
            KeyCode::Char('s'),
            KeyCode::Char('d'),
            KeyCode::Char('q'),
            KeyCode::F(5),
            KeyCode::Delete,
        ] {
            app.handle_key(key(code));
        }
        assert!(!app.branches.open && !app.settings.open && !app.quit);
        assert!(!app.want_refresh && app.branches.want_refresh.is_none());
        assert_eq!(app.marks, vec![id]);
        app.handle_mouse(
            MouseEvent {
                kind: MouseKind::Down(Button::Left),
                x: 5,
                y: 3,
                ctrl: false,
                shift: false,
            },
            &areas(),
            100,
            12,
        );
        assert_eq!(app.selected, 0);
        app.handle_key(key(KeyCode::Esc));
        assert!(app.checkout_confirmation.is_none() && app.checkout_request.is_none());
        assert_eq!(app.marks, vec![id]);
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Down));
        crate::ui::checkout::draw(
            app.checkout_confirmation.as_mut().unwrap(),
            &mut crate::term::Screen::new(120, 30),
        );
        // Refreshes or selection changes cannot retarget the pending checkout.
        app.set_refs(RefSnapshot::default());
        app.select(2);
        app.handle_key(key(KeyCode::Enter));
        let expected_sha = id.to_string();
        assert!(
            matches!(&app.checkout_request,Some(Request::Branch(t)) if t.name == "other" && t.sha == expected_sha)
        );
        assert!(app.checkout_confirmation.is_none());
        app.handle_key(key(KeyCode::Enter));
        assert!(app.checkout_confirmation.is_none()); // already queued
    }

    #[test]
    fn graph_single_local_with_renamed_upstream_dispatches_without_detached_popup() {
        let mut app = app_with(1);
        let id = app.selected_id().unwrap();
        let mut refs = RefSnapshot {
            refs: vec![
                crate::git::RefEntry {
                    name: "local-topic".into(),
                    id,
                    kind: RefKind::Branch,
                },
                crate::git::RefEntry {
                    name: "origin/server-topic".into(),
                    id,
                    kind: RefKind::Remote,
                },
            ],
            ..Default::default()
        };
        refs.upstreams.insert(
            "local-topic".into(),
            ("origin".into(), "server-topic".into()),
        );
        app.set_refs(refs);
        app.confirm_commit_checkout(id);
        assert!(app.checkout_confirmation.is_none());
        assert!(
            matches!(app.checkout_request,Some(crate::checkout::Request::Branch(ref target)) if target.name=="local-topic" && target.remote.is_none())
        );
    }

    #[test]
    fn browser_checkout_uses_the_same_confirmation_and_cancel_keeps_the_browser() {
        use crate::checkout::Request;
        let mut app = app_with(1);
        let id = app.selected_id().unwrap();
        let refs = RefSnapshot {
            refs: vec![crate::git::RefEntry {
                name: "topic".into(),
                id,
                kind: RefKind::Branch,
            }],
            ..Default::default()
        };
        app.set_refs(refs);
        app.branches.show(app.refs.as_ref());
        app.handle_key(key(KeyCode::Enter));
        assert!(app.checkout_request.is_none());
        assert_eq!(app.checkout_confirmation.as_ref().unwrap().choices.len(), 1);
        app.handle_key(key(KeyCode::Esc));
        assert!(app.branches.open && app.checkout_request.is_none());
        app.handle_key(key(KeyCode::Enter));
        crate::ui::checkout::draw(
            app.checkout_confirmation.as_mut().unwrap(),
            &mut crate::term::Screen::new(120, 30),
        );
        app.handle_key(key(KeyCode::Enter));
        assert!(matches!(&app.checkout_request,Some(Request::Branch(t)) if t.name == "topic"));
    }

    #[test]
    fn ghost_branch_confirmation_fetches_its_frozen_remote_target() {
        use crate::checkout::Request;
        let mut app = app_with(3);
        let id = app.selected_id().unwrap();
        let sha = id.to_string();
        let local = app.history.commits[1].id;
        let remote =
            crate::remote::Remote::parse("origin", "https://github.com/test/repo", None).unwrap();
        app.set_refs(RefSnapshot {
            head_id: Some(local),
            head_branch: Some("topic/nested".into()),
            refs: vec![crate::git::RefEntry {
                name: "topic/nested".into(),
                id: local,
                kind: RefKind::Branch,
            }],
            ..Default::default()
        });
        app.branches.update(
            crate::remote::Update {
                remote: remote.clone(),
                branches: vec![crate::remote::Branch {
                    name: "topic/nested".into(),
                    sha: id.to_string(),
                }],
                scoped: false,
                cached: false,
                stale: false,
            },
            app.refs.as_ref(),
        );
        app.ghosts.insert(
            id,
            CommitDetails {
                id,
                parents: vec![local],
                author_name: String::new(),
                author_email: String::new(),
                author_time: 0,
                author_offset: 0,
                committer_name: String::new(),
                committer_email: String::new(),
                commit_time: 0,
                commit_offset: 0,
                message: "Remote-only tip".into(),
            },
        );
        app.ghost_sources.insert(id, remote.clone());
        app.confirm_commit_checkout(id);
        assert!(matches!(app.checkout_request.as_ref().unwrap(),
            Request::FetchBranch(t) if t.name == "topic/nested"
                && t.remote.as_deref() == Some("origin")
                && t.url.as_deref() == Some(remote.url.as_str())
                && t.sha == sha));
        assert!(app.checkout_confirmation.is_none());
        // A refresh cannot downgrade or retarget the already-frozen request.
        app.branches.remotes.clear();
        app.ghosts.clear();
        app.ghost_sources.clear();
        assert!(matches!(&app.checkout_request,
            Some(Request::FetchBranch(t)) if t.sha == sha
                && t.url.as_deref() == Some(remote.url.as_str())));
        app.checkout_request = None;
        app.branches.update(
            crate::remote::Update {
                remote,
                branches: vec![crate::remote::Branch {
                    name: "topic/nested".into(),
                    sha: id.to_string(),
                }],
                scoped: false,
                cached: false,
                stale: false,
            },
            app.refs.as_ref(),
        );
        app.confirm_commit_checkout(id);
        assert!(matches!(
            app.checkout_request.as_ref().unwrap(),
            Request::Branch(_)
        ));
    }

    #[test]
    fn graph_commit_confirmation_captures_remote_and_blocks_unreadable_confirmation() {
        use crate::checkout::Request;
        let mut app = app_with(3);
        app.focus = Focus::Details;
        let id = app.selected_id().unwrap();
        let remote =
            crate::remote::Remote::parse("origin", "https://github.com/git/git", None).unwrap();
        app.ghost_sources.insert(id, remote.clone());
        app.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(app.checkout_confirmation.as_ref().unwrap().chosen(),Request::Commit {id:target,remote:Some(r)} if *target == id && r == &remote)
        );
        crate::ui::checkout::draw(
            app.checkout_confirmation.as_mut().unwrap(),
            &mut crate::term::Screen::new(30, 6),
        );
        app.handle_key(key(KeyCode::Enter));
        assert!(app.checkout_request.is_none());
        crate::ui::checkout::draw(
            app.checkout_confirmation.as_mut().unwrap(),
            &mut crate::term::Screen::new(120, 30),
        );
        app.ghost_sources.clear();
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            app.checkout_request,
            Some(Request::Commit {
                id,
                remote: Some(remote)
            })
        );
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
    fn desktop_diff_uses_only_one_or_two_explicit_selections() {
        let mut app = app_with(4);
        let ids: Vec<_> = app.history.commits.iter().map(|c| c.id).collect();
        app.open_selected_diff(&[]);
        assert!(matches!(app.mode, Mode::Graph));
        app.open_selected_diff(&ids[..3]);
        assert!(matches!(app.mode, Mode::Graph));
        app.open_selected_diff(&ids[1..2]);
        assert!(matches!(&app.mode,Mode::Diff(v) if v.changes.new==ids[1]));
        app.close_diff();
        app.history.commits[0].commit_time = 20;
        app.history.commits[2].commit_time = 10;
        app.open_selected_diff(&[ids[0], ids[2]]);
        assert!(
            matches!(&app.mode,Mode::Diff(v) if v.changes.old==Some(ids[2]) && v.changes.new==ids[0])
        );
        app.close_diff();
        app.history.commits[2].commit_time = 20;
        app.open_selected_diff(&[ids[0], ids[2]]);
        assert!(
            matches!(&app.mode,Mode::Diff(v) if v.changes.old==Some(ids[2]) && v.changes.new==ids[0])
        );
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
    fn checkout_uses_selected_endpoint_even_when_details_focus_is_elsewhere() {
        let mut app = app_with(10);
        let selected = app.history.commits[0].id;
        app.select(1);
        app.confirm_commit_checkout(selected);
        assert!(
            matches!(app.checkout_confirmation.as_ref().unwrap().chosen(),
            crate::checkout::Request::Commit { id, .. } if *id == selected)
        );
        assert!(
            app.checkout_request.is_none(),
            "opening the menu choice must not perform checkout"
        );
    }

    #[test]
    fn hidden_diff_finishes_loading_and_resumes_without_recomputation() {
        use crate::diff::{ChangeKind, FileChange, TreeDiff};
        let mut app = app_with(10);
        app.open_parent_diff(app.history.commits[0].id);
        let id = app.diff_view_mut().unwrap().id;
        let key = app.diff_view_mut().unwrap().changes.key;
        app.park_diff();
        app.park_diff(); // Switching between other tabs must not drop it.
        assert!(matches!(app.mode, Mode::Graph));
        assert!(
            matches!(app.pending_requests().as_slice(), [DiffReq::Tree { key: k, .. }] if *k == key)
        );
        assert!(app.changes_by_key_mut(key).is_some());
        app.diff_view_mut().unwrap().set_tree(Ok(TreeDiff {
            files: vec![FileChange {
                worktree: false,
                remote: None,
                path: "a.rs".into(),
                old_path: None,
                kind: ChangeKind::Added,
                old: None,
                new: None,
            }],
        }));
        let request = app.pending_requests();
        let [
            DiffReq::File {
                view, seq, file, ..
            },
        ] = request.as_slice()
        else {
            panic!("hidden tab must request its file")
        };
        assert_eq!(*view, id);
        app.diff_view_mut().unwrap().set_file_diff(
            *seq,
            *file,
            Ok(crate::diff::sidebyside::build(b"", b"hello\n")),
        );
        app.diff_view_mut().unwrap().scroll = 17;
        app.resume_diff();
        let Mode::Diff(v) = &app.mode else {
            panic!("expected resumed diff")
        };
        assert_eq!(v.id, id);
        assert_eq!(v.scroll, 17);
        assert!(v.diff.is_some());
        assert!(app.pending_requests().is_empty());
        app.park_diff();
        app.open_parent_diff(app.history.commits[1].id);
        assert!(app.parked_diff.is_none());
        assert_ne!(app.diff_view_mut().unwrap().id, id);
        assert!(
            app.changes_by_key_mut(key).is_none(),
            "discard superseded worker results"
        );
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
            worktree: false,
            remote: None,
            path: p.to_string(),
            old_path: None,
            kind: ChangeKind::Added,
            old: None,
            new: None,
        };
        app.changes_by_key_mut(cs_key)
            .unwrap()
            .set_tree(Ok(TreeDiff {
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
        let Mode::Diff(v) = &app.mode else {
            panic!("expected diff view")
        };
        assert_eq!(v.changes.key, cs_key, "change set reused, no recomputation");
        assert_eq!(v.current, Some(1), "opened at b.rs");
        assert!(app.details_changes.is_none());
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(
            app.details_changes.as_ref().map(|c| c.key),
            Some(cs_key),
            "handed back"
        );
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
        assert_eq!(
            app.selected_id(),
            Some(keep),
            "selection follows the commit id"
        );
    }
}
