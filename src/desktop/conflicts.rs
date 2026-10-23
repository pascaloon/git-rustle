//! Conflict workspace. Commit/file lists are virtualized; text drafts stay on the UI thread.
use super::*;
use crate::git::operations::{Conflict, Request, Resolution};
use std::collections::HashMap;

#[derive(Clone)]
struct Source {
    identity: String,
    path: String,
    original: String,
}
struct Draft {
    source: Source,
    text: String,
}
pub(super) struct State {
    editor: Entity<InputState>,
    identity: String,
    epoch: u64,
    selected: Option<String>,
    requested: Option<String>,
    source: Option<Source>,
    drafts: HashMap<String, Draft>,
    stale: bool,
    incoming: UniformListScrollHandle,
    local: UniformListScrollHandle,
    files: UniformListScrollHandle,
}
impl State {
    fn new(identity: String, editor: Entity<InputState>) -> Self {
        Self {
            editor,
            identity,
            epoch: u64::MAX,
            selected: None,
            requested: None,
            source: None,
            drafts: HashMap::new(),
            stale: false,
            incoming: UniformListScrollHandle::new(),
            local: UniformListScrollHandle::new(),
            files: UniformListScrollHandle::new(),
        }
    }
}

fn incoming_choice(conflict: &Conflict) -> Resolution {
    if conflict.kind == "Rebase" {
        Resolution::Ours
    } else {
        Resolution::Theirs
    }
}

fn side_preview(text: &str) -> String {
    let mut preview: String = text.chars().take(512).collect();
    if preview.len() < text.len() {
        preview.push('…');
    }
    preview
}

impl Desktop {
    fn dismiss_stale_conflict_confirmation(&mut self) {
        if self
            .operations
            .confirmation
            .as_ref()
            .is_some_and(|(request, _)| matches!(request, Request::OnConflict { .. }))
        {
            self.operations.confirmation = None;
        }
    }

    pub(super) fn bind_conflict_request(&self, request: Request) -> Request {
        if matches!(
            request,
            Request::Abort | Request::Skip | Request::Continue | Request::Resolve { .. }
        ) && let Some(conflict) = &self.engine.operations.conflict
        {
            Request::OnConflict {
                identity: conflict.identity.clone(),
                request: Box::new(request),
            }
        } else {
            request
        }
    }

    fn remember_conflict_draft(&mut self, cx: &Context<Self>) {
        let Some(state) = &mut self.operations.conflicts else {
            return;
        };
        let Some(source) = &state.source else {
            return;
        };
        let text = state.editor.read(cx).value().to_string();
        if text != source.original {
            state.drafts.insert(
                source.path.clone(),
                Draft {
                    source: source.clone(),
                    text,
                },
            );
        } else {
            state.drafts.remove(&source.path);
        }
    }

