//! Contextual graph actions and focused recovery views.
use super::*;
use crate::git::operations::{PushPreview, PushRisk, Request, Resolution};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Dialog {
    Create,
    Rename,
    Delete,
    Merge,
    Rebase,
    Plan,
    Push,
}
#[derive(Clone)]
pub(super) enum Action {
    Open(Dialog, String, String),
    Submit,
    Pull,
    StartPlan,
    ClosePlan,
    ShowConflicts,
    Run(Request),
    Confirm,
    Cancel,
    History,
    SaveConflict,
}
pub(super) struct Panel {
    pub tab: Option<Tab>,
    name: Entity<InputState>,
    _name_subscription: Subscription,
    target: String,
    label: String,
    current_branch: String,
    current_head: String,
    dialog: Option<Dialog>,
    push: Option<PushPreview>,
    push_pending: bool,
    pub rebase_open: bool,
    pub plan_pending: bool,
    rebase_submitted: bool,
    plan_text: Entity<InputState>,
    dialog_error: Option<String>,
    epoch: u64,
    conflict_open: bool,
    confirmation: Option<(Request, String)>,
    resolution: Entity<InputState>,
    resolution_source: Option<(String, String)>,
}
impl Panel {
    pub fn summary(&self) -> String {
        format!(
            "dialog={:?} rebase_open={} push={:?}",
            self.dialog,
            self.rebase_open,
            self.push.as_ref().map(|p| &p.risk)
        )
    }
    pub fn dismiss(&mut self) -> bool {
        self.dialog.take().is_some() || self.confirmation.take().is_some()
    }

