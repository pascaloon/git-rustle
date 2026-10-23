//! Contextual graph actions and focused recovery views.
use super::rebase::{CommitAction, Draft};
use super::*;
use crate::git::operations::{PushPreview, PushRisk, Request, ResetMode, Resolution};
use gpui_component::checkbox::Checkbox;
use gpui_component::select::{Select, SelectEvent, SelectItem, SelectState};
use std::collections::HashMap;
mod history;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Dialog {
    Create,
    CreateTag,
    Rename,
    Delete,
    Reset,
    Merge,
    Rebase,
    Plan,
    Push,
}
#[derive(Clone)]
pub(super) enum Action {
    Open(Dialog, String, String),
    Submit,
    ResetMode(ResetMode),
    ForceDelete(bool),
    Pull,
    CherryPick(Vec<String>),
    StartPlan,
    ClosePlan,
    ResetPlan,
    SetPlan(usize, CommitAction),
    MovePlan(usize, isize),
    ShowConflicts,
    Run(Request),
    Confirm,
    Cancel,
    History,
    CloseHistory,
    ReviewClear,
    CancelClear,
    ClearHistory,
    SaveConflict,
    SelectConflictFile(String),
    ReloadConflict,
}
pub(super) struct Panel {
    pub tab: Option<Tab>,
    name: Entity<InputState>,
    _name_subscription: Subscription,
    annotation: Option<Entity<InputState>>,
    _annotation_subscription: Option<Subscription>,
    target: String,
    label: String,
    current_branch: String,
    current_head: String,
    dialog: Option<Dialog>,
    reset_mode: ResetMode,
    force_delete: bool,
    push: Option<PushPreview>,
    push_pending: bool,
    pub rebase_open: bool,
    pub plan_pending: bool,
    rebase_submitted: bool,
    draft: Draft,
    controls: HashMap<usize, PlanControls>,
    plan_scroll: UniformListScrollHandle,
    plan_visible: usize,
    history_clear: bool,
    history_scroll: UniformListScrollHandle,
    history_visible: usize,
    dialog_error: Option<String>,
    epoch: u64,
    conflict_open: bool,
    pub(super) confirmation: Option<(Request, String)>,
    pub(super) conflicts: Option<super::conflicts::State>,
    tag_scenario_target: Option<gix::ObjectId>,
    tag_scenario_head: Option<gix::ObjectId>,
    tag_scenario_submitted: bool,
    tag_scenario_label: bool,
    tag_scenario_verified: bool,
}
struct PlanControls {
    select: Entity<SelectState<Vec<CommitAction>>>,
    message: Option<Entity<InputState>>,
    _subscriptions: Vec<Subscription>,
}
impl SelectItem for CommitAction {
    type Value = Self;
    fn title(&self) -> SharedString {
        self.name().into()
    }
    fn value(&self) -> &Self {
        self
    }
    fn render(&self, _: &mut Window, _: &mut gpui::App) -> impl IntoElement {
        column()
            .py_1()
            .gap_1()
            .child(self.name())
            .child(caption(self.description()))
    }
}
impl Panel {
    pub(super) fn modal_open(&self) -> bool {
        self.dialog.is_some() || self.confirmation.is_some() || self.history_clear
    }
    pub fn summary(&self) -> String {
        format!(
            "dialog={:?} rebase_open={} push={:?} plan_rows={} plan_visible={} plan_errors={} history_visible={} history_clear={} tag_scenario_created={} tag_scenario_label={} tag_scenario_verified={}",
            self.dialog,
            self.rebase_open,
            self.push.as_ref().map(|p| &p.risk),
            self.draft.entries.len(),
            self.plan_visible,
            self.draft.errors,
            self.history_visible,
            self.history_clear,
            self.tag_scenario_submitted,
            self.tag_scenario_label,
            self.tag_scenario_verified,
        )
    }
    pub fn dismiss(&mut self) -> bool {
        self.dialog.take().is_some() || self.confirmation.take().is_some()
    }

