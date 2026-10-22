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
    Select(Stash),
    CopySha(Stash),
    File(Stash, usize),
}

pub(super) struct State {
    pub open: bool,
    entries: Vec<Stash>,
    selected: Option<String>,
    detail_oid: Option<String>,
    detail_text: Option<Entity<TextBlock>>,
    files_scroll: UniformListScrollHandle,
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
            selected: None,
            detail_oid: None,
            detail_text: None,
            files_scroll: UniformListScrollHandle::new(),
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
    pub(super) fn stash_selected_oid(&self) -> Option<&str> {
        self.stashes.selected.as_deref()
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
                    if v.stashes.contains_identity(&stash) {
                        v.stashes.previews.insert(stash.oid, result);
                        v.sync_stash_details(cx);
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
            Action::CopySha(stash) => {
                if self.stashes.contains_identity(&stash) {
                    cx.write_to_clipboard(ClipboardItem::new_string(stash.oid));
                }
            }
            Action::Select(stash) => {
                if self.stashes.contains_identity(&stash) {
                    self.stashes.selected = Some(stash.oid.clone());
                    self.stashes
                        .files_scroll
                        .scroll_to_item(0, ScrollStrategy::Top);
                    self.load_stash_preview(stash, cx);
                }
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
        self.sync_stash_details(cx);
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
        self.action(super::Action::ForceRefresh, window, cx);
        assert!(
            self.stashes.want_refresh,
            "global refresh must invalidate stashes"
        );
        self.request_stashes();
        assert!(self.stashes.requested && self.engine.operation_busy);
        assert!(!self.stashes.want_refresh);
        self.action(super::Action::ForceRefresh, window, cx);
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
                if self
                    .stashes
                    .selected
                    .as_ref()
                    .is_none_or(|id| !ids.contains(id))
                {
                    self.stashes.selected = entries.first().map(|stash| stash.oid.clone());
                }
                self.stashes.previews.retain(|id, _| ids.contains(id));
                if self
                    .stashes
                    .delete_armed
                    .as_ref()
                    .is_some_and(|stash| !entries.contains(stash))
                {
                    self.stashes.delete_armed = None;
                }
                self.stashes.received = true;
                if let Some(stash) = self
                    .stashes
                    .entries
                    .iter()
                    .find(|stash| Some(&stash.oid) == self.stashes.selected.as_ref())
                    .cloned()
                {
                    self.load_stash_preview(stash, cx);
                }
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
        self.sync_stash_details(cx);
        if !self.stashes.requested
            && self.stashes.open
            && (self.stashes.want_refresh
                || self.stashes.status_refresh
                || self.stashes.refresh_after)
        {
            self.request_stashes();
        }
    }
    fn sync_stash_details(&mut self, cx: &mut Context<Self>) {
        let selected = self.stashes.selected.clone();
        if self.stashes.detail_oid == selected {
            return;
        }
        self.stashes.detail_oid = None;
        self.stashes.detail_text = None;
        let Some(oid) = selected else {
            return;
        };
        let Some(Ok(preview)) = self.stashes.previews.get(&oid) else {
            return;
        };
        let document = crate::desktop_details::document(&preview.details);
        let text = cx.new(TextBlock::new);
        text.update(cx, |text, cx| text.set_document(document, "", cx));
        self.stashes.detail_text = Some(text);
        self.stashes.detail_oid = Some(oid);
    }
    fn stash_inspector(&self, enabled: bool, cx: &mut Context<Self>) -> AnyElement {
        let stash = self
            .stashes
            .entries
            .iter()
            .find(|stash| Some(&stash.oid) == self.stashes.selected.as_ref());
        let mut panel = column()
            .relative()
            .w(px(self.pane_width))
            .flex_shrink_0()
            .h_full()
            .min_h_0()
            .border_l_1()
            .border_color(rgb(INSPECTOR_LINE))
            .children(self.graph_feature_probe(0, 9));
        let Some(stash) = stash else {
            return panel
                .child(caption("Select a stash to inspect its details").p_4())
                .into_any_element();
        };
        let apply = stash.clone();
        let pop = stash.clone();
        let delete = stash.clone();
        let armed = self.stashes.delete_armed.as_ref() == Some(stash);
        let preview = self.stashes.previews.get(&stash.oid);
        let copy = stash.clone();
        let oid = gix::ObjectId::from_hex(stash.oid.as_bytes()).ok();
        let author = preview
            .and_then(|result| result.as_ref().ok())
            .map(|preview| preview.details.author_name.as_str())
            .unwrap_or("?");
        panel = panel
            .child(
                row()
                    .h(px(44.))
                    .flex_shrink_0()
                    .px_4()
                    .border_b_1()
                    .border_color(rgb(INSPECTOR_LINE))
                    .child(
                        div()
                            .flex_1()
                            .child(caption("STASH DETAILS").text_color(rgb(INSPECTOR_MUTED))),
                    )
                    .child(
                        div()
                            .relative()
                            .children(self.graph_feature_probe(0, 10))
                            .child(requests::avatar(
                                oid.as_ref().and_then(|id| self.avatars.image(id)),
                                author,
                                28.,
                            )),
                    )
                    .child(
                        div()
                            .font_family("Cascadia Mono")
                            .text_size(px(11.))
                            .text_color(rgb(INSPECTOR_MUTED))
                            .child(stash.oid.chars().take(8).collect::<String>()),
                    )
                    .child(
                        div()
                            .relative()
                            .children(self.graph_feature_probe(0, 11))
                            .child(
                                Button::new("stash-copy-sha")
                                    .small()
                                    .ghost()
                                    .icon(gpui_component::Icon::default().path("rustle/copy"))
                                    .tooltip("Copy stash SHA")
                                    .on_click(cx.listener(move |v, _, w, cx| {
                                        v.stash_action(Action::CopySha(copy.clone()), w, cx)
                                    })),
                            ),
                    ),
            )
            .child(
                row()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .flex_shrink_0()
                    .child(
                        Button::new("selected-stash-apply")
                            .small()
                            .ghost()
                            .label("Apply…")
                            .disabled(!enabled)
                            .tooltip("Apply changes and keep this stash")
                            .on_click(cx.listener(move |v, _, w, cx| {
                                v.stash_action(Action::Apply(apply.clone()), w, cx)
                            })),
                    )
                    .child(
                        Button::new("selected-stash-pop")
                            .small()
                            .ghost()
                            .label("Pop…")
                            .disabled(!enabled)
                            .tooltip("Remove the stash only after changes apply successfully")
                            .on_click(cx.listener(move |v, _, w, cx| {
                                v.stash_action(Action::Pop(pop.clone()), w, cx)
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("selected-stash-delete")
                            .small()
                            .ghost()
                            .icon(gpui_component::Icon::default().path("rustle/trash"))
                            .disabled(!enabled)
                            .tooltip(if armed {
                                "Confirm deleting this stash"
                            } else {
                                "Delete stash — two clicks"
                            })
                            .when(armed, |s| s.text_color(rgb(ACCENT)))
                            .on_click(cx.listener(move |v, _, w, cx| {
                                v.stash_action(Action::Delete(delete.clone()), w, cx)
                            })),
                    ),
            )
            .when(armed, |s| {
                s.child(
                    caption("Click trash again to delete this stash")
                        .px_4()
                        .pb_2(),
                )
            })
            .child(caption(stash.selector.clone()).px_4().pb_2())
            .child(
                div()
                    .h(px(240.))
                    .flex_shrink_0()
                    .min_h_0()
                    .px_4()
                    .py_3()
                    .when_some(self.stashes.detail_text.as_ref(), |s, text| {
                        s.child(text.clone())
                    })
                    .when(self.stashes.detail_text.is_none(), |s| {
                        s.child(caption("Loading stash details…"))
                    }),
            )
            .child(
                row()
                    .px_4()
                    .py_2()
                    .flex_shrink_0()
                    .border_t_1()
                    .border_b_1()
                    .border_color(rgb(INSPECTOR_LINE))
                    .child(caption("FILES")),
            );
        match preview {
            Some(Ok(preview)) if !preview.changes.files.is_empty() => {
                let oid = stash.oid.clone();
                let count = preview.changes.files.len();
                let list = uniform_list(
                    "stash-inspector-files",
                    count,
                    cx.processor(move |v, range: std::ops::Range<usize>, _, cx| {
                        let Some(stash) = v.stashes.entries.iter().find(|stash| stash.oid == oid)
                        else {
                            return Vec::new();
                        };
                        let Some(Ok(preview)) = v.stashes.previews.get(&oid) else {
                            return Vec::new();
                        };
                        range
                            .map(|file| {
                                let change = &preview.changes.files[file];
                                let open = stash.clone();
                                row()
                                    .relative()
                                    .id(("stash-inspector-file", file))
                                    .w_full()
                                    .h(px(30.))
                                    .px_3()
                                    .gap_2()
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(HOVER)))
                                    .children(v.graph_feature_probe(0, 8))
                                    .child(caption(change.kind.letter().to_string()))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .child(change.path.clone()),
                                    )
                                    .on_click(cx.listener(move |v, _, w, cx| {
                                        v.stash_action(Action::File(open.clone(), file), w, cx)
                                    }))
                                    .into_any_element()
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .size_full()
                .track_scroll(self.stashes.files_scroll.clone());
                panel = panel.child(self.scroll_region(
                    6,
                    list,
                    Scrollbar::vertical(&self.stashes.files_scroll),
                    cx,
                ));
            }
            Some(Ok(_)) => panel = panel.child(caption("No file changes").p_4()),
            Some(Err(error)) => panel = panel.child(caption(error.clone()).p_4()),
            None => panel = panel.child(caption("Loading stash details and files…").p_4()),
        }
        panel.into_any_element()
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
            self.stashes.entries.len(),
            cx.processor(move |v, range: std::ops::Range<usize>, _, cx| {
                range
                    .map(|row_index| {
                        let stash = &v.stashes.entries[row_index];
                        let select = stash.clone();
                        let selected = v.stashes.selected.as_deref() == Some(&stash.oid);
                        let line = row()
                            .id(("stash-row", row_index))
                            .relative()
                            .w_full()
                            .h(px(40.))
                            .px_3()
                            .gap_2()
                            .children(v.todo_stash_probe(row_index, 0))
                            .children(v.todo_stash_probe(row_index, 1))
                            .children(v.todo_stash_probe(row_index, 2))
                            .bg(rgb(if selected { SELECTED } else { BG }))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(HOVER)))
                            .child(icon("stash").text_color(rgb(if selected {
                                ACCENT
                            } else {
                                MUTED
                            })))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(stash.subject.clone()),
                            )
                            .child(caption(stash.selector.clone()).flex_shrink_0())
                            .child(
                                caption(stash.oid.chars().take(7).collect::<String>())
                                    .flex_shrink_0(),
                            )
                            .on_click(cx.listener(move |v, _, w, cx| {
                                v.stash_action(Action::Select(select.clone()), w, cx)
                            }));
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
                        )
                        .child(self.stash_inspector(enabled, cx)),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Stash, State};

    #[test]
    fn stash_selection_keeps_a_single_row_per_entry_and_frozen_identity() {
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
        state.selected = Some(stash.oid.clone());
        assert_eq!(state.entries.len(), 1);
        assert!(state.contains_identity(&stash));
        assert!(state.previews.is_empty());
        state.entries[0].selector = "stash@{1}".into();
        assert_eq!(state.selected.as_ref(), Some(&state.entries[0].oid));
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
