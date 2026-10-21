//! Lazy stash sidebar. Git access stays on the serialized worker.
use super::*;
use crate::git::operations::{Request, stashes::Stash};

#[derive(Clone)]
pub(super) enum Action {
    Toggle,
    Create,
    Save,
    Cancel,
    Apply(Stash),
    Delete(Stash),
}

pub(super) struct State {
    pub open: bool,
    entries: Vec<Stash>,
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
    pub(super) fn stash_action(
        &mut self,
        action: Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            Action::Toggle => {
                self.stashes.open = !self.stashes.open;
                if self.stashes.open {
                    self.invalidate_stashes();
                    self.request_stashes();
                }
            }
            Action::Create => {
                self.stashes.open = true;
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
            Action::Delete(stash) => self.operation_action(
                operations::Action::Run(Request::DropStash(stash)),
                window,
                cx,
            ),
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
    pub(super) fn sync_stashes(&mut self, cx: &mut Context<Self>) {
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
            self.stashes.entries.len(),
            cx.processor(move |v, range: std::ops::Range<usize>, _, cx| {
                range
                    .map(|i| {
                        let stash = &v.stashes.entries[i];
                        let apply = stash.clone();
                        let delete = stash.clone();
                        column()
                            .id(("stash", i))
                            .w_full()
                            .relative()
                            .children(v.todo_stash_probe(i, 2))
                            .h(px(112.))
                            .px_3()
                            .py_2()
                            .gap_2()
                            .border_b_1()
                            .border_color(rgb(LINE))
                            .child(
                                row()
                                    .gap_2()
                                    .child(caption(stash.selector.clone()))
                                    .child(div().flex_1())
                                    .child(caption(stash.oid.chars().take(7).collect::<String>())),
                            )
                            .child(
                                div()
                                    .id(("stash-subject", i))
                                    .relative()
                                    .children(v.todo_stash_probe(i, 0))
                                    .truncate()
                                    .child(stash.subject.clone())
                                    .tooltip({
                                        let text = format!("{}\n{}", stash.subject, stash.oid);
                                        move |w, cx| {
                                            gpui_component::tooltip::Tooltip::new(text.clone())
                                                .build(w, cx)
                                        }
                                    }),
                            )
                            .child(
                                row()
                                    .relative()
                                    .children(v.todo_stash_probe(i, 1))
                                    .gap_2()
                                    .child(
                                        Button::new(("apply-stash", i))
                                            .small()
                                            .label("Apply…")
                                            .disabled(!enabled)
                                            .tooltip("Restore changes and keep this stash")
                                            .on_click(cx.listener(move |v, _, w, cx| {
                                                v.stash_action(Action::Apply(apply.clone()), w, cx)
                                            })),
                                    )
                                    .child(div().flex_1())
                                    .child(
                                        Button::new(("delete-stash", i))
                                            .small()
                                            .ghost()
                                            .label("Delete…")
                                            .disabled(!enabled)
                                            .on_click(cx.listener(move |v, _, w, cx| {
                                                v.stash_action(
                                                    Action::Delete(delete.clone()),
                                                    w,
                                                    cx,
                                                )
                                            })),
                                    ),
                            )
                            .into_any_element()
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
                .w(px(340.))
                .h_full()
                .flex_shrink_0()
                .border_r_1()
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
                    caption("Saved changes · Apply keeps the stash")
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
    use super::State;

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