    pub fn new(window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Name"));
        let subscription = cx.subscribe(&name, |_, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        Self {
            tab: None,
            name,
            _name_subscription: subscription,
            annotation: None,
            _annotation_subscription: None,
            target: String::new(),
            label: String::new(),
            current_branch: String::new(),
            current_head: String::new(),
            dialog: None,
            reset_mode: ResetMode::Mixed,
            force_delete: false,
            push: None,
            push_pending: false,
            rebase_open: false,
            plan_pending: false,
            rebase_submitted: false,
            draft: Draft::default(),
            controls: HashMap::new(),
            plan_scroll: UniformListScrollHandle::new(),
            plan_visible: 0,
            history_clear: false,
            history_scroll: UniformListScrollHandle::new(),
            history_visible: 0,
            dialog_error: None,
            epoch: 0,
            conflict_open: false,
            confirmation: None,
            conflicts: None,
            tag_scenario_target: None,
            tag_scenario_head: None,
            tag_scenario_submitted: false,
            tag_scenario_label: false,
            tag_scenario_verified: false,
        }
    }
}
impl Desktop {
    pub(super) fn set_branch_scenario_name(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        assert_eq!(self.operations.dialog, Some(Dialog::Create));
        self.operations.name.update(cx, |state, cx| {
            state.set_value(name.to_string(), window, cx)
        });
    }

    pub(super) fn set_tag_scenario_inputs(
        &mut self,
        name: &str,
        message: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.operations
            .name
            .update(cx, |s, cx| s.set_value(name.to_string(), window, cx));
        self.operations
            .annotation
            .as_ref()
            .expect("tag dialog annotation")
            .update(cx, |s, cx| s.set_value(message.to_string(), window, cx));
    }

    pub(super) fn tag_scenario_start(
        &mut self,
        create: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        assert!(
            self.engine.app.history.len() >= 2,
            "tag fixture needs two commits"
        );
        self.action(super::Action::Select(1, false), window, cx);
        self.operations.tag_scenario_target = self
            .selected_commits
            .checkout_target(self.engine.app.selected_id());
        self.operations.tag_scenario_head = self.engine.app.head_id();
        assert_ne!(
            self.operations.tag_scenario_target,
            self.operations.tag_scenario_head
        );
        let (action, enabled) = self.create_tag_action().expect("selected local tag target");
        assert!(enabled);
        if !create {
            self.engine.mark("desktop operation requested");
        }
        self.action(action, window, cx);
        if create {
            self.set_tag_scenario_inputs("bench-tag", "Benchmark annotation", window, cx);
            let weak = cx.entity().downgrade();
            window.on_next_frame(move |window, cx| {
                let _ = weak.update(cx, |v, cx| {
                    v.engine.mark("desktop operation requested");
                    v.operations.tag_scenario_submitted = true;
                    v.operation_action(Action::Submit, window, cx);
                });
            });
        }
        cx.notify();
    }

    pub(super) fn tag_scenario_ready(&self, create: bool) -> bool {
        if !create {
            return self.operations.dialog == Some(Dialog::CreateTag);
        }
        self.operations.tag_scenario_submitted
            && !self.engine.operation_busy
            && (self.engine.operation_error
                || self.operations.tag_scenario_target.is_some_and(|id| {
                    self.engine.app.labels.get(&id).is_some_and(|labels| {
                        labels.iter().any(|label| {
                            label.kind == crate::git::RefKind::Tag && label.name == "bench-tag"
                        })
                    })
                }))
    }

    pub(super) fn tag_scenario_assert(&mut self, create: bool) {
        assert_eq!(self.engine.app.head_id(), self.operations.tag_scenario_head);
        if create {
            assert!(
                !self.engine.operation_error,
                "{}",
                self.engine.operation_status
            );
            assert!(self.tag_scenario_ready(true));
            assert!(
                self.engine
                    .operations
                    .labels
                    .iter()
                    .any(|label| label == "Create tag bench-tag")
            );
            self.operations.tag_scenario_label = true;
        } else {
            assert_eq!(self.operations.dialog, Some(Dialog::CreateTag));
            assert!(!self.operations.tag_scenario_submitted);
        }
        self.operations.tag_scenario_verified = true;
    }

