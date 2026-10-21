//! Lazy stash tab. Git access stays on the serialized worker.
use super::*;
use crate::git::operations::{Request, stashes::Stash};
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
pub(super) enum Action {
    Open,
    Toggle,
    Create,
    Save,
    Cancel,
    Apply(Stash),
    Pop(Stash),
    Delete(Stash),
    Expand(Stash),
    File(Stash, usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StashRow {
    Header(usize),
    Actions(usize),
    File(usize, usize),
    Status(usize),
}

pub(super) struct State {
    pub open: bool,
    entries: Vec<Stash>,
    rows: Vec<StashRow>,
    expanded: HashSet<String>,
    previews: HashMap<String, Result<crate::git::operations::stashes::Preview, String>>,
    inflight: HashSet<String>,
    delete_armed: Option<Stash>,
    repository: String,
    session: u64,
    apply_epoch: Option<u64>,
    apply_succeeded: Option<bool>,
    requested: bool,
    received: bool,
    refresh_after: bool,
    epoch: u64,
    operation_epoch: u64,
    revision: u64,
    want_refresh: bool,
    status_refresh: bool,
    listed_stamp: Option<u64>,
    error: Option<String>,
    pub(super) name: Option<Entity<InputState>>,
    subscription: Option<Subscription>,
    pub(super) composing: bool,
    scroll: UniformListScrollHandle,
}
impl Default for State {
    fn default() -> Self {
        Self {
            open: false,
            entries: Vec::new(),
            rows: Vec::new(),
            expanded: HashSet::new(),
            previews: HashMap::new(),
            inflight: HashSet::new(),
            delete_armed: None,
            repository: String::new(),
            session: 0,
            apply_epoch: None,
            apply_succeeded: None,
            requested: false,
            received: false,
            refresh_after: false,
            epoch: 0,
            operation_epoch: 0,
            revision: 0,
            want_refresh: false,
            status_refresh: false,
            listed_stamp: None,
            error: None,
            name: None,
            subscription: None,
            composing: false,
            scroll: UniformListScrollHandle::new(),
        }
    }
}

impl State {
    fn contains_identity(&self, frozen: &Stash) -> bool {
        self.entries.iter().any(|entry| {
            entry.oid == frozen.oid && entry.subject == frozen.subject && entry.time == frozen.time
        })
    }
    fn rebuild_rows(&mut self) {
        self.rows.clear();
        for (i, stash) in self.entries.iter().enumerate() {
            self.rows.push(StashRow::Header(i));
            self.rows.push(StashRow::Actions(i));
            if self.expanded.contains(&stash.oid) {
                match self.previews.get(&stash.oid) {
                    Some(Ok(preview)) if !preview.changes.files.is_empty() => {
                        self.rows.extend(
                            (0..preview.changes.files.len()).map(|file| StashRow::File(i, file)),
                        );
                    }
                    _ => self.rows.push(StashRow::Status(i)),
                }
            }
        }
    }
    fn coalesce_status_refresh(&mut self, observed: Option<u64>) {
        if self.listed_stamp.is_some() && self.listed_stamp == observed {
            self.status_refresh = false;
        }
    }
}
impl Desktop {
    pub(super) fn stash_entries(&self) -> &[Stash] {
        &self.stashes.entries
    }
    pub(super) fn stash_preview_file_count(&self, stash: &Stash) -> Option<usize> {
        self.stashes
            .previews
            .get(&stash.oid)
            .and_then(|preview| preview.as_ref().ok())
            .map(|preview| preview.changes.files.len())
    }
    pub(super) fn stashes_ready(&self) -> bool {
        self.stashes.open
            && self.stashes.received
            && !self.stashes.requested
            && !self.stashes.want_refresh
            && !self.stashes.status_refresh
            && !self.stashes.refresh_after
            && !self.engine.operation_busy
    }
    pub(super) fn invalidate_stashes(&mut self) {
        self.stashes.want_refresh = true;
    }
    pub(super) fn stash_mutation_started(&mut self) {
        self.stashes.refresh_after = true;
    }
    pub(super) fn stash_apply_started(&mut self) {
        self.stashes.apply_epoch = Some(self.engine.operation_epoch);
        self.stashes.apply_succeeded = None;
    }

    fn load_stash_preview(&mut self, stash: Stash, cx: &mut Context<Self>) {
        if self.stashes.previews.contains_key(&stash.oid)
            || !self.stashes.inflight.insert(stash.oid.clone())
        {
            return;
        }
        let repository = self.engine.repository_path();
        let session = self.stashes.session;
        let frozen = stash.clone();
        let path = std::path::PathBuf::from(&repository);
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(crate::git::operations::stashes::preview(&path, &frozen));
        });
        cx.spawn(async move |view, cx| {
            if let Ok(result) = rx.recv().await {
                let _ = view.update(cx, |v, cx| {
                    if v.stashes.session != session || v.engine.repository_path() != repository {
                        return;
                    }
                    v.stashes.inflight.remove(&stash.oid);
                    if v.stashes.contains_identity(&stash)
                        && v.stashes.expanded.contains(&stash.oid)
                    {
                        v.stashes.previews.insert(stash.oid, result);
                        v.stashes.rebuild_rows();
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }
    pub(super) fn stash_action(
        &mut self,
        action: Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(action, Action::Delete(_)) {
            self.stashes.delete_armed = None;
        }
        match action {
            Action::Open => {
                self.stashes.open = true;
                self.invalidate_stashes();
                self.request_stashes();
            }
            Action::Toggle => {
                self.action(
                    super::Action::Tab(if self.stashes.open {
                        Tab::Graph
                    } else {
                        Tab::Stashes
                    }),
                    window,
                    cx,
                );
            }
            Action::Create => {
                if !self.stashes.open {
                    self.action(super::Action::Tab(Tab::Stashes), window, cx);
                }
                self.stashes.composing = true;
                if self.stashes.name.is_none() {
                    let name = cx
                        .new(|cx| InputState::new(window, cx).placeholder("Stash name (optional)"));
                    self.stashes.subscription =
                        Some(cx.subscribe(&name, |_, _, _: &InputEvent, cx| cx.notify()));
                    self.stashes.name = Some(name);
                }
                if let Some(name) = &self.stashes.name {
                    name.update(cx, |name, cx| name.focus(window, cx));
                }
                if !self.stashes.received {
                    self.invalidate_stashes();
                    self.request_stashes();
                }
            }
            Action::Cancel => self.stashes.composing = false,
            Action::Save => {
                let message = self
                    .stashes
                    .name
                    .as_ref()
                    .map(|name| name.read(cx).value().to_string());
                if self.engine.request_operation(Request::CreateStash(message)) {
                    self.stash_mutation_started();
                    self.stashes.composing = false;
                    if let Some(name) = &self.stashes.name {
                        name.update(cx, |name, cx| name.set_value("", window, cx));
                    }
                    self.engine.tick(None);
                }
            }
            Action::Apply(stash) => self.operation_action(
                operations::Action::Run(Request::ApplyStash(stash)),
                window,
                cx,
            ),
            Action::Pop(stash) => self.operation_action(
                operations::Action::Run(Request::PopStash(stash)),
                window,
                cx,
            ),
            Action::Delete(stash) => {
                if self.stashes.delete_armed.as_ref() == Some(&stash) {
                    self.stashes.delete_armed = None;
                    if self.engine.request_operation(Request::DropStash(stash)) {
                        self.stash_mutation_started();
                        self.engine.tick(None);
                    }
                } else {
                    self.stashes.delete_armed = Some(stash);
                }
            }
            Action::Expand(stash) => {
                self.stashes.delete_armed = None;
                if !self.stashes.expanded.remove(&stash.oid) {
                    self.stashes.expanded.insert(stash.oid.clone());
                    self.load_stash_preview(stash, cx);
                } else {
                    self.stashes.previews.remove(&stash.oid);
                }
                self.stashes.rebuild_rows();
            }
            Action::File(stash, file) => {
                if let Some(Ok(preview)) = self.stashes.previews.get(&stash.oid).cloned()
                    && self.stashes.entries.contains(&stash)
                    && file < preview.changes.files.len()
                {
                    self.engine.app.open_commit_pair(preview.base, preview.oid);
                    if let Some(diff) = self.engine.app.diff_view_mut() {
                        diff.changes.set_tree(Ok(preview.changes));
                        diff.changes.requested = true;
                        diff.show(Some(file));
                    }
                    self.stashes.open = false;
                    self.operations.tab = None;
                    self.engine.tick(None);
                }
            }
        }
        cx.notify();
    }
    fn request_stashes(&mut self) {
        if !self.external_merge_busy()
            && !self.stashes.requested
            && self.engine.request_operation(Request::ListStashes)
        {
            self.stashes.want_refresh = false;
            self.stashes.status_refresh = false;
            self.stashes.refresh_after = false;
            self.stashes.requested = true;
            self.stashes.error = None;
            self.stashes.epoch = self.engine.stash_epoch;
            self.stashes.operation_epoch = self.engine.operation_epoch;
            self.engine.tick(None);
        }
    }
    /// Native scenario uses the real global action while a list is in flight.
    pub(super) fn stash_scenario_refresh_during_list(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> u64 {
        let epoch = self.engine.stash_epoch;
        self.action(super::Action::Refresh, window, cx);
        assert!(
            self.stashes.want_refresh,
            "global refresh must invalidate stashes"
        );
        self.request_stashes();
        assert!(self.stashes.requested && self.engine.operation_busy);
        assert!(!self.stashes.want_refresh);
        self.action(super::Action::Refresh, window, cx);
        assert!(
            self.stashes.requested && self.stashes.want_refresh,
            "explicit refresh during listing must remain queued"
        );
        epoch
    }
    pub(super) fn sync_stashes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let repository = self.engine.repository_path();
        if self.stashes.repository.is_empty() {
            self.stashes.repository = repository.clone();
        } else if self.stashes.repository != repository {
            let session = self.stashes.session.wrapping_add(1);
            self.stashes = State {
                repository,
                session,
                ..State::default()
            };
        }
        if self.stashes.apply_succeeded.is_none()
            && self
                .stashes
                .apply_epoch
                .is_some_and(|epoch| epoch != self.engine.operation_epoch)
            && !self.engine.operation_busy
        {
            self.stashes.apply_succeeded =
                Some(!self.engine.operation_error && self.engine.operations.conflict.is_none());
        }
        if self.stashes.apply_succeeded == Some(false) {
            self.stashes.apply_epoch = None;
            self.stashes.apply_succeeded = None;
        }
        if self.stashes.revision != self.engine.stash_revision {
            self.stashes.revision = self.engine.stash_revision;
            self.stashes.status_refresh = true;
            self.stashes
                .coalesce_status_refresh(self.engine.stash_status_stamp());
        }
        // Other operations and recovery views also advance operation_epoch. Only
        // a successfully delivered stash list may mark this request as received.
        if self.stashes.requested && self.stashes.epoch != self.engine.stash_epoch {
            self.stashes.requested = false;
            self.stashes.listed_stamp = self.engine.stash_list_stamp;
            self.stashes
                .coalesce_status_refresh(self.engine.stash_status_stamp());
            if let Some(entries) = &self.engine.operations.stashes {
                self.stashes.entries = entries.clone();
                let ids: HashSet<_> = entries.iter().map(|stash| stash.oid.clone()).collect();
                self.stashes.expanded.retain(|id| ids.contains(id));
                self.stashes.previews.retain(|id, _| ids.contains(id));
                if self
                    .stashes
                    .delete_armed
                    .as_ref()
                    .is_some_and(|stash| !entries.contains(stash))
                {
                    self.stashes.delete_armed = None;
                }
                self.stashes.rebuild_rows();
                self.stashes.received = true;
            }
            cx.notify();
        } else if self.stashes.requested
            && !self.engine.operation_busy
            && self.stashes.operation_epoch != self.engine.operation_epoch
            && self.engine.operation_error
        {
            self.stashes.requested = false;
            self.stashes.error = Some(self.engine.operation_status.clone());
            cx.notify();
        }
        if self.stashes.apply_succeeded == Some(true)
            && !self.engine.operation_busy
            && !self.engine.app.pending.loading
            && !self.engine.app.pending.want_refresh
            && self.engine.app.pending.known
            && !self.stashes.requested
            && !self.stashes.refresh_after
        {
            self.stashes.apply_epoch = None;
            self.stashes.apply_succeeded = None;
            if self.engine.operations.conflict.is_none() {
                self.action(super::Action::Tab(Tab::Graph), window, cx);
                self.action(super::Action::Head, window, cx);
            }
        }
        if !self.stashes.requested
            && self.stashes.open
            && (self.stashes.want_refresh
                || self.stashes.status_refresh
                || self.stashes.refresh_after)
        {
            self.request_stashes();
        }
    }
    pub(super) fn stash_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.stashes.open {
            return None;
        }
        let enabled = !self.external_merge_busy()
            && !self.engine.operation_busy
            && !self.engine.app.pending.busy
            && !self.engine.app.branches.busy
            && !self.engine.app.settings.saving
            && self.engine.operations.conflict.is_none();
        let can_create = enabled
            && self.engine.app.pending.known
            && !self.engine.app.pending.snapshot.files.is_empty();
        let list = uniform_list(
            "stash-list",
            self.stashes.rows.len(),
            cx.processor(move |v, range: std::ops::Range<usize>, _, cx| {
                range
                    .map(|row_index| {
                        let kind = v.stashes.rows[row_index];
                        let i = match kind {
                            StashRow::Header(i)
                            | StashRow::Actions(i)
                            | StashRow::File(i, _)
                            | StashRow::Status(i) => i,
                        };
                        let stash = &v.stashes.entries[i];
                        let mut line = row()
                            .id(("stash-row", row_index))
                            .relative()
                            .w_full()
                            .h(px(36.))
                            .px_3()
                            .gap_2();
                        match kind {
                            StashRow::Header(_) => {
                                let expand = stash.clone();
                                line = line
                                    .children(v.todo_stash_probe(i, 2))
                                    .children(v.todo_stash_probe(i, 0))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(HOVER)))
                                    .child(if v.stashes.expanded.contains(&stash.oid) {
                                        "▾"
                                    } else {
                                        "▸"
                                    })
                                    .child(icon("stash"))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .child(stash.subject.clone()),
                                    )
                                    .child(caption(stash.selector.clone()))
                                    .child(caption(stash.oid.chars().take(7).collect::<String>()))
                                    .on_click(cx.listener(move |v, _, w, cx| {
                                        v.stash_action(Action::Expand(expand.clone()), w, cx)
                                    }));
                            }
                            StashRow::Actions(_) => {
                                let apply = stash.clone();
                                let pop = stash.clone();
                                let delete = stash.clone();
                                let armed = v.stashes.delete_armed.as_ref() == Some(stash);
                                line = line
                                    .children(v.todo_stash_probe(i, 1))
                                    .pl(px(34.))
                                    .border_b_1()
                                    .border_color(rgb(LINE))
                                    .child(
                                        Button::new(("apply-stash", i))
                                            .xsmall()
                                            .ghost()
                                            .label("Apply…")
                                            .disabled(!enabled)
                                            .tooltip("Apply changes and keep this stash")
                                            .on_click(cx.listener(move |v, _, w, cx| {
                                                v.stash_action(Action::Apply(apply.clone()), w, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new(("pop-stash", i))
                                            .xsmall()
                                            .ghost()
                                            .label("Pop…")
                                            .disabled(!enabled)
                                            .tooltip("Apply changes; remove stash only on success")
                                            .on_click(cx.listener(move |v, _, w, cx| {
                                                v.stash_action(Action::Pop(pop.clone()), w, cx)
                                            })),
                                    )
                                    .child(div().flex_1())
                                    .when(armed, |s| {
                                        s.child(caption("Click trash again to delete"))
                                    })
                                    .child(
                                        Button::new(("delete-stash", i))
                                            .xsmall()
                                            .ghost()
                                            .child(icon("trash"))
                                            .when(armed, |s| s.text_color(rgb(ACCENT)))
                                            .disabled(!enabled)
                                            .tooltip(if armed {
                                                "Confirm deleting this stash"
                                            } else {
                                                "Delete stash — two clicks"
                                            })
                                            .on_click(cx.listener(move |v, _, w, cx| {
                                                v.stash_action(
                                                    Action::Delete(delete.clone()),
                                                    w,
                                                    cx,
                                                )
                                            })),
                                    );
                            }
                            StashRow::File(_, file) => {
                                let Some(Ok(preview)) = v.stashes.previews.get(&stash.oid) else {
                                    return line.into_any_element();
                                };
                                let change = &preview.changes.files[file];
                                let open = stash.clone();
                                line = line
                                    .children(v.graph_feature_probe(0, 8))
                                    .pl(px(42.))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(HOVER)))
                                    .child(caption(change.kind.letter().to_string()))
                                    .child(div().flex_1().truncate().child(change.path.clone()))
                                    .on_click(cx.listener(move |v, _, w, cx| {
                                        v.stash_action(Action::File(open.clone(), file), w, cx)
                                    }));
                            }
                            StashRow::Status(_) => {
                                let text = match v.stashes.previews.get(&stash.oid) {
                                    Some(Err(error)) => error.clone(),
                                    Some(Ok(_)) => "No file changes".into(),
                                    None => "Loading stash files…".into(),
                                };
                                line = line.pl(px(42.)).child(caption(text));
                            }
                        }
                        line.into_any_element()
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .size_full()
        .track_scroll(self.stashes.scroll.clone());
        Some(
            column()
                .relative()
                .children(self.todo_probe(2))
                .w_full()
                .h_full()
                .flex_1()
                .border_color(rgb(LINE))
                .child(
                    row()
                        .h(px(44.))
                        .px_3()
                        .gap_2()
                        .child("Stashes")
                        .child(caption(self.stashes.entries.len().to_string()))
                        .child(div().flex_1())
                        .child(
                            Button::new("new-stash")
                                .xsmall()
                                .ghost()
                                .label("New stash")
                                .disabled(!can_create)
                                .on_click(
                                    cx.listener(|v, _, w, cx| {
                                        v.stash_action(Action::Create, w, cx)
                                    }),
                                ),
                        )
                        .child(
                            Button::new("close-stashes")
                                .xsmall()
                                .ghost()
                                .label("×")
                                .tooltip("Close stashes")
                                .on_click(
                                    cx.listener(|v, _, w, cx| {
                                        v.stash_action(Action::Toggle, w, cx)
                                    }),
                                ),
                        ),
                )
                .child(
                    caption("Saved changes · Apply keeps the stash · Pop removes it after success")
                        .px_3()
                        .pb_2(),
                )
                .when(self.stashes.composing, |s| {
                    s.child(
                        column()
                            .p_3()
                            .gap_2()
                            .child(
                                Input::new(self.stashes.name.as_ref().unwrap()).disabled(!enabled),
                            )
                            .child(caption("Includes staged, unstaged and untracked files."))
                            .child(
                                row()
                                    .gap_2()
                                    .child(
                                        Button::new("save-stash")
                                            .small()
                                            .label("Stash changes")
                                            .disabled(!can_create)
                                            .on_click(cx.listener(|v, _, w, cx| {
                                                v.stash_action(Action::Save, w, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("cancel-stash")
                                            .small()
                                            .ghost()
                                            .label("Cancel")
                                            .on_click(cx.listener(|v, _, w, cx| {
                                                v.stash_action(Action::Cancel, w, cx)
                                            })),
                                    ),
                            ),
                    )
                })
                .when_some(self.stashes.error.as_ref(), |s, error| {
                    s.child(caption(error.clone()).p_3())
                })
                .when(self.stashes.entries.is_empty(), |s| {
                    s.child(
                        caption(if self.stashes.requested {
                            "Loading stashes…"
                        } else if self.stashes.error.is_some() {
                            "Use global Refresh to try again."
                        } else {
                            "No saved changes yet"
                        })
                        .p_3(),
                    )
                })
                .child(
                    row()
                        .flex_1()
                        .min_h_0()
                        .gap_0()
                        .child(
                            div()
                                .relative()
                                .flex_1()
                                .min_w_0()
                                .h_full()
                                .children(self.todo_probe(14))
                                .child(list),
                        )
                        .child(
                            Scrollbar::vertical(&self.stashes.scroll)
                                .scrollbar_show(ScrollbarShow::Always),
                        ),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Stash, StashRow, State};

    #[test]
    fn collapsed_stash_rows_are_bounded_and_expansion_shows_loading() {
        let stash = Stash {
            oid: "a".repeat(40),
            selector: "stash@{0}".into(),
            subject: "Saved work".into(),
            time: "1".into(),
        };
        let mut state = State {
            entries: vec![stash.clone()],
            ..State::default()
        };
        state.rebuild_rows();
        assert_eq!(state.rows, vec![StashRow::Header(0), StashRow::Actions(0)]);
        state.expanded.insert(stash.oid.clone());
        state.rebuild_rows();
        assert_eq!(state.rows.last(), Some(&StashRow::Status(0)));
        state.expanded.clear();
        state.rebuild_rows();
        assert_eq!(state.rows.len(), 2);
        assert!(state.previews.is_empty());
    }

    #[test]
    fn preview_identity_survives_reflog_selector_shifts_but_rejects_replacements() {
        let frozen = Stash {
            oid: "a".repeat(40),
            selector: "stash@{0}".into(),
            subject: "Saved work".into(),
            time: "1".into(),
        };
        let mut shifted = frozen.clone();
        shifted.selector = "stash@{1}".into();
        let mut state = State {
            entries: vec![shifted],
            ..State::default()
        };
        assert!(state.contains_identity(&frozen));
        state.entries[0].oid = "b".repeat(40);
        assert!(!state.contains_identity(&frozen));
        state.entries[0] = frozen.clone();
        state.entries[0].subject = "Changed reflog identity".into();
        assert!(!state.contains_identity(&frozen));
        state.entries.clear();
        assert!(!state.contains_identity(&frozen));
    }

    #[test]
    fn matching_list_version_coalesces_status_but_preserves_explicit_refresh() {
        let mut state = State {
            status_refresh: true,
            want_refresh: true,
            refresh_after: true,
            listed_stamp: Some(7),
            ..State::default()
        };
        state.coalesce_status_refresh(Some(7));
        assert!(!state.status_refresh);
        assert!(
            state.want_refresh,
            "global refresh during listing must remain queued"
        );
        assert!(
            state.refresh_after,
            "a mutation must keep its own refresh reason"
        );
    }

    #[test]
    fn changed_or_unknown_metadata_never_coalesces_status_refresh() {
        for (listed, observed) in [
            (Some(7), Some(8)),
            (Some(7), None),
            (None, Some(7)),
            (None, None),
        ] {
            let mut state = State {
                status_refresh: true,
                listed_stamp: listed,
                ..State::default()
            };
            state.coalesce_status_refresh(observed);
            assert!(
                state.status_refresh,
                "concurrent or unknown metadata needs another read"
            );
        }
    }

    #[test]
    fn matching_status_delivered_after_list_uses_the_already_loaded_version() {
        let mut state = State {
            listed_stamp: Some(7),
            ..State::default()
        };
        state.status_refresh = true;
        state.coalesce_status_refresh(Some(7));
        assert!(!state.status_refresh);
        state.status_refresh = true;
        state.coalesce_status_refresh(Some(8));
        assert!(state.status_refresh, "a later external change must refresh");
    }
}
