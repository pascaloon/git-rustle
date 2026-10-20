//! Lazy graph stash sidebar; all Git access uses the serialized worker.
use super::*;
use crate::git::operations::{Request, stashes::Stash};

#[derive(Clone)]
pub(super) enum Action {
    Toggle,
    Refresh,
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
    want_refresh: bool,
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
            want_refresh: false,
            scroll: UniformListScrollHandle::new(),
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
            && !self.stashes.refresh_after
            && !self.engine.operation_busy
    }
    pub(super) fn stash_mutation_started(&mut self) {
        self.stashes.refresh_after = true;
        self.stashes.received = false;
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
                    self.request_stashes();
                }
            }
            Action::Refresh => self.request_stashes(),
            Action::Apply(stash) => {
                self.operation_action(
                    operations::Action::Run(Request::ApplyStash(stash)),
                    window,
                    cx,
                );
            }
            Action::Delete(stash) => {
                self.operation_action(
                    operations::Action::Run(Request::DropStash(stash)),
                    window,
                    cx,
                );
            }
        }
        cx.notify();
    }

    fn request_stashes(&mut self) {
        self.stashes.want_refresh = true;
        if !self.external_merge_busy()
            && !self.stashes.requested
            && self.engine.request_operation(Request::ListStashes)
        {
            self.stashes.want_refresh = false;
            self.stashes.requested = true;
            self.stashes.received = false;
            self.stashes.epoch = self.engine.operation_epoch;
            self.engine.tick(None);
        }
    }

    pub(super) fn sync_stashes(&mut self, cx: &mut Context<Self>) {
        if self.stashes.requested && self.stashes.epoch != self.engine.operation_epoch {
            self.stashes.requested = false;
            self.stashes.refresh_after = false;
            if let Some(entries) = &self.engine.operations.stashes {
                self.stashes.entries = entries.clone();
                self.stashes.received = true;
            }
            cx.notify();
        }
        if !self.stashes.requested
            && self.stashes.open
            && (self.stashes.want_refresh || self.stashes.refresh_after)
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
                            .h(px(112.))
                            .px_3()
                            .py_2()
                            .gap_2()
                            .border_b_1()
                            .border_color(rgb(LINE))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgb(MUTED))
                                    .child(stash.selector.clone()),
                            )
                            .child(
                                div()
                                    .id(("stash-subject", i))
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
                                    .gap_2()
                                    .child(
                                        Button::new(("apply-stash", i))
                                            .small()
                                            .label("Apply…")
                                            .disabled(!enabled)
                                            .on_click(cx.listener(move |v, _, w, cx| {
                                                v.stash_action(Action::Apply(apply.clone()), w, cx)
                                            })),
                                    )
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
        .flex_1()
        .min_h_0()
        .track_scroll(self.stashes.scroll.clone());
        Some(
            column()
                .relative()
                .children(self.todo_probe(2))
                .w(px(320.))
                .h_full()
                .flex_shrink_0()
                .border_r_1()
                .border_color(rgb(LINE))
                .child(
                    row()
                        .h(px(40.))
                        .px_3()
                        .gap_2()
                        .child("Stashes")
                        .child(div().flex_1())
                        .child(
                            Button::new("refresh-stashes")
                                .xsmall()
                                .ghost()
                                .label("Refresh")
                                .disabled(
                                    self.external_merge_busy()
                                        || self.stashes.requested
                                        || self.engine.operation_busy,
                                )
                                .on_click(cx.listener(|v, _, w, cx| {
                                    v.stash_action(Action::Refresh, w, cx)
                                })),
                        ),
                )
                .when(self.stashes.entries.is_empty(), |s| {
                    s.child(
                        caption(if self.stashes.requested {
                            "Loading stashes…"
                        } else {
                            "No stashes"
                        })
                        .p_3(),
                    )
                })
                .child(row().flex_1().min_h_0().child(list).child(
                    Scrollbar::vertical(&self.stashes.scroll).scrollbar_show(ScrollbarShow::Always),
                ))
                .into_any_element(),
        )
    }
}
