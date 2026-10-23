//! AI actions share one cancellable worker and retain reviewable drafts.
use super::*;
use crate::ai::{self, Provider, Task};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
#[path = "ai_scenarios.rs"]
mod scenarios;

pub(super) const SETTINGS_PAGE: usize = 4;

#[derive(Clone)]
pub(super) enum Action {
    Run(Task),
    Rebase(String, String),
    Merge(String, String),
    Provider(Provider),
    Toggle(usize),
    Save,
    Cancel,
    Close,
    Copy,
}
enum Reply {
    Loaded(Result<ai::Settings, String>),
    Saved(Result<ai::Settings, String>),
    Progress(String),
    Integrated(Result<String, String>),
    Answer(Result<String, String>),
}
enum Destination {
    Summary,
    Commit {
        description: String,
        fingerprint: u64,
        files: Vec<diff::FileChange>,
    },
    Conflict {
        identity: String,
        path: String,
        text: String,
    },
}
pub(super) struct State {
    scenario: scenarios::State,
    settings: Option<ai::Settings>,
    provider: Provider,
    model: Entity<InputState>,
    prompts: Vec<Entity<InputState>>,
    expanded: [bool; 5],
    _subscriptions: Vec<Subscription>,
    receiver: Option<mpsc::Receiver<Reply>>,
    cancel: Arc<AtomicBool>,
    destination: Option<Destination>,
    repository: String,
    busy: bool,
    loading: bool,
    saving: bool,
    integrating: Option<Task>,
    feedback: bool,
    status: String,
    error: bool,
    result: String,
    result_title: String,
    output: Entity<TextBlock>,
    pub(super) show_result: bool,
}
impl State {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        let model =
            cx.new(|cx| InputState::new(window, cx).placeholder("Leave empty for the CLI default"));
        let prompts = Task::ALL
            .iter()
            .map(|_| cx.new(|cx| InputState::new(window, cx).multi_line(true)))
            .collect::<Vec<_>>();
        let subscriptions = std::iter::once(&model)
            .chain(prompts.iter())
            .map(|input| {
                cx.subscribe(input, |_, _, event, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                })
            })
            .collect();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(Reply::Loaded(ai::Settings::load()));
        });
        Self {
            scenario: Default::default(),
            settings: None,
            provider: Provider::Codex,
            model,
            prompts,
            expanded: [true, false, false, false, false],
            _subscriptions: subscriptions,
            receiver: Some(rx),
            cancel: Arc::default(),
            destination: None,
            repository: String::new(),
            busy: false,
            loading: true,
            saving: false,
            integrating: None,
            feedback: false,
            status: "Loading AI settings…".into(),
            error: false,
            result: String::new(),
            result_title: String::new(),
            output: cx.new(TextBlock::new),
            show_result: false,
        }
    }
    fn draft(&self, cx: &App) -> ai::Settings {
        ai::Settings {
            provider: self.provider,
            model: self.model.read(cx).value().to_string(),
            prompts: std::array::from_fn(|i| self.prompts[i].read(cx).value().to_string()),
        }
    }
}
impl Drop for State {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

impl Desktop {
    pub(super) fn ai_repository_changed(&mut self) {
        self.ai.cancel.store(true, Ordering::Release);
        self.ai.show_result = false;
    }
    pub(super) fn ai_action(
        &mut self,
        action: Action,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            Action::Provider(provider) if !self.ai.loading && !self.ai.saving => {
                self.ai.provider = provider
            }
            Action::Toggle(i) if i < Task::ALL.len() => self.ai.expanded[i] = !self.ai.expanded[i],
            Action::Cancel => {
                self.ai.cancel.store(true, Ordering::Release);
                self.ai.status = "Cancelling AI request…".into();
            }
            Action::Close => self.ai.show_result = false,
            Action::Copy => {
                cx.write_to_clipboard(ClipboardItem::new_string(self.ai.result.clone()))
            }
            Action::Save if !self.ai.loading && !self.ai.saving && !self.ai.busy => {
                let settings = self.ai.draft(cx);
                if let Err(error) = settings.validate() {
                    self.ai.status = error;
                    self.ai.error = true;
                    cx.notify();
                    return;
                }
                self.ai.saving = true;
                self.ai.status = "Saving AI settings…".into();
                self.ai.error = false;
                let (tx, rx) = mpsc::channel();
                self.ai.receiver = Some(rx);
                std::thread::spawn(move || {
                    let result = settings.save().map(|_| settings);
                    let _ = tx.send(Reply::Saved(result));
                });
            }
            Action::Rebase(target, label) if self.ai_available() => {
                self.context_menu = None;
                self.context_menu_group = None;
                self.start_ai_integration(Task::Rebase, target, label, cx);
            }
            Action::Merge(target, label) if self.ai_available() => {
                self.context_menu = None;
                self.context_menu_group = None;
                self.start_ai_integration(Task::Merge, target, label, cx);
            }
            Action::Run(task) if self.ai_available() => self.start_ai(task, cx),
            _ => {}
        }
        cx.notify();
    }
    pub(super) fn ai_available(&self) -> bool {
        self.ai.settings.is_some()
            && !self.ai.busy
            && !self.ai.saving
            && !self.ai.loading
            && !self.ai.show_result
            && !self.updates.open
            && !self.operations.modal_open()
            && !self.ref_storage.confirming
            && !self.engine.operation_busy
            && !self.engine.repository_edit_busy
            && !self.engine.app.pending.busy
            && !self.external_merge_busy()
    }
    pub(super) fn ai_button(
        &self,
        id: &'static str,
        label: &'static str,
        task: Task,
        cx: &Context<Self>,
    ) -> AnyElement {
        div()
            .relative()
            .children(self.ai_probe(task.index() + 1))
            .child(
                Button::new(id)
                    .small()
                    .ghost()
                    .icon(gpui_component::Icon::default().path("rustle/sparkles"))
                    .label(label)
                    .disabled(!self.ai_available())
                    .on_click(cx.listener(move |v, _, window, cx| {
                        v.ai_action(Action::Run(task), window, cx)
                    })),
            )
            .into_any_element()
    }
    fn start_ai_integration(
        &mut self,
        task: Task,
        target: String,
        label: String,
        cx: &mut Context<Self>,
    ) {
        let Some(branch) = self
            .engine
            .app
            .refs
            .as_ref()
            .and_then(|r| r.head_branch.clone())
        else {
            self.show_ai_result(
                task.label(),
                "Check out a branch before merging or rebasing.".into(),
                true,
                cx,
            );
            return;
        };
        let Some(head) = self.engine.app.head_id().map(|id| id.to_string()) else {
            return;
        };
        if self.engine.operations.conflict.is_some() || !self.engine.begin_repository_edit() {
            return;
        }
        self.ai.busy = true;
        self.ai.integrating = Some(task);
        self.ai.feedback = true;
        self.ai.error = false;
        self.ai.repository = self.engine.repository_path();
        self.ai.cancel = Arc::default();
        self.ai.status = if task == Task::Merge {
            format!("AI merge · merging {label} into {branch}…")
        } else {
            format!("AI rebase · rebasing {branch} onto {label}…")
        };
        let settings = self.ai.settings.clone().unwrap();
        let cancel = self.ai.cancel.clone();
        let root = std::path::PathBuf::from(&self.ai.repository);
        let (tx, rx) = mpsc::channel();
        self.ai.receiver = Some(rx);
        std::thread::spawn(move || {
            let result = ai::integrate::run(
                &root,
                &settings,
                &ai::integrate::Target {
                    task,
                    branch,
                    head,
                    target,
                },
                cancel,
                |status| {
                    let _ = tx.send(Reply::Progress(status.into()));
                },
            );
            let _ = tx.send(Reply::Integrated(result));
        });
        cx.notify();
    }
    pub(super) fn clear_ai_feedback(&mut self) {
        if !self.ai.busy {
            self.ai.feedback = false;
            self.ai.error = false;
        }
    }
    pub(super) fn ai_status(&self) -> Option<(&str, bool, bool)> {
        (self.ai.feedback || self.ai.busy).then_some((
            self.ai.status.as_str(),
            self.ai.busy,
            self.ai.error,
        ))
    }
    pub(super) fn ai_status_controls(&self, cx: &Context<Self>) -> Option<AnyElement> {
        self.ai.busy.then(|| {
            Button::new("cancel-ai")
                .xsmall()
                .ghost()
                .label("Cancel AI")
                .on_click(cx.listener(|v, _, w, cx| v.ai_action(Action::Cancel, w, cx)))
                .into_any_element()
        })
    }
    fn start_ai(&mut self, task: Task, cx: &mut Context<Self>) {
        let prepared = self.prepare_ai(task, cx);
        let (context, destination, files) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.show_ai_result(task.label(), error, true, cx);
                return;
            }
        };
        self.ai.destination = Some(destination);
        self.ai.repository = self.engine.repository_path();
        self.ai.cancel = Arc::default();
        self.ai.busy = true;
        self.ai.feedback = true;
        self.ai.error = false;
        self.ai.status = format!(
            "{} · {} is working…",
            task.label(),
            self.ai.settings.as_ref().unwrap().provider.label()
        );
        let settings = self.ai.settings.clone().unwrap();
        let cancel = self.ai.cancel.clone();
        let root = std::path::PathBuf::from(&self.ai.repository);
        let conflict_path = if task == Task::Conflict {
            self.conflict_selected_path().map(str::to_owned)
        } else {
            None
        };
        let (tx, rx) = mpsc::channel();
        self.ai.receiver = Some(rx);
        std::thread::spawn(move || {
            let result = (|| {
                let mut context = context;
                if let Some(path) = conflict_path {
                    let spec = format!(":1:{path}");
                    if let Ok(base) = crate::git::command::checked(&root, &["show", &spec]) {
                        context.push_str("\n--- MERGE BASE ---\n");
                        context.push_str(
                            std::str::from_utf8(&base).map_err(|_| "Merge base is not UTF-8")?,
                        );
                    }
                }
                if task == Task::Summary {
                    context.push_str(&ai::diff_context(&root, &files, &cancel)?);
                } else if !files.is_empty() {
                    let repo = gix::open(&root).map_err(|e| e.to_string())?;
                    for file in files {
                        if cancel.load(Ordering::Acquire) {
                            return Err("AI request cancelled".into());
                        }
                        let (old, new) =
                            diff::external_contents(&repo, &file).map_err(|e| e.to_string())?;
                        context.push_str(&ai::file_context(&file.path, &old, &new)?);
                        if context.len() > ai::MAX_CONTEXT {
                            return Err(
                                "Selected changes exceed 256 KiB. Select fewer files.".into()
                            );
                        }
                    }
                    if task == Task::Commit
                        && let Ok(subjects) =
                            crate::git::command::checked(&root, &["log", "-5", "--format=%s"])
                    {
                        context.push_str("\nRecent commit subjects:\n");
                        context.push_str(&String::from_utf8_lossy(&subjects));
                    }
                }
                ai::run(&settings, task, &context, cancel)
            })();
            let _ = tx.send(Reply::Answer(result));
        });
    }
    fn prepare_ai(
        &self,
        task: Task,
        cx: &Context<Self>,
    ) -> Result<(String, Destination, Vec<diff::FileChange>), String> {
        match task {
            Task::Summary => {
                let Mode::Diff(view) = &self.engine.app.mode else {
                    return Err("Open a diff first".into());
                };
                if !view.changes.loaded {
                    return Err("Wait for the diff's file list to load".into());
                }
                if let Some(error) = &view.changes.error {
                    return Err(format!("Cannot summarize this diff: {error}"));
                }
                if view.changes.files.is_empty() {
                    return Err("This diff has no changed files to summarize".into());
                }
                Ok((
                    format!(
                        "Base: {}\nTarget: {}\n",
                        view.changes
                            .old
                            .map(|id| id.to_string())
                            .unwrap_or_else(|| "empty tree".into()),
                        if view.changes.files.iter().any(|file| file.worktree) {
                            "working tree".into()
                        } else {
                            view.changes.new.to_string()
                        }
                    ),
                    Destination::Summary,
                    view.changes.files.as_ref().clone(),
                ))
            }
            Task::Commit => {
                if !self.engine.app.pending.selected {
                    return Err("Select pending changes first".into());
                }
                let changes = self
                    .engine
                    .app
                    .active_changes()
                    .ok_or("Wait for pending changes to load")?;
                let files = changes
                    .files
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| changes.checked.as_ref().is_some_and(|c| c[*i]))
                    .map(|(_, f)| f.clone())
                    .collect::<Vec<_>>();
                if files.is_empty() {
                    return Err("Check at least one file to describe".into());
                }
                if self.engine.app.pending.snapshot.conflicts {
                    return Err("Resolve conflicts before creating a commit description".into());
                }
                Ok((
                    String::new(),
                    Destination::Commit {
                        description: self.description.read(cx).value().to_string(),
                        fingerprint: self.engine.app.pending.snapshot.fingerprint,
                        files: files.clone(),
                    },
                    files,
                ))
            }
            Task::Conflict => {
                let conflict = self
                    .engine
                    .operations
                    .conflict
                    .as_ref()
                    .ok_or("No conflict is active")?;
                let path = self
                    .conflict_selected_path()
                    .ok_or("Select a conflicted file")?
                    .to_string();
                if !conflict.files.contains(&path) || self.conflict_editor_stale() {
                    return Err("Select an unresolved file and reload any stale draft first".into());
                }
                let preview = self
                    .engine
                    .operations
                    .preview
                    .as_ref()
                    .filter(|p| p.path == path && p.identity == conflict.identity)
                    .ok_or("Wait for conflict content to load")?;
                let text = self
                    .conflict_editor_value(cx)
                    .ok_or("AI resolution requires a text file")?;
                let (ours_label, theirs_label) = if conflict.kind == "Rebase" {
                    (&conflict.incoming_label, &conflict.local_label)
                } else {
                    (&conflict.local_label, &conflict.incoming_label)
                };
                let context = format!(
                    "Operation: {}\nFile: {}\nOurs: {}\nTheirs: {}\nFor a rebase, ours is the destination branch and theirs is the replayed commit.\n--- OURS ---\n{}\n--- THEIRS ---\n{}\n--- CURRENT DRAFT ---\n{}",
                    conflict.kind,
                    path,
                    ours_label,
                    theirs_label,
                    preview.ours,
                    preview.theirs,
                    text
                );
                Ok((
                    context,
                    Destination::Conflict {
                        identity: conflict.identity.clone(),
                        path,
                        text,
                    },
                    vec![],
                ))
            }
            Task::Rebase | Task::Merge => {
                Err("Use the automatic merge or rebase context-menu action".into())
            }
        }
    }
    pub(super) fn ai_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ai.busy && self.ai.repository != self.engine.repository_path() {
            self.ai.cancel.store(true, Ordering::Release);
        }
        let reply = loop {
            let Some(rx) = self.ai.receiver.as_ref() else {
                break None;
            };
            match rx.try_recv() {
                Ok(Reply::Progress(status)) => {
                    self.ai.status = status;
                    cx.notify();
                }
                Ok(reply) => break Some(reply),
                Err(mpsc::TryRecvError::Disconnected) => {
                    break Some(if self.ai.loading {
                        Reply::Loaded(Err("AI settings worker stopped".into()))
                    } else if self.ai.saving {
                        Reply::Saved(Err("AI settings worker stopped".into()))
                    } else if self.ai.integrating.is_some() {
                        Reply::Integrated(Err(
                            "AI integration worker stopped unexpectedly. Any active operation is paused."
                                .into(),
                        ))
                    } else {
                        Reply::Answer(Err("AI worker stopped unexpectedly".into()))
                    });
                }
                Err(mpsc::TryRecvError::Empty) => break None,
            }
        };
        if let Some(reply) = reply {
            self.ai.receiver = None;
            match reply {
                Reply::Progress(_) => unreachable!(),
                Reply::Integrated(result) => {
                    self.ai.busy = false;
                    let task = self.ai.integrating.take().unwrap_or(Task::Merge);
                    self.engine.end_repository_edit();
                    self.engine.tick(None);
                    match result {
                        Ok(message) => {
                            self.ai.status = message;
                            self.ai.error = false;
                        }
                        Err(error) if self.ai.cancel.load(Ordering::Acquire) => {
                            self.ai.status = error;
                            self.ai.error = false;
                        }
                        Err(error) => self.show_ai_result(
                            if task == Task::Merge {
                                "AI merge stopped"
                            } else {
                                "AI rebase stopped"
                            },
                            error,
                            true,
                            cx,
                        ),
                    }
                }
                Reply::Loaded(result) => {
                    self.ai.loading = false;
                    match result {
                        Ok(settings) => {
                            self.ai.provider = settings.provider;
                            self.ai.model.update(cx, |s, cx| {
                                s.set_value(settings.model.clone(), window, cx)
                            });
                            for (input, value) in self.ai.prompts.iter().zip(&settings.prompts) {
                                input.update(cx, |s, cx| s.set_value(value.clone(), window, cx));
                            }
                            self.ai.settings = Some(settings);
                            self.ai.status = "AI settings apply to all repositories.".into();
                        }
                        Err(error) => {
                            self.ai.error = true;
                            self.ai.status = error;
                            let settings = ai::Settings::default();
                            for (input, value) in self.ai.prompts.iter().zip(&settings.prompts) {
                                input.update(cx, |s, cx| s.set_value(value.clone(), window, cx));
                            }
                        }
                    }
                }
                Reply::Saved(result) => {
                    self.ai.saving = false;
                    match result {
                        Ok(settings) => {
                            self.ai.settings = Some(settings);
                            self.ai.status = "AI settings saved for all repositories.".into();
                            self.ai.error = false;
                        }
                        Err(error) => {
                            self.ai.status = error;
                            self.ai.error = true;
                        }
                    }
                }
                Reply::Answer(result) => {
                    self.ai.busy = false;
                    if self.ai.repository != self.engine.repository_path()
                        || self.ai.cancel.load(Ordering::Acquire)
                    {
                        self.ai.destination = None;
                        self.ai.status = "AI request cancelled.".into();
                    } else {
                        let destination = self.ai.destination.take();
                        match result {
                            Ok(text) => self.apply_ai(destination, text, window, cx),
                            Err(error) => self.show_ai_result("AI request failed", error, true, cx),
                        }
                    }
                }
            }
            cx.notify();
        }
    }
    fn apply_ai(
        &mut self,
        destination: Option<Destination>,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = match destination {
            Some(Destination::Summary) => {
                self.show_ai_result("Whole diff summary", text, false, cx);
                return;
            }
            Some(Destination::Commit {
                description,
                fingerprint,
                files,
            }) => {
                let current_files = self
                    .engine
                    .app
                    .active_changes()
                    .map(|cs| {
                        cs.files
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| cs.checked.as_ref().is_some_and(|c| c[*i]))
                            .map(|(_, f)| f.clone())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if self.engine.app.pending.selected
                    && self.engine.app.pending.snapshot.fingerprint == fingerprint
                    && current_files == files
                    && self.description.read(cx).value().as_ref() == description
                {
                    let text = text.trim().to_string();
                    self.engine.app.pending.description.value = text.clone();
                    self.draft_text = text.clone();
                    self.description
                        .update(cx, |s, cx| s.set_value(text, window, cx));
                    Ok("Commit description filled. Review it before committing.")
                } else {
                    self.show_ai_result(
                        "Changes or description changed · generated message",
                        text,
                        false,
                        cx,
                    );
                    return;
                }
            }
            Some(Destination::Conflict {
                identity,
                path,
                text: original,
            }) => {
                if text
                    .lines()
                    .any(|l| l.starts_with("<<<<<<<") || l == "=======" || l.starts_with(">>>>>>>"))
                    || text.starts_with("```")
                {
                    Err(
                        "The AI response still contains conflict markers or a code fence. Try again or edit the file manually.",
                    )
                } else if self
                    .engine
                    .operations
                    .conflict
                    .as_ref()
                    .is_some_and(|c| c.identity == identity && c.files.contains(&path))
                    && self.conflict_selected_path() == Some(path.as_str())
                    && self.conflict_editor_value(cx).as_deref() == Some(original.as_str())
                    && !self.conflict_editor_stale()
                {
                    self.conflict_editor_edit(&text, window, cx);
                    Ok("AI resolution drafted. Review it, then save and mark resolved.")
                } else {
                    self.show_ai_result("Conflict changed · suggested resolution", text, false, cx);
                    return;
                }
            }
            None => return,
        };
        match result {
            Ok(message) => {
                self.ai.status = message.into();
                self.ai.error = false;
            }
            Err(error) => self.show_ai_result("AI result needs attention", error.into(), true, cx),
        }
    }
    fn show_ai_result(&mut self, title: &str, text: String, error: bool, cx: &mut Context<Self>) {
        self.ai.feedback = true;
        self.ai.result_title = title.into();
        self.ai.result = text.clone();
        self.ai.output.update(cx, |s, cx| s.set(text, "", cx));
        self.ai.show_result = true;
        self.ai.error = error;
        self.ai.status = if error {
            "AI request failed. See the result for details."
        } else {
            "AI result ready."
        }
        .into();
    }
    pub(super) fn ai_settings(&self, cx: &Context<Self>) -> AnyElement {
        let disabled = self.ai.loading || self.ai.saving;
        let save = Button::new("save-ai-settings")
            .small()
            .primary()
            .icon(gpui_component::Icon::default().path("rustle/sparkles"))
            .label(if self.ai.saving {
                "Saving…"
            } else {
                "Save AI settings"
            })
            .disabled(disabled || self.ai.busy)
            .on_click(cx.listener(|v, _, w, cx| v.ai_action(Action::Save, w, cx)));
        let mut form = column().w_full().gap_3()
            .child(row().gap_3().child(caption("Global AI settings · shared by all repositories").flex_1()).child(save))
            .child(div().text_size(px(12.)).text_color(rgb(if self.ai.error { REMOVED } else { MUTED })).child(self.ai.status.clone()))
            .child(caption("Provider"))
            .child(row().gap_2().children([Provider::Codex, Provider::Claude].into_iter().enumerate().map(|(i, provider)| {
                Button::new(("ai-provider", i)).small().label(provider.label()).selected(self.ai.provider == provider).disabled(disabled)
                    .on_click(cx.listener(move |v, _, w, cx| v.ai_action(Action::Provider(provider), w, cx)))
            })))
            .child(caption("Install your CLI and sign in in a terminal. Git Rustle uses its existing authentication."))
            .child(caption("Model name"))
            .child(div().relative().children(self.ai_probe(0)).child(Input::new(&self.ai.model).disabled(disabled)))
            .child(caption("Markdown prompts · additional context is supplied automatically"));
        for task in Task::ALL {
            let i = task.index();
            let expanded = self.ai.expanded[i];
            form = form.child(
                column()
                    .w_full()
                    .flex_shrink_0()
                    .border_1()
                    .border_color(rgb(LINE))
                    .rounded_md()
                    .overflow_hidden()
                    .child(
                        row()
                            .id(("ai-prompt-toggle", i))
                            .w_full()
                            .p_3()
                            .cursor_pointer()
                            .bg(rgb(CONTROL))
                            .gap_2()
                            .child(icon(if expanded { "down" } else { "right" }))
                            .child(icon("sparkles").text_color(rgb(ACCENT)))
                            .child(task.label())
                            .on_click(cx.listener(move |v, _, w, cx| {
                                v.ai_action(Action::Toggle(i), w, cx)
                            })),
                    )
                    .when(expanded, |s| {
                        s.child(
                            div().p_3().child(
                                Input::new(&self.ai.prompts[i])
                                    .h(px(180.))
                                    .disabled(disabled),
                            ),
                        )
                    }),
            );
        }
        form.into_any_element()
    }
    pub(super) fn ai_overlay(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if !self.ai.show_result {
            return None;
        }
        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x00000099))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    column()
                        .relative()
                        .children(self.ai_probe(5))
                        .w(px(760.))
                        .max_w(relative(0.9))
                        .max_h(relative(0.85))
                        .p_5()
                        .gap_3()
                        .rounded_lg()
                        .bg(rgb(PANEL))
                        .border_1()
                        .border_color(rgb(LINE))
                        .child(
                            row()
                                .gap_2()
                                .child(icon("sparkles").text_color(rgb(ACCENT)))
                                .child(self.ai.result_title.clone())
                                .child(div().flex_1())
                                .child(
                                    Button::new("copy-ai-result")
                                        .small()
                                        .ghost()
                                        .label("Copy")
                                        .on_click(cx.listener(|v, _, w, cx| {
                                            v.ai_action(Action::Copy, w, cx)
                                        })),
                                ),
                        )
                        .child(div().h(px(390.)).min_h_0().child(self.ai.output.clone()))
                        .child(
                            row().justify_end().child(
                                Button::new("close-ai-result")
                                    .small()
                                    .primary()
                                    .label("Close")
                                    .on_click(
                                        cx.listener(|v, _, w, cx| {
                                            v.ai_action(Action::Close, w, cx)
                                        }),
                                    ),
                            ),
                        ),
                )
                .into_any_element(),
        )
    }
}