    pub(super) fn current_branch(&self) -> String {
        self.engine
            .app
            .refs
            .as_ref()
            .and_then(|r| r.head_branch.clone())
            .unwrap_or_else(|| "detached HEAD".into())
    }
    pub(super) fn sync_operations(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_conflicts(window, cx);
        if self.operations.epoch == self.engine.operation_epoch {
            return;
        }
        self.operations.epoch = self.engine.operation_epoch;
        if self.status_panel == Some(status::Panel::History) {
            self.operations.history_scroll.scroll_to_item(
                self.engine
                    .operations
                    .labels
                    .len()
                    .saturating_sub(self.engine.operations.cursor),
                ScrollStrategy::Center,
            );
        }
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
                self.operations.draft = Draft::new(plan);
                self.operations.controls.clear();
                self.operations.plan_scroll = UniformListScrollHandle::new();
                self.operations.plan_visible = 0;
                self.operations.rebase_open = true;
                self.operations.tab = Some(Tab::Rebase);
                self.operations.dialog = None;
            }
        }
        if self.operations.rebase_submitted && !self.engine.operation_busy {
            self.operations.rebase_submitted = false;
            if !self.engine.operation_error || conflict {
                self.operations.rebase_open = false;
                self.operations.controls.clear();
                self.operations.draft = Draft::default();
            }
        }
        if self.operations.tab == Some(Tab::Rebase)
            && !self.operations.rebase_open
            && !self.engine.operations.interactive_rebase
        {
            self.operations.tab = None;
        }
        cx.notify();
    }
    pub(super) fn operation_action(
        &mut self,
        action: Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.external_merge_busy() {
            return;
        }
        let request = match action {
            Action::ResetMode(mode) => {
                self.operations.reset_mode = mode;
                None
            }
            Action::ForceDelete(force) => {
                self.operations.force_delete = force;
                None
            }
            Action::Open(dialog, target, label) => {
                self.operations.dialog = Some(dialog);
                self.operations.reset_mode = ResetMode::Mixed;
                self.operations.force_delete = false;
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
                if dialog == Dialog::CreateTag && self.operations.annotation.is_none() {
                    let annotation = cx.new(|cx| {
                        InputState::new(window, cx)
                            .multi_line(true)
                            .placeholder("Optional annotation message")
                    });
                    self.operations._annotation_subscription =
                        Some(cx.subscribe(&annotation, |_, _, event, cx| {
                            if matches!(event, InputEvent::Change) {
                                cx.notify();
                            }
                        }));
                    self.operations.annotation = Some(annotation);
                }
                if let Some(annotation) = &self.operations.annotation {
                    annotation.update(cx, |s, cx| s.set_value("", window, cx));
                }
                if matches!(dialog, Dialog::Create | Dialog::CreateTag | Dialog::Rename) {
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
                    Some(Dialog::Create) => Some(Request::CreateAndCheckout { name, target }),
                    Some(Dialog::CreateTag) => {
                        let message = self
                            .operations
                            .annotation
                            .as_ref()
                            .expect("tag dialog annotation")
                            .read(cx)
                            .value()
                            .to_string();
                        Some(Request::CreateTag {
                            name,
                            target,
                            message: (!message.trim().is_empty()).then_some(message),
                        })
                    }
                    Some(Dialog::Rename) => Some(Request::Rename {
                        old: target,
                        new: name,
                    }),
                    Some(Dialog::Delete) => Some(Request::Delete {
                        name: target,
                        force: self.operations.force_delete,
                    }),
                    Some(Dialog::Reset) => Some(Request::Reset {
                        target,
                        mode: self.operations.reset_mode,
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
                    if matches!(
                        r,
                        Request::Merge(_)
                            | Request::Rebase(_)
                            | Request::Plan(_)
                            | Request::Reset { .. }
                    ) {
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
            Action::History => {
                self.operations.history_clear = false;
                self.toggle_status_panel(status::Panel::History)
                    .then_some(Request::Inspect)
            }
            Action::CloseHistory => {
                self.status_panel = None;
                self.operations.history_clear = false;
                None
            }
            Action::ReviewClear => {
                if !self.engine.operation_busy && !self.engine.operations.labels.is_empty() {
                    self.operations.history_clear = true;
                }
                None
            }
            Action::CancelClear => {
                self.operations.history_clear = false;
                None
            }
            Action::ClearHistory => {
                if !self.operations.history_clear {
                    return;
                }
                self.operations.history_clear = false;
                Some(Request::ClearHistory)
            }
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
            Action::SaveConflict => self.conflict_save_request(cx),
            Action::SelectConflictFile(path) => self.select_conflict_file(path, window, cx),
            Action::ReloadConflict => self.reload_conflict_file(),
            Action::CherryPick(commits) => {
                if commits.is_empty()
                    || self.engine.operation_busy
                    || self.engine.operations.conflict.is_some()
                {
                    return;
                }
                let request = Request::OnBranch {
                    branch: self.current_branch(),
                    head: self
                        .engine
                        .app
                        .head_id()
                        .map(|id| id.to_string())
                        .unwrap_or_default(),
                    request: Box::new(Request::CherryPick(commits.clone())),
                };
                self.operations.confirmation = Some((
                    request,
                    format!(
                        "Cherry-pick {} commit(s) onto {} in this order?\n{}\n\nPending changes must be committed or stashed first. Conflicts pause the sequence for Continue, Skip or Abort. Successful replay can be undone in History.",
                        commits.len(),
                        self.current_branch(),
                        commits
                            .iter()
                            .map(|id| format!("• {id}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ),
                ));
                None
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
                self.operations.draft = Draft::default();
                self.operations.controls.clear();
                None
            }
            Action::ResetPlan => {
                if let Some(plan) = &self.engine.operations.plan {
                    self.operations.draft = Draft::new(plan);
                    self.operations.controls.clear();
                    self.operations.plan_visible = 0;
                    self.operations
                        .plan_scroll
                        .scroll_to_item(0, ScrollStrategy::Top);
                }
                None
            }
            Action::SetPlan(key, action) => {
                if self.engine.operation_busy || self.operations.rebase_submitted {
                    return;
                }
                self.operations.draft.set_action(key, action);
                if let Some(controls) = self.operations.controls.get(&key) {
                    controls
                        .select
                        .update(cx, |s, cx| s.set_selected_value(&action, window, cx));
                }
                if action == CommitAction::Reword {
                    self.ensure_plan_controls(key, window, cx);
                    if let Some(input) = self
                        .operations
                        .controls
                        .get(&key)
                        .and_then(|c| c.message.as_ref())
                    {
                        input.update(cx, |s, cx| s.focus(window, cx));
                    }
                }
                None
            }
            Action::MovePlan(key, direction) => {
                if self.engine.operation_busy || self.operations.rebase_submitted {
                    return;
                }
                if let Some(to) = self.operations.draft.move_entry(key, direction) {
                    self.operations
                        .plan_scroll
                        .scroll_to_item(to, ScrollStrategy::Center);
                }
                None
            }
            Action::StartPlan => {
                if self.operations.draft.errors > 0 || self.operations.draft.entries.is_empty() {
                    return;
                }
                if let Some(plan) = &self.engine.operations.plan {
                    self.operations.confirmation = Some((
                        Request::Interactive {
                            plan: plan.clone(),
                            text: self.operations.draft.text(),
                        },
                        format!(
                            "Rewrite {} using this plan, based on {}? {}. Dropped commits remain in local recovery checkpoints.",
                            plan.branch,
                            &plan.base[..12.min(plan.base.len())],
                            self.operations.draft.summary
                        ),
                    ));
                }
                None
            }
            Action::Run(
                request @ (Request::Abort
                | Request::Skip
                | Request::ApplyStash(_)
                | Request::PopStash(_)
                | Request::DropStash(_)
                | Request::Resolve {
                    choice: Resolution::Ours | Resolution::Theirs,
                    ..
                }),
            ) => {
                let text = match &request {
                    Request::Abort => {
                        if self
                            .engine
                            .operations
                            .conflict
                            .as_ref()
                            .is_some_and(|c| c.kind == "Stash apply")
                        {
                            "Abort the stash apply? Tracked conflict edits will be discarded. Untracked files and the stash will be kept.".into()
                        } else {
                            "Abort the operation? Conflict-resolution edits will be discarded."
                                .into()
                        }
                    }
                    Request::ApplyStash(stash) => format!(
                        "Apply {}? Pending changes must be committed or stashed first. The stash will be kept.",
                        stash.subject
                    ),
                    Request::PopStash(stash) => format!(
                        "Pop {}? Pending changes must be committed or stashed first. The stash is removed only after successful application; conflicts keep it.",
                        stash.subject
                    ),
                    Request::DropStash(stash) => format!(
                        "Delete {}? Its saved changes will be removed from the stash list. This action can be undone in History.",
                        stash.subject
                    ),
                    Request::Skip => "Skip this commit? Its changes will be omitted.".into(),
                    Request::Resolve { path, choice } => format!(
                        "Replace all of {path} with {} and stage it? A deleted side removes the file.",
                        if matches!(choice, Resolution::Ours)
                            == self
                                .engine
                                .operations
                                .conflict
                                .as_ref()
                                .is_some_and(|c| c.kind == "Rebase")
                        {
                            "the incoming version"
                        } else {
                            "the local version"
                        }
                    ),
                    _ => unreachable!(),
                };
                self.operations.confirmation = Some((self.bind_conflict_request(request), text));
                None
            }
            Action::Run(request) => Some(self.bind_conflict_request(request)),
        };
        if let Some(request) = request {
            if request.changes_repository() {
                self.clear_ai_feedback();
            }
            if self
                .engine
                .app
                .checkout_notice
                .as_ref()
                .is_some_and(Result::is_ok)
            {
                self.engine.app.checkout_notice = None;
            }
            let applied_stash = matches!(&request, Request::ApplyStash(_) | Request::PopStash(_));
            let refresh_stashes = applied_stash || matches!(&request, Request::DropStash(_));
            let cleanup_mutation =
                matches!(&request, Request::Cleanup(_) | Request::OptimizeStorage)
                    || (self.cleanup.open && matches!(&request, Request::MoveTo(_)));
            let read_conflict = matches!(&request, Request::ReadConflict(_));
            if self.engine.request_operation(request) {
                if cleanup_mutation {
                    self.cleanup_mutation_started();
                }
                if refresh_stashes {
                    self.stash_mutation_started();
                }
                if applied_stash {
                    self.stash_apply_started();
                }
            } else if read_conflict {
                self.defer_conflict_read();
            }
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
        if self.operations.tab == Some(Tab::Conflicts) {
            return self.conflicts_content(cx);
        }
        if self.operations.tab == Some(Tab::Rebase) && !self.engine.operations.interactive_rebase {
            return self.rebase_content(cx);
        }
        let mut content = column()
            .id("git-workflows")
            .flex_1()
            .overflow_y_scroll()
            .p_6()
            .gap_4();
        if let Some(Tab::Rebase) = self.operations.tab {
            content = content.child(
                row()
                    .child(icon("graph"))
                    .child(div().text_size(px(22.)).child("Interactive rebase")),
            );
            if self.engine.operations.interactive_rebase {
                content = content.child(caption("Rebase is in progress. Resolve or amend the current commit, then continue."))
                        .child(self.op_button("rebase-progress", "Open rebase progress", Action::ShowConflicts, cx));
            } else {
                unreachable!("draft rebase uses its own virtualized layout");
            }
        }
        content.into_any_element()
    }
    // Allocate component state only as rows become visible. Reword inputs are
    // created on demand; ordinary commits never carry an editor or text layout.
    fn ensure_plan_controls(&mut self, key: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(i) = self.operations.draft.index(key) else {
            return;
        };
        let action = self.operations.draft.entries[i].action;
        self.operations.controls.entry(key).or_insert_with(|| {
            let select = cx.new(|cx| {
                SelectState::new(
                    CommitAction::ALL.to_vec(),
                    Some(
                        gpui_component::IndexPath::default()
                            .row(CommitAction::ALL.iter().position(|a| *a == action).unwrap()),
                    ),
                    window,
                    cx,
                )
            });
            let subscription = cx.subscribe_in(&select, window, move |v, _, event, window, cx| {
                if let SelectEvent::Confirm(Some(action)) = event {
                    v.operation_action(Action::SetPlan(key, *action), window, cx);
                }
            });
            PlanControls {
                select,
                message: None,
                _subscriptions: vec![subscription],
            }
        });
        if action == CommitAction::Reword && self.operations.controls[&key].message.is_none() {
            let message = self.operations.draft.entries[i].message.clone();
            let input = cx.new(|cx| {
                let mut input = InputState::new(window, cx).placeholder("New commit message");
                input.set_value(message, window, cx);
                input
            });
            let subscription = cx.subscribe(&input, move |v, input, event, cx| {
                if matches!(event, InputEvent::Change)
                    && let Some(i) = v.operations.draft.index(key)
                {
                    v.operations.draft.entries[i].message = input.read(cx).value().to_string();
                    v.operations.draft.refresh();
                    cx.notify();
                }
            });
            let controls = self.operations.controls.get_mut(&key).unwrap();
            controls.message = Some(input);
            controls._subscriptions.push(subscription);
        }
    }
    fn rebase_content(&self, cx: &mut Context<Self>) -> AnyElement {
        let plan = self.engine.operations.plan.as_ref();
        let enabled = !self.engine.operation_busy && !self.operations.rebase_submitted;
        let valid = self.operations.draft.errors == 0 && !self.operations.draft.entries.is_empty();
        let count = self.operations.draft.entries.len();
        let list = uniform_list(
            "rebase-commits",
            count,
            cx.processor(|v, range: std::ops::Range<usize>, window, cx| {
                v.operations.plan_visible = v.operations.plan_visible.max(range.len());
                if v.operations.controls.len() > 64 {
                    let visible = &v.operations.draft.entries[range.clone()];
                    v.operations
                        .controls
                        .retain(|key, _| visible.iter().any(|e| e.key == *key));
                }
                range
                    .map(|i| {
                        let key = v.operations.draft.entries[i].key;
                        v.ensure_plan_controls(key, window, cx);
                        v.rebase_row(i, cx)
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .min_h_0()
        .h_full()
        .track_scroll(self.operations.plan_scroll.clone());
        column().flex_1().size_full().p_6().gap_4().overflow_hidden()
            .child(column().flex_shrink_0().gap_2()
                .child(row().gap_3().child(icon("graph").text_color(rgb(ACCENT)))
                    .child(div().text_size(px(22.)).child("Interactive rebase"))
                    .child(div().flex_1())
                    .child(caption(plan.map(|p| format!("{}  /  onto {}", p.branch, &p.base[..12.min(p.base.len())])).unwrap_or_default()).truncate()))
                .child(caption("Shape your history. Choose an action for each commit and use the arrows to change replay order.")))
            .child(column().flex_1().border_1().border_color(rgb(LINE)).rounded_lg().overflow_hidden()
                .child(row().flex_shrink_0().h(px(38.)).px_4().bg(rgb(CONTROL))
                    .child(caption("REPLAY ORDER · OLDEST FIRST"))
                    .child(div().flex_1())
                    .child(caption(format!("{count} commits"))))
                .child(row().flex_1().min_h_0().gap_0().child(list)
                    .child(Scrollbar::vertical(&self.operations.plan_scroll).scrollbar_show(ScrollbarShow::Always))))
            .child(column().flex_shrink_0().gap_3()
                .child(row().child(div().text_size(px(13.)).child(self.operations.draft.summary.clone()))
                    .child(div().flex_1())
                    .when(!valid, |r| r.child(div().text_size(px(12.)).text_color(rgb(REMOVED)).child(format!("Fix {} plan issue(s) before continuing", self.operations.draft.errors)))))
                .child(row()
                    .child(Button::new("reset-rebase").small().ghost().label("Reset plan").disabled(!enabled)
                        .on_click(cx.listener(|v, _, window, cx| v.operation_action(Action::ResetPlan, window, cx))))
                    .child(caption("History changes only after review and confirmation.").flex_1())
                    .child(self.op_button("close-plan", "Discard plan", Action::ClosePlan, cx))
                    .child(Button::new("start-plan").small().primary().label("Review rebase…").disabled(!enabled || !valid)
                        .on_click(cx.listener(|v, _, window, cx| v.operation_action(Action::StartPlan, window, cx))))))
            .into_any_element()
    }
    fn rebase_row(&self, i: usize, cx: &Context<Self>) -> AnyElement {
        let entry = &self.operations.draft.entries[i];
        let key = entry.key;
        let controls = &self.operations.controls[&key];
        let enabled = !self.engine.operation_busy && !self.operations.rebase_submitted;
        let color = if entry.error.is_some() || entry.action == CommitAction::Drop {
            REMOVED
        } else if entry.action == CommitAction::Pick {
            MUTED
        } else {
            ACCENT
        };
        let detail = if let Some(error) = entry.error {
            error.to_string()
        } else if entry.action.combines() {
            let target = &self.operations.draft.entries[entry.target.unwrap()];
            format!(
                "↳ {} {} · {}",
                if entry.action == CommitAction::Fixup {
                    "Fold into"
                } else {
                    "Combine with"
                },
                &target.id[..8.min(target.id.len())],
                if target.action == CommitAction::Reword {
                    target.message.trim()
                } else {
                    &target.subject
                }
            )
        } else {
            entry.action.description().into()
        };
        let mut body = column().flex_1().gap_2().child(
            row()
                .gap_3()
                .child(
                    div()
                        .font_family("Cascadia Mono")
                        .text_size(px(12.))
                        .text_color(rgb(MUTED))
                        .child(entry.id[..8.min(entry.id.len())].to_string()),
                )
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .text_color(rgb(if entry.action == CommitAction::Drop {
                            MUTED
                        } else {
                            TEXT
                        }))
                        .when(entry.action == CommitAction::Drop, |s| s.line_through())
                        .child(entry.subject.clone()),
                ),
        );
        if entry.action == CommitAction::Reword {
            body = body.child(
                row()
                    .child(caption("Message"))
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(controls.message.as_ref().unwrap())
                                .small()
                                .disabled(!enabled)
                                .w_full(),
                        ),
                    )
                    .when(entry.error.is_some(), |r| {
                        r.child(div().text_color(rgb(REMOVED)).child("Required"))
                    }),
            );
        } else {
            body = body.child(caption(detail).truncate().text_color(rgb(
                if entry.error.is_some() {
                    REMOVED
                } else {
                    MUTED
                },
            )));
        }
        row()
            .id(("rebase-row", key))
            .relative()
            .w_full()
            .h(px(92.))
            .px_4()
            .gap_4()
            .border_b_1()
            .border_color(rgb(INSPECTOR_LINE))
            .bg(rgb(if entry.error.is_some() {
                REMOVED_BG
            } else {
                PANEL
            }))
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(2.))
                    .bg(rgb(color)),
            )
            .child(
                div()
                    .w(px(24.))
                    .flex_shrink_0()
                    .text_size(px(12.))
                    .text_color(rgb(MUTED))
                    .child(format!("{:02}", i + 1)),
            )
            .child(
                div().w(px(126.)).flex_shrink_0().child(
                    Select::new(&controls.select)
                        .small()
                        .w_full()
                        .menu_width(px(420.))
                        .disabled(!enabled),
                ),
            )
            .child(body)
            .child(
                row()
                    .flex_shrink_0()
                    .gap_1()
                    .child(
                        Button::new(("rebase-up", key))
                            .small()
                            .ghost()
                            .label("↑")
                            .tooltip("Move earlier in replay order")
                            .disabled(!enabled || i == 0)
                            .on_click(cx.listener(move |v, _, window, cx| {
                                v.operation_action(Action::MovePlan(key, -1), window, cx)
                            })),
                    )
                    .child(
                        Button::new(("rebase-down", key))
                            .small()
                            .ghost()
                            .label("↓")
                            .tooltip("Move later in replay order")
                            .disabled(!enabled || i + 1 == self.operations.draft.entries.len())
                            .on_click(cx.listener(move |v, _, window, cx| {
                                v.operation_action(Action::MovePlan(key, 1), window, cx)
                            })),
                    ),
            )
            .into_any_element()
    }
    pub(super) fn rebase_scenario_ready(&self) -> bool {
        self.operations.rebase_open
            && !self.engine.operation_busy
            && self.operations.draft.entries.len() >= 3
    }
    pub(super) fn rebase_scenario_step(
        &mut self,
        step: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.scenario.as_deref() == Some("rebase-scroll") {
            self.operations.plan_scroll.scroll_to_item(
                (step * 37) % self.operations.draft.entries.len(),
                ScrollStrategy::Top,
            );
        } else {
            self.operation_action(Action::SetPlan(1, CommitAction::ALL[step % 6]), window, cx);
            self.operation_action(
                Action::MovePlan(1, if step.is_multiple_of(2) { 1 } else { -1 }),
                window,
                cx,
            );
            if let Some(input) = self
                .operations
                .controls
                .get(&1)
                .and_then(|c| c.message.clone())
            {
                input.update(cx, |s, cx| {
                    s.set_value(format!("Reworded commit {step}"), window, cx)
                });
            }
        }
    }
    pub(super) fn operations_overlay(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if let Some(dialog) = self.operations.dialog {
            let op = &self.operations;
            let (title, detail, submit) = match dialog {
                Dialog::Create => (
                    "Create branch",
                    format!(
                        "Create and switch to a local branch at {}. Pending changes will be carried over when safe; Git will refuse if files would be overwritten.",
                        op.label
                    ),
                    "Create and checkout",
                ),
                Dialog::CreateTag => (
                    "Create tag",
                    format!(
                        "Tag {} locally. Add a message to create an annotated tag.",
                        op.label
                    ),
                    "Create tag",
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
                        "Remove local branch {}. Checked-out branches cannot be deleted. Its remote branch stays intact. Undo can restore the local branch.",
                        op.label
                    ),
                    "Delete branch",
                ),
                Dialog::Reset => (
                    "Reset current branch",
                    format!(
                        "Move {} to {}. Choose how to handle the index and working files. Undo can restore the previous state.",
                        op.current_branch, op.label
                    ),
                    "Reset branch",
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
            let mut body = dialog_body(title)
                .relative()
                .children(self.graph_context_probe(9));
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
                if dialog == Dialog::Delete {
                    body = body.child(
                        Checkbox::new("delete-unmerged")
                            .checked(op.force_delete)
                            .label("Delete even if the branch has unmerged commits")
                            .on_click(cx.listener(|v, checked: &bool, w, cx| {
                                v.operation_action(Action::ForceDelete(*checked), w, cx)
                            })),
                    );
                }
                if dialog == Dialog::Reset {
                    for (mode, title, description) in [
                        (
                            ResetMode::Soft,
                            "Soft",
                            "Keep staged changes and working files.",
                        ),
                        (
                            ResetMode::Mixed,
                            "Mixed",
                            "Unstage changes and keep working files.",
                        ),
                        (
                            ResetMode::Hard,
                            "Hard",
                            "Discard staged and working changes. Untracked files in the way may be removed.",
                        ),
                    ] {
                        body = body.child(
                            column()
                                .gap_1()
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "reset-{}",
                                        mode.flag()
                                    )))
                                    .label(title)
                                    .selected(op.reset_mode == mode)
                                    .on_click(cx.listener(move |v, _, w, cx| {
                                        v.operation_action(Action::ResetMode(mode), w, cx)
                                    })),
                                )
                                .child(caption(description)),
                        );
                    }
                }
                if matches!(dialog, Dialog::Create | Dialog::CreateTag | Dialog::Rename) {
                    body = body.child(Input::new(&op.name));
                }
                if dialog == Dialog::CreateTag {
                    body = body.child(caption("Annotation (optional)")).child(
                        Input::new(op.annotation.as_ref().expect("tag dialog annotation"))
                            .h(px(100.)),
                    );
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
                    Dialog::Create | Dialog::CreateTag | Dialog::Rename => {
                        !op.name.read(cx).value().trim().is_empty()
                    }
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
                            .bg(rgb(if enabled { ACCENT } else { CONTROL }))
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
                            .relative()
                            .children(self.todo_probe(7))
                            .children(self.branch_cleanup_probe(6))
                            .children(self.graph_context_probe(4))
                            .children(self.conflicts_probe(9))
                            .w(px(600.))
                            .p_6()
                            .gap_4()
                            .rounded_lg()
                            .bg(rgb(PANEL))
                            .border_1()
                            .border_color(rgb(LINE))
                            .child(div().text_size(px(20.)).child("Review Git operation"))
                            .child(
                                div()
                                    .id("operation-review-references")
                                    .max_h(px(360.))
                                    .overflow_y_scroll()
                                    .child(text.clone()),
                            )
                            .child(
                                dialog_actions()
                                    .child(self.op_button(
                                        "operation-cancel",
                                        "Cancel",
                                        Action::Cancel,
                                        cx,
                                    ))
                                    .child(
                                        Button::new("operation-confirm")
                                            .primary()
                                            .label("Confirm")
                                            .disabled(
                                                self.engine.operation_busy
                                                    || self.engine.repository_edit_busy
                                                    || self.engine.app.pending.busy
                                                    || self.engine.app.branches.busy
                                                    || self.engine.app.settings.saving
                                                    || self.external_merge_busy(),
                                            )
                                            .on_click(cx.listener(|v, _, w, cx| {
                                                v.operation_action(Action::Confirm, w, cx)
                                            })),
                                    ),
                            ),
                    )
                    .into_any_element(),
            );
        }
        if self.status_panel != Some(status::Panel::History) {
            return None;
        }
        Some(self.history_panel(cx))
    }
}