    pub fn new(window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Branch name"));
        let subscription = cx.subscribe(&name, |_, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        Self {
            tab: None,
            name,
            _name_subscription: subscription,
            target: String::new(),
            label: String::new(),
            current_branch: String::new(),
            current_head: String::new(),
            dialog: None,
            push: None,
            push_pending: false,
            rebase_open: false,
            plan_pending: false,
            rebase_submitted: false,
            plan_text: cx.new(|cx| InputState::new(window, cx).multi_line(true)),
            dialog_error: None,
            epoch: 0,
            conflict_open: false,
            confirmation: None,
            resolution: cx.new(|cx| InputState::new(window, cx).multi_line(true)),
            resolution_source: None,
        }
    }
}
impl Desktop {
    pub(super) fn current_branch(&self) -> String {
        self.engine
            .app
            .refs
            .as_ref()
            .and_then(|r| r.head_branch.clone())
            .unwrap_or_else(|| "detached HEAD".into())
    }
    pub(super) fn sync_operations(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.operations.epoch == self.engine.operation_epoch {
            return;
        }
        self.operations.epoch = self.engine.operation_epoch;
        let conflict = self.engine.operations.conflict.is_some();
        if conflict && !self.operations.conflict_open {
            self.operations.tab = Some(Tab::Conflicts);
        }
        if !conflict && self.operations.tab == Some(Tab::Conflicts) {
            self.operations.tab = None;
        }
        self.operations.conflict_open = conflict;
        if self.operations.push_pending && !self.engine.operation_busy {
            self.operations.push_pending = false;
            self.operations.push = self.engine.operations.push.clone();
        }
        if self.operations.plan_pending && !self.engine.operation_busy {
            self.operations.plan_pending = false;
            if self.engine.operation_error {
                self.operations.dialog_error = Some(self.engine.operation_status.clone());
            }
            if !self.engine.operation_error
                && let Some(plan) = &self.engine.operations.plan
            {
                self.operations
                    .plan_text
                    .update(cx, |s, cx| s.set_value(plan.text.clone(), window, cx));

                self.operations.rebase_open = true;
                self.operations.tab = Some(Tab::Rebase);
                self.operations.dialog = None;
            }
        }
        if self.operations.rebase_submitted && !self.engine.operation_busy {
            self.operations.rebase_submitted = false;
            if !self.engine.operation_error || conflict {
                self.operations.rebase_open = false;
            }
        }
        if self.operations.tab == Some(Tab::Rebase)
            && !self.operations.rebase_open
            && !self.engine.operations.interactive_rebase
        {
            self.operations.tab = None;
        }
        if let Some(preview) = &self.engine.operations.preview
            && let Some(text) = &preview.original
        {
            let identity = (preview.path.clone(), text.clone());
            if self.operations.resolution_source.as_ref() != Some(&identity) {
                self.operations
                    .resolution
                    .update(cx, |s, cx| s.set_value(text.clone(), window, cx));
                self.operations.resolution_source = Some(identity);
            }
        }
        cx.notify();
    }
    pub(super) fn operation_action(
        &mut self,
        action: Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let request = match action {
            Action::Open(dialog, target, label) => {
                self.operations.dialog = Some(dialog);
                self.operations.dialog_error = None;
                self.operations.target = target;
                self.operations.label = label;
                self.operations.current_branch = self.current_branch();
                self.operations.current_head = self
                    .engine
                    .app
                    .head_id()
                    .map(|id| id.to_string())
                    .unwrap_or_default();
                self.operations
                    .name
                    .update(cx, |s, cx| s.set_value("", window, cx));
                if matches!(dialog, Dialog::Create | Dialog::Rename) {
                    self.operations.name.update(cx, |s, cx| s.focus(window, cx));
                }
                self.operations.push = None;
                self.operations.push_pending = dialog == Dialog::Push;
                if dialog == Dialog::Push {
                    Some(Request::PreviewPush)
                } else {
                    None
                }
            }
            Action::Submit => {
                let target = self.operations.target.clone();
                let name = self.operations.name.read(cx).value().trim().to_string();
                let request = match self.operations.dialog {
                    Some(Dialog::Create) => Some(Request::Create { name, target }),
                    Some(Dialog::Rename) => Some(Request::Rename {
                        old: target,
                        new: name,
                    }),
                    Some(Dialog::Delete) => Some(Request::Delete {
                        name: target,
                        force: false,
                    }),
                    Some(Dialog::Merge) => Some(Request::Merge(target)),
                    Some(Dialog::Rebase) => Some(Request::Rebase(target)),
                    Some(Dialog::Plan) => {
                        self.engine.operations.plan = None;
                        self.operations.plan_pending = true;
                        Some(Request::Plan(target))
                    }
                    Some(Dialog::Push) => self
                        .operations
                        .push
                        .as_ref()
                        .filter(|p| p.risk != PushRisk::Blocked)
                        .cloned()
                        .map(Request::PushReviewed),
                    None => None,
                };
                let request = request.map(|r| {
                    if matches!(r, Request::Merge(_) | Request::Rebase(_) | Request::Plan(_)) {
                        Request::OnBranch {
                            branch: self.operations.current_branch.clone(),
                            head: self.operations.current_head.clone(),
                            request: Box::new(r),
                        }
                    } else {
                        r
                    }
                });
                if request.is_some() && !self.operations.plan_pending {
                    self.operations.dialog = None;
                }
                request
            }
            Action::History => self
                .toggle_status_panel(status::Panel::History)
                .then_some(Request::Inspect),
            Action::Cancel => {
                self.operations.confirmation = None;
                self.operations.dialog = None;
                None
            }
            Action::Confirm => {
                let request = self.operations.confirmation.take().map(|c| c.0);
                if matches!(request, Some(Request::Interactive { .. })) {
                    self.operations.rebase_submitted = true;
                }
                request
            }
            Action::SaveConflict => {
                self.operations
                    .resolution_source
                    .as_ref()
                    .map(|(path, original)| Request::SaveConflict {
                        path: path.clone(),
                        original: original.clone(),
                        edited: self.operations.resolution.read(cx).value().to_string(),
                    })
            }
            Action::Pull => Some(Request::Pull),
            Action::ShowConflicts => {
                self.operations.tab = Some(Tab::Conflicts);
                None
            }
            Action::ClosePlan => {
                self.operations.rebase_open = false;
                self.operations.tab = None;
                self.engine.operations.plan = None;
                None
            }
            Action::StartPlan => {
                if let Some(plan) = &self.engine.operations.plan {
                    self.operations.confirmation = Some((
                        Request::Interactive {
                            plan: plan.clone(),
                            text: self.operations.plan_text.read(cx).value().to_string(),
                        },
                        format!(
                            "Rewrite {} using this plan, based on {}? Dropped commits remain in local recovery checkpoints.",
                            plan.branch,
                            &plan.base[..12.min(plan.base.len())]
                        ),
                    ));
                }
                None
            }
            Action::Run(
                request @ (Request::Abort
                | Request::Skip
                | Request::Resolve {
                    choice: Resolution::Ours | Resolution::Theirs,
                    ..
                }),
            ) => {
                let text = match &request {
                    Request::Abort => {
                        "Abort the operation? Conflict-resolution edits will be discarded.".into()
                    }
                    Request::Skip => "Skip this commit? Its changes will be omitted.".into(),
                    Request::Resolve { path, choice } => format!(
                        "Replace all of {path} with {} and stage it? A deleted side removes the file.",
                        if matches!(choice, Resolution::Ours) {
                            "ours"
                        } else {
                            "theirs"
                        }
                    ),
                    _ => unreachable!(),
                };
                self.operations.confirmation = Some((request, text));
                None
            }
            Action::Run(request) => Some(request),
        };
        if let Some(request) = request {
            if self
                .engine
                .app
                .checkout_notice
                .as_ref()
                .is_some_and(Result::is_ok)
            {
                self.engine.app.checkout_notice = None;
            }
            self.engine.request_operation(request);
            self.engine.tick(None);
        }
        cx.notify();
    }
    fn op_button(
        &self,
        id: &'static str,
        text: &'static str,
        action: Action,
        cx: &Context<Self>,
    ) -> AnyElement {
        self.button(id, text, super::Action::Operation(Box::new(action)), cx)
            .into_any_element()
    }
    pub(super) fn operations_content(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut content = column()
            .id("git-workflows")
            .flex_1()
            .overflow_y_scroll()
            .p_6()
            .gap_4();
        match self.operations.tab {
            Some(Tab::Rebase) => {
                content = content.child(
                    row()
                        .child(icon("graph"))
                        .child(div().text_size(px(22.)).child("Interactive rebase")),
                );
                if self.engine.operations.interactive_rebase {
                    content = content.child(caption("Rebase is in progress. Resolve or amend the current commit, then continue."))
                        .child(self.op_button("rebase-progress", "Open rebase progress", Action::ShowConflicts, cx));
                } else {
                    content = content.child(caption("Oldest commit first. Move lines to reorder; change pick to reword, squash, fixup, drop or edit."))
                        .child(caption("For reword, replace the message after the commit ID. Keep every full ID once. Edit pauses for an external amend."))
                        .child(div().min_h(px(300.)).child(Input::new(&self.operations.plan_text).h(px(360.))))
                        .child(row().child(self.op_button("start-plan", "Review and start rebase…", Action::StartPlan, cx))
                            .child(self.op_button("close-plan", "Discard plan", Action::ClosePlan, cx)));
                }
            }
            Some(Tab::Conflicts) => {
                if let Some(conflict) = &self.engine.operations.conflict {
                    content = content.child(div().text_size(px(22.)).child(format!("{} in progress", conflict.kind)))
                        .child(caption("1. Resolve each file.  2. Mark edited files resolved.  3. Continue when none remain."))
                        .child(caption(if conflict.kind == "Rebase" { "During rebase: ours is the new base plus replayed commits; theirs is the commit being replayed." } else { "Ours is your current branch; theirs is the incoming version." }))
                        .child(caption("Edit files in your usual editor. Choosing an entire side replaces and stages it, including binary files and deletions."))
                        .child(row().child(self.op_button("conflict-refresh", "Refresh files", Action::Run(Request::Inspect), cx))
                            .child(self.op_button("conflict-continue", "Continue", Action::Run(Request::Continue), cx))
                            .when(conflict.kind != "Merge", |r| r.child(self.op_button("conflict-skip", "Skip commit…", Action::Run(Request::Skip), cx)))
                            .child(self.op_button("conflict-abort", "Abort…", Action::Run(Request::Abort), cx)))
                        .child(caption(format!("{} unresolved files", conflict.files.len())));
                    content = content.child(self.conflict_list(cx));
                    if conflict.files.is_empty() {
                        content = content.child(caption("No unresolved files. Continue to finish or reach the next rebase step."));
                    }
                    if let Some(preview) = &self.engine.operations.preview {
                        content = content
                            .child(div().text_size(px(18.)).child(preview.path.clone()))
                            .child(
                                row()
                                    .items_start()
                                    .child(
                                        column().flex_1().gap_2().child("Ours").child(
                                            div()
                                                .id("conflict-ours")
                                                .h(px(160.))
                                                .overflow_y_scroll()
                                                .p_3()
                                                .bg(rgb(PANEL))
                                                .child(preview.ours.clone()),
                                        ),
                                    )
                                    .child(
                                        column().flex_1().gap_2().child("Theirs").child(
                                            div()
                                                .id("conflict-theirs")
                                                .h(px(160.))
                                                .overflow_y_scroll()
                                                .p_3()
                                                .bg(rgb(PANEL))
                                                .child(preview.theirs.clone()),
                                        ),
                                    ),
                            );
                        if preview.original.is_some() {
                            content = content
                                .child("Resolved result")
                                .child(Input::new(&self.operations.resolution).h(px(220.)))
                                .child(self.op_button(
                                    "save-resolution",
                                    "Save and mark resolved",
                                    Action::SaveConflict,
                                    cx,
                                ));
                        } else {
                            content = content.child(caption(
                                "Use a whole side or your external editor for this file.",
                            ));
                        }
                    }
                } else {
                    content = content.child("No operation is waiting for conflict resolution.");
                }
            }
            _ => {}
        }
        content.into_any_element()
    }
    fn conflict_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let count = self
            .engine
            .operations
            .conflict
            .as_ref()
            .map_or(0, |c| c.files.len());
        uniform_list(
            "conflict-files",
            count,
            cx.processor(|v, range: std::ops::Range<usize>, _, cx| {
                let mut rows = Vec::new();
                let Some(conflict) = &v.engine.operations.conflict else {
                    return rows;
                };
                for i in range {
                    let path = &conflict.files[i];
                    let read_path = path.clone();
                    let mut buttons = row().child(
                        div()
                            .id(("inspect-conflict", i))
                            .px_3()
                            .py_2()
                            .bg(rgb(CONTROL))
                            .rounded_md()
                            .cursor_pointer()
                            .child("Compare / edit")
                            .on_click(cx.listener(move |v, _, w, cx| {
                                v.operation_action(
                                    Action::Run(Request::ReadConflict(read_path.clone())),
                                    w,
                                    cx,
                                )
                            })),
                    );
                    for (j, (label, choice)) in [
                        ("Use ours…", Resolution::Ours),
                        ("Use theirs…", Resolution::Theirs),
                        ("Mark edited file resolved", Resolution::Mark),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        let path = path.clone();
                        buttons = buttons.child(
                            div()
                                .id(("resolve", i * 3 + j))
                                .px_3()
                                .py_2()
                                .bg(rgb(CONTROL))
                                .rounded_md()
                                .cursor_pointer()
                                .child(label)
                                .on_click(cx.listener(move |v, _, w, cx| {
                                    v.operation_action(
                                        Action::Run(Request::Resolve {
                                            path: path.clone(),
                                            choice: choice.clone(),
                                        }),
                                        w,
                                        cx,
                                    )
                                })),
                        );
                    }
                    rows.push(
                        column()
                            .p_3()
                            .gap_2()
                            .bg(rgb(PANEL))
                            .rounded_md()
                            .child(path.clone())
                            .child(buttons)
                            .h(px(88.))
                            .into_any_element(),
                    );
                }
                rows
            }),
        )
        .h(px((count.min(4) * 88) as f32))
        .w_full()
        .into_any_element()
    }
    pub(super) fn operations_overlay(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if let Some(dialog) = self.operations.dialog {
            let op = &self.operations;
            let (title, detail, submit) = match dialog {
                Dialog::Create => (
                    "Create branch",
                    format!(
                        "Create a local branch at {}. Your current checkout stays on {}.",
                        op.label, op.current_branch
                    ),
                    "Create branch",
                ),
                Dialog::Rename => (
                    "Rename branch",
                    format!(
                        "Give local branch {} a new name. Its commits stay the same.",
                        op.label
                    ),
                    "Rename branch",
                ),
                Dialog::Delete => (
                    "Delete branch",
                    format!(
                        "Remove local branch {}. Git will refuse if it has unmerged commits or is checked out. Its remote branch stays intact.",
                        op.label
                    ),
                    "Delete branch",
                ),
                Dialog::Merge => (
                    "Merge into your branch",
                    format!(
                        "Bring the changes from {} into {}. Git will fast-forward when possible, or create a merge commit. Conflicts open a guided resolution view.",
                        op.label, op.current_branch
                    ),
                    "Merge",
                ),
                Dialog::Rebase => (
                    "Rebase your branch",
                    format!(
                        "Replay the commits on {} onto {}. This rewrites commit IDs on {} and may require a force push with lease afterward.",
                        op.current_branch, op.label, op.current_branch
                    ),
                    "Rebase",
                ),
                Dialog::Plan => (
                    "Plan an interactive rebase",
                    format!(
                        "Edit the commits on {} after {} (the base itself is excluded). Review their order and choose which to squash, rename or drop before anything changes.",
                        op.current_branch, op.label
                    ),
                    "Prepare plan",
                ),
                Dialog::Push => (
                    "Push current branch",
                    "".into(),
                    if op
                        .push
                        .as_ref()
                        .is_some_and(|p| p.risk == PushRisk::Rewrite)
                    {
                        "Force push with lease"
                    } else {
                        "Push branch"
                    },
                ),
            };
            let mut body = dialog_body(title);
            if dialog == Dialog::Push {
                if op.push_pending {
                    body = body.child(caption("Checking the remote branch… You can close this dialog while the check runs."));
                } else if let Some(push) = &op.push {
                    let (color, label) = match push.risk {
                        PushRisk::Trivial => (ADDED, "Ready for a normal push"),
                        PushRisk::Rewrite => (0xe5a04b, "History rewrite · lease protected"),
                        PushRisk::Blocked => (REMOVED, "Remote changed · push blocked"),
                    };
                    body = body
                        .child(
                            row()
                                .gap_3()
                                .child(
                                    icon(if push.risk == PushRisk::Trivial {
                                        "shield-check"
                                    } else {
                                        "shield-alert"
                                    })
                                    .size(px(32.))
                                    .text_color(rgb(color)),
                                )
                                .child(div().text_color(rgb(color)).child(label)),
                        )
                        .child(column().p_3().gap_2().bg(rgb(CONTROL)).rounded_md().child(
                            format!("{}  →  {}/{}", push.local_branch, push.remote, push.branch),
                        ));
                    if push.risk != PushRisk::Trivial {
                        body = body.child(push.explanation.clone());
                    }
                    body = body.child(caption("clears undo history"));
                } else {
                    body = body.child(row().child(icon("shield-alert").size(px(32.)).text_color(rgb(REMOVED))).child("Could not verify this push"))
                        .child(self.engine.operation_status.clone()).child(caption("No push was sent. Check your remote and authentication, then try again."));
                }
            } else {
                body = body.child(detail);
                if matches!(dialog, Dialog::Create | Dialog::Rename) {
                    body = body.child(Input::new(&op.name));
                }
                if matches!(dialog, Dialog::Merge | Dialog::Rebase | Dialog::Plan) {
                    body = body.child(
                        column()
                            .p_3()
                            .gap_2()
                            .rounded_md()
                            .bg(rgb(CONTROL))
                            .child(caption("SELECTED TARGET"))
                            .child(op.label.clone())
                            .child(caption("CURRENT BRANCH"))
                            .child(op.current_branch.clone()),
                    );
                }
            }
            if let Some(error) = &op.dialog_error {
                body = body.child(div().text_color(rgb(REMOVED)).child(error.clone()));
            }
            let enabled = !self.engine.operation_busy
                && !self.engine.app.pending.busy
                && !self.engine.app.branches.busy
                && !self.engine.app.settings.saving
                && match dialog {
                    Dialog::Push => op
                        .push
                        .as_ref()
                        .is_some_and(|p| p.risk != PushRisk::Blocked),
                    Dialog::Create | Dialog::Rename => !op.name.read(cx).value().trim().is_empty(),
                    _ => true,
                };
            body = body.child(
                dialog_actions()
                    .child(self.op_button("dialog-cancel", "Cancel", Action::Cancel, cx))
                    .child(
                        div()
                            .id("dialog-submit")
                            .px_4()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(if enabled {
                                if dialog == Dialog::Delete {
                                    REMOVED
                                } else {
                                    ACCENT
                                }
                            } else {
                                CONTROL
                            }))
                            .text_color(rgb(if enabled { BG } else { MUTED }))
                            .child(submit)
                            .when(enabled, |s| {
                                s.cursor_pointer()
                                    .hover(|s| s.opacity(0.85))
                                    .on_click(cx.listener(|v, _, w, cx| {
                                        v.operation_action(Action::Submit, w, cx)
                                    }))
                            }),
                    ),
            );
            return Some(dialog_overlay(body).into_any_element());
        }
        if let Some((_, text)) = &self.operations.confirmation {
            return Some(
                column()
                    .absolute()
                    .inset_0()
                    .items_center()
                    .justify_center()
                    .bg(rgba(0x00000099))
                    .occlude()
                    .child(
                        column()
                            .w(px(600.))
                            .p_6()
                            .gap_4()
                            .rounded_lg()
                            .bg(rgb(PANEL))
                            .border_1()
                            .border_color(rgb(LINE))
                            .child(div().text_size(px(20.)).child("Review Git operation"))
                            .child(text.clone())
                            .child(
                                row()
                                    .child(self.op_button(
                                        "operation-confirm",
                                        "Confirm",
                                        Action::Confirm,
                                        cx,
                                    ))
                                    .child(self.op_button(
                                        "operation-cancel",
                                        "Cancel",
                                        Action::Cancel,
                                        cx,
                                    )),
                            ),
                    )
                    .into_any_element(),
            );
        }
        if self.status_panel != Some(status::Panel::History) {
            return None;
        }
        let view = &self.engine.operations;
        let mut list = column()
            .id("operation-history-list")
            .max_h(px(320.))
            .overflow_y_scroll()
            .gap_1();
        for (i, label) in std::iter::once("Starting state")
            .chain(view.labels.iter().map(String::as_str))
            .enumerate()
        {
            list = list.child(
                row()
                    .id(("history-position", i))
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(if i == view.cursor { SELECTED } else { CONTROL }))
                    .child(format!(
                        "{} {i}. {label}",
                        if i == view.cursor { "●" } else { " " }
                    ))
                    .cursor_pointer()
                    .on_click(cx.listener(move |v, event: &ClickEvent, w, cx| {
                        if event.click_count() == 2 {
                            v.operation_action(Action::Run(Request::MoveTo(i)), w, cx);
                        }
                    })),
            );
        }
        Some(
            column()
                .id("operation-history-panel")
                .absolute()
                .right(px(12.))
                .bottom(px(42.))
                .w(px(540.))
                .p_4()
                .gap_3()
                .bg(rgb(PANEL))
                .border_1()
                .border_color(rgb(LINE))
                .rounded_lg()
                .shadow_lg()
                .occlude()
                .on_mouse_down_out(cx.listener(Self::dismiss_status_panel_outside))
                .child(div().text_size(px(18.)).child("Undo / redo history"))
                .child(caption(
                    "Double-click a state to undo or redo all operations up to it.",
                ))
                .when(!view.boundary.is_empty(), |s| {
                    s.child(caption(view.boundary.clone()))
                })
                .child(list)
                .child(
                    row()
                        .child(self.op_button(
                            "history-undo",
                            "Undo",
                            Action::Run(Request::MoveTo(view.cursor.saturating_sub(1))),
                            cx,
                        ))
                        .child(self.op_button(
                            "history-redo",
                            "Redo",
                            Action::Run(Request::MoveTo((view.cursor + 1).min(view.labels.len()))),
                            cx,
                        )),
                )
                .into_any_element(),
        )
    }
}