    pub(super) fn sync_conflicts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(conflict) = &self.engine.operations.conflict else {
            self.dismiss_stale_conflict_confirmation();
            self.operations.conflicts = None;
            return;
        };
        let first = self
            .operations
            .conflicts
            .as_ref()
            .is_none_or(|s| s.identity != conflict.identity);
        if first {
            let identity = conflict.identity.clone();
            self.dismiss_stale_conflict_confirmation();
            let editor = cx.new(|cx| InputState::new(window, cx).multi_line(true));
            self.operations.conflicts = Some(State::new(identity, editor));
            self.operations.tab = Some(Tab::Conflicts);
        }
        if self.operations.tab == Some(Tab::Conflicts)
            && (first || self.engine.app.inspected_id().is_none())
        {
            let conflict = self.engine.operations.conflict.as_ref().unwrap();
            let first_commit = conflict
                .incoming
                .first()
                .or_else(|| conflict.local.first())
                .map(|c| (c.id, c.parents.first().copied()));
            if let Some((id, parent)) = first_commit {
                self.select_conflict_commit(id, parent, window, cx);
            }
        }
        let Some(conflict) = self.engine.operations.conflict.as_ref() else {
            return;
        };
        let state = self.operations.conflicts.as_mut().unwrap();
        if state
            .selected
            .as_ref()
            .is_none_or(|path| !conflict.states.iter().any(|s| &s.path == path))
        {
            state.selected = conflict
                .states
                .iter()
                .find(|s| s.unmerged)
                .or_else(|| conflict.states.first())
                .map(|s| s.path.clone());
            state.requested = None;
        }
        let request = state
            .selected
            .as_ref()
            .filter(|path| {
                state.requested.as_ref() != Some(path)
                    && conflict.files.contains(path)
                    && !self.engine.operation_busy
            })
            .cloned();
        if let Some(path) = request
            && self
                .engine
                .request_operation(Request::ReadConflict(path.clone()))
        {
            self.operations.conflicts.as_mut().unwrap().requested = Some(path);
            self.engine.tick(None);
        }
        let state = self.operations.conflicts.as_mut().unwrap();
        if state.epoch == self.engine.operation_epoch {
            return;
        }
        state.epoch = self.engine.operation_epoch;
        let Some(preview) = &self.engine.operations.preview else {
            return;
        };
        if preview.identity != state.identity || state.selected.as_ref() != Some(&preview.path) {
            return;
        }
        let Some(original) = &preview.original else {
            let path = preview.path.clone();
            self.remember_conflict_draft(cx);
            let state = self.operations.conflicts.as_mut().unwrap();
            if let Some(draft) = state.drafts.get(&path) {
                state.source = Some(draft.source.clone());
                state
                    .editor
                    .update(cx, |s, cx| s.set_value(draft.text.clone(), window, cx));
                state.stale = true;
            } else {
                state.source = None;
                state.stale = false;
            }
            return;
        };
        if state.source.as_ref().is_some_and(|s| {
            s.identity == preview.identity && s.path == preview.path && &s.original == original
        }) {
            return;
        }
        let source = Source {
            identity: preview.identity.clone(),
            path: preview.path.clone(),
            original: original.clone(),
        };
        self.remember_conflict_draft(cx);
        let state = self.operations.conflicts.as_mut().unwrap();
        let (source, text, stale) = if let Some(draft) = state.drafts.get(&source.path) {
            (
                draft.source.clone(),
                draft.text.clone(),
                draft.source.original != source.original,
            )
        } else {
            let text = source.original.clone();
            (source, text, false)
        };
        state
            .editor
            .update(cx, |s, cx| s.set_value(text, window, cx));
        state.source = Some(source);
        state.stale = stale;
    }

    pub(super) fn select_conflict_file(
        &mut self,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Request> {
        self.remember_conflict_draft(cx);
        let state = self.operations.conflicts.as_mut()?;
        let file = self
            .engine
            .operations
            .conflict
            .as_ref()?
            .states
            .iter()
            .find(|s| s.path == path)?;
        state.selected = Some(path.clone());
        state.requested = (!self.engine.operation_busy).then(|| path.clone());
        state.source = None;
        state.stale = false;
        state.epoch = u64::MAX;
        if !file.unmerged
            && let Some(draft) = state.drafts.get(&path)
        {
            state.source = Some(draft.source.clone());
            state
                .editor
                .update(cx, |s, cx| s.set_value(draft.text.clone(), window, cx));
            state.stale = true;
        }
        (file.unmerged && !self.engine.operation_busy).then_some(Request::ReadConflict(path))
    }

    pub(super) fn reload_conflict_file(&mut self) -> Option<Request> {
        let state = self.operations.conflicts.as_mut()?;
        let path = state.selected.clone()?;
        state.drafts.remove(&path);
        state.source = None;
        state.stale = false;
        state.epoch = u64::MAX;
        state.requested = (!self.engine.operation_busy).then(|| path.clone());
        (!self.engine.operation_busy).then_some(Request::ReadConflict(path))
    }

    pub(super) fn defer_conflict_read(&mut self) {
        if let Some(state) = &mut self.operations.conflicts {
            state.requested = None;
        }
    }

    pub(super) fn conflict_save_request(&self, cx: &Context<Self>) -> Option<Request> {
        let state = self.operations.conflicts.as_ref()?;
        let source = state.source.as_ref()?;
        let conflict = self.engine.operations.conflict.as_ref()?;
        if source.identity != conflict.identity || !conflict.files.contains(&source.path) {
            return None;
        }
        Some(Request::SaveConflict {
            identity: source.identity.clone(),
            path: source.path.clone(),
            original: source.original.clone(),
            edited: state.editor.read(cx).value().to_string(),
        })
    }

    pub(super) fn conflict_continue_enabled(&self) -> bool {
        !self.engine.operation_busy
            && !self.external_merge_busy()
            && self
                .engine
                .operations
                .conflict
                .as_ref()
                .is_some_and(|c| c.kind != "Unmerged index" && c.files.is_empty())
    }
    pub(super) fn conflict_abort_enabled(&self) -> bool {
        !self.engine.operation_busy
            && !self.external_merge_busy()
            && self
                .engine
                .operations
                .conflict
                .as_ref()
                .is_some_and(|c| c.kind != "Unmerged index")
    }
    pub(super) fn conflict_selected_path(&self) -> Option<&str> {
        self.operations.conflicts.as_ref()?.selected.as_deref()
    }
    pub(super) fn conflict_editor_value(&self, cx: &Context<Self>) -> Option<String> {
        let state = self.operations.conflicts.as_ref()?;
        state.source.as_ref()?;
        Some(state.editor.read(cx).value().to_string())
    }
    pub(super) fn conflict_editor_edit(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = &self.operations.conflicts {
            state
                .editor
                .update(cx, |s, cx| s.set_value(text.to_owned(), window, cx));
        }
    }
    pub(super) fn conflict_editor_stale(&self) -> bool {
        self.operations.conflicts.as_ref().is_some_and(|s| s.stale)
    }

    fn conflict_commit_column(&self, side: usize, cx: &mut Context<Self>) -> AnyElement {
        let conflict = self.engine.operations.conflict.as_ref().unwrap();
        let state = self.operations.conflicts.as_ref().unwrap();
        let (title, label, count, scroll) = if side == 0 {
            (
                "INCOMING",
                &conflict.incoming_label,
                conflict.incoming.len(),
                state.incoming.clone(),
            )
        } else {
            (
                "LOCAL",
                &conflict.local_label,
                conflict.local.len(),
                state.local.clone(),
            )
        };
        column()
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .border_1()
            .border_color(rgb(LINE))
            .rounded_md()
            .overflow_hidden()
            .children(self.conflicts_probe(side))
            .child(
                column()
                    .px_3()
                    .py_2()
                    .gap_1()
                    .flex_shrink_0()
                    .bg(rgb(PANEL))
                    .child(
                        row()
                            .gap_2()
                            .child(
                                icon(if side == 0 { "pull" } else { "branch" })
                                    .text_color(rgb(ACCENT)),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(ACCENT))
                                    .child(title),
                            )
                            .child(div().flex_1())
                            .child(caption(count.to_string())),
                    )
                    .child(div().text_ellipsis().child(label.clone())),
            )
            .child(
                uniform_list(
                    if side == 0 {
                        "conflict-incoming"
                    } else {
                        "conflict-local"
                    },
                    count,
                    cx.processor(move |v, range: std::ops::Range<usize>, _, cx| {
                        let Some(conflict) = &v.engine.operations.conflict else {
                            return Vec::new();
                        };
                        let commits = if side == 0 {
                            &conflict.incoming
                        } else {
                            &conflict.local
                        };
                        range
                            .filter_map(|index| {
                                commits.get(index).map(|commit| {
                                    v.conflict_commit_row(
                                        commit,
                                        v.engine.app.inspected_id() == Some(commit.id),
                                        side,
                                        index,
                                        cx,
                                    )
                                })
                            })
                            .collect()
                    }),
                )
                .track_scroll(scroll)
                .flex_1()
                .min_h_0(),
            )
            .when(count == 0, |p| {
                p.child(caption("No separate commits on this side.").p_3())
            })
            .into_any_element()
    }

    fn conflict_files(&self, cx: &mut Context<Self>) -> AnyElement {
        let conflict = self.engine.operations.conflict.as_ref().unwrap();
        let state = self.operations.conflicts.as_ref().unwrap();
        column()
            .relative()
            .w(px(190.))
            .min_w(px(140.))
            .max_w(relative(0.36))
            .h_full()
            .flex_shrink_0()
            .border_r_1()
            .border_color(rgb(LINE))
            .children(self.conflicts_probe(2))
            .child(
                div()
                    .h(px(38.))
                    .px_3()
                    .py_2()
                    .flex_shrink_0()
                    .text_size(px(11.))
                    .text_color(rgb(MUTED))
                    .child("CONFLICTING FILES"),
            )
            .child(
                uniform_list(
                    "conflict-files",
                    conflict.states.len(),
                    cx.processor(|v, range: std::ops::Range<usize>, _, cx| {
                        let Some(conflict) = &v.engine.operations.conflict else {
                            return Vec::new();
                        };
                        range
                            .filter_map(|index| {
                                conflict.states.get(index).map(|file| {
                                    let path = file.path.clone();
                                    let selected =
                                        v.conflict_selected_path() == Some(path.as_str());
                                    row()
                                        .id(("conflict-file", index))
                                        .h(px(54.))
                                        .px_3()
                                        .gap_2()
                                        .border_l_2()
                                        .border_color(rgb(if selected { ACCENT } else { BG }))
                                        .bg(rgb(if selected { SELECTED } else { BG }))
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgb(HOVER)))
                                        .child(
                                            icon(if file.unmerged { "attention" } else { "check" })
                                                .text_color(rgb(if file.unmerged {
                                                    ACCENT
                                                } else {
                                                    ADDED
                                                })),
                                        )
                                        .child(
                                            column()
                                                .flex_1()
                                                .min_w_0()
                                                .gap_1()
                                                .child(div().text_ellipsis().child(path.clone()))
                                                .child(caption(if file.unmerged {
                                                    if !file.exists {
                                                        "Deleted · unresolved"
                                                    } else if file.edited {
                                                        "Edited · unresolved"
                                                    } else {
                                                        "Needs resolution"
                                                    }
                                                } else {
                                                    "Resolved"
                                                })),
                                        )
                                        .tooltip({
                                            let path = path.clone();
                                            move |w, cx| {
                                                gpui_component::tooltip::Tooltip::new(path.clone())
                                                    .build(w, cx)
                                            }
                                        })
                                        .on_click(cx.listener(move |v, _, w, cx| {
                                            v.operation_action(
                                                operations::Action::SelectConflictFile(
                                                    path.clone(),
                                                ),
                                                w,
                                                cx,
                                            )
                                        }))
                                        .into_any_element()
                                })
                            })
                            .collect()
                    }),
                )
                .track_scroll(state.files.clone())
                .flex_1()
                .min_h_0(),
            )
            .into_any_element()
    }

    fn conflict_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let conflict = self.engine.operations.conflict.as_ref().unwrap();
        let state = self.operations.conflicts.as_ref().unwrap();
        let Some(path) = &state.selected else {
            return column()
                .flex_1()
                .p_4()
                .child(caption("No conflicted files remain."))
                .into_any_element();
        };
        let file = conflict.states.iter().find(|s| &s.path == path);
        let unmerged = file.is_some_and(|s| s.unmerged);
        let disabled = self.engine.operation_busy || self.external_merge_busy() || !unmerged;
        let incoming = incoming_choice(conflict);
        let local = if matches!(incoming, Resolution::Ours) {
            Resolution::Theirs
        } else {
            Resolution::Ours
        };
        let side_button =
            |id: &'static str, title: &'static str, choice: Resolution, cx: &Context<Self>| {
                Button::new(id)
                    .small()
                    .label(title)
                    .disabled(disabled)
                    .tooltip(
                        self.engine
                            .operations
                            .preview
                            .as_ref()
                            .filter(|p| p.identity == conflict.identity && &p.path == path)
                            .map(|p| {
                                side_preview(if matches!(choice, Resolution::Ours) {
                                    &p.ours
                                } else {
                                    &p.theirs
                                })
                            })
                            .unwrap_or_else(|| {
                                "Replace the whole file with this version and stage it.".into()
                            }),
                    )
                    .on_click({
                        let path = path.clone();
                        cx.listener(move |v, _, w, cx| {
                            v.operation_action(
                                operations::Action::Run(Request::Resolve {
                                    path: path.clone(),
                                    choice: choice.clone(),
                                }),
                                w,
                                cx,
                            )
                        })
                    })
            };
        let mut editor = column()
            .flex_1()
            .min_w_0()
            .h_full()
            .p_3()
            .gap_2()
            .child(
                column()
                    .relative()
                    .flex_shrink_0()
                    .gap_1()
                    .children(self.conflicts_probe(8))
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_ellipsis()
                            .child(path.clone()),
                    )
                    .child(caption(if file.is_some_and(|f| f.unmerged && !f.exists) {
                        "Deleted in the working tree. Choose a version or mark the deletion resolved."
                    } else if unmerged {
                        "Choose a whole version or edit the result below."
                    } else {
                        "Resolved and staged. The editor buffer below is retained and has not been saved again."
                    })),
            )
            .child(
                row()
                    .gap_2()
                    .flex_wrap()
                    .flex_shrink_0()
                    .child(side_button(
                        "conflict-use-incoming",
                        "Use incoming…",
                        incoming,
                        cx,
                    ))
                    .child(side_button("conflict-use-local", "Use local…", local, cx))
                    .child(self.tools_merge_button(path, cx))
                    .child(self.ai_button("ai-resolve-conflict", "Resolve with AI", crate::ai::Task::Conflict, cx)),
            );
        if state.stale {
            editor = editor.child(row().flex_wrap().gap_2().flex_shrink_0()
                .child(caption("File changed on disk. Your draft is preserved; saving will check the original file.").text_color(rgb(ACCENT)))
                .child(Button::new("conflict-reload-draft").small().label("Discard draft & reload").disabled(disabled)
                    .on_click(cx.listener(|v, _, w, cx| v.operation_action(operations::Action::ReloadConflict, w, cx)))));
        }
        if state.source.as_ref().is_some_and(|s| s.path == *path) {
            editor = editor
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h(px(110.))
                        .children(self.conflicts_probe(3))
                        .child(Input::new(&state.editor).size_full().disabled(disabled)),
                )
                .child(
                    row()
                        .flex_wrap()
                        .gap_2()
                        .flex_shrink_0()
                        .child(
                            Button::new("save-resolution")
                                .small()
                                .primary()
                                .label("Save and mark resolved")
                                .disabled(disabled)
                                .on_click(cx.listener(|v, _, w, cx| {
                                    v.operation_action(operations::Action::SaveConflict, w, cx)
                                })),
                        )
                        .child(
                            Button::new("mark-resolution")
                                .small()
                                .ghost()
                                .label("Mark file resolved")
                                .disabled(disabled)
                                .on_click({
                                    let path = path.clone();
                                    cx.listener(move |v, _, w, cx| {
                                        v.operation_action(
                                            operations::Action::Run(Request::Resolve {
                                                path: path.clone(),
                                                choice: Resolution::Mark,
                                            }),
                                            w,
                                            cx,
                                        )
                                    })
                                }),
                        ),
                );
        } else if unmerged {
            editor = editor.child(caption(if self.engine.operation_busy { "Loading file…" } else { "Binary, large, or non-text file. Choose an entire version or edit it in your application." }))
                .child(Button::new("mark-binary-resolution").small().label("Mark file resolved").disabled(disabled)
                    .on_click({ let path = path.clone(); cx.listener(move |v, _, w, cx| v.operation_action(operations::Action::Run(Request::Resolve { path: path.clone(), choice: Resolution::Mark }), w, cx)) }));
        }
        editor.into_any_element()
    }

    pub(super) fn conflicts_content(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(conflict) = &self.engine.operations.conflict else {
            return column()
                .flex_1()
                .p_6()
                .child("No operation is waiting for conflict resolution.")
                .into_any_element();
        };
        if self.operations.conflicts.is_none() {
            return column()
                .flex_1()
                .p_6()
                .child("Loading conflict workspace…")
                .into_any_element();
        }
        let left = column().flex_1().min_w_0().h_full().min_h_0()
            .child(column().p_4().gap_1().flex_shrink_0()
                .child(row().gap_2().child(icon("attention").text_color(rgb(ACCENT)))
                    .child(div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).child(format!("Resolve {} conflicts", conflict.kind.to_lowercase()))))
                .child(caption(format!("{} unresolved · changes refresh automatically", conflict.files.len())))
                .when(conflict.commits_truncated, |p| p.child(caption("Commit lists are limited; select a commit to inspect its full details.")))
                .when_some(conflict.metadata_error.clone(), |p, error| p.child(caption(error).text_color(rgb(REMOVED)))))
            .when_some(self.engine.conflict_refresh_error.clone(), |p, error| p.child(caption(format!("Automatic refresh paused: {error}")).px_4().text_color(rgb(REMOVED))))
            .when(self.engine.operation_error, |p| p.child(caption(self.engine.operation_status.clone()).px_4().text_color(rgb(REMOVED))))
            .child(row().h(relative(0.34)).min_h(px(155.)).max_h(px(270.)).flex_shrink_0().px_4().gap_3()
                .child(self.conflict_commit_column(0, cx)).child(self.conflict_commit_column(1, cx)))
            .child(row().flex_1().min_h_0().mt_3().border_t_1().border_color(rgb(LINE))
                .child(self.conflict_files(cx)).child(self.conflict_editor(cx)))
            .children(self.tools_notice())
            .child(row().relative().h(px(64.)).w_full().flex_shrink_0().px_4().gap_2().bg(rgb(PANEL)).border_t_1().border_color(rgb(LINE))
                .children(self.conflicts_probe(4))
                .when(matches!(conflict.kind.as_str(), "Rebase" | "Cherry-pick" | "Revert"), |r| r.child(
                    Button::new("conflict-skip").small().ghost().label("Skip commit…").disabled(self.engine.operation_busy || self.external_merge_busy())
                        .on_click(cx.listener(|v, _, w, cx| v.operation_action(operations::Action::Run(Request::Skip), w, cx)))))
                .child(div().flex_1())
                .child(div().relative().children(self.conflicts_probe(6)).child(Button::new("conflict-abort").label("Abort…").disabled(!self.conflict_abort_enabled())
                    .on_click(cx.listener(|v, _, w, cx| v.operation_action(operations::Action::Run(Request::Abort), w, cx)))))
                .child(div().relative().children(self.conflicts_probe(5)).child(Button::new("conflict-continue").primary()
                    .label(if conflict.kind == "Stash apply" { "Finish applying stash" } else { "Continue" }).disabled(!self.conflict_continue_enabled())
                    .on_click(cx.listener(|v, _, w, cx| v.operation_action(operations::Action::Run(Request::Continue), w, cx))))));
        row()
            .flex_1()
            .min_h_0()
            .size_full()
            .items_start()
            .child(left)
            .child(self.conflict_details(cx))
            .into_any_element()
    }
}
