//! Explicit repository-local conversion; discovery and Git run on workers.
use super::*;
use crate::git::refs::BackendInfo;

#[derive(Clone)]
pub(super) enum Action {
    Review,
    Confirm,
    Cancel,
}

#[derive(Default)]
pub(super) struct State {
    info: Option<BackendInfo>,
    loading: bool,
    review_requested: bool,
    pub confirming: bool,
    busy: bool,
    message: String,
    failed: bool,
}

impl Desktop {
    pub(super) fn ref_storage_ready(&self) -> bool {
        self.ref_storage.info.is_some()
            && !self.ref_storage.loading
            && !self.ref_storage.busy
            && !self.ref_storage.failed
    }
    pub(super) fn ref_storage_enabled(&self) -> bool {
        self.ref_storage_ready()
            && !self.ref_storage.confirming
            && self
                .ref_storage
                .info
                .as_ref()
                .is_some_and(|info| info.reftable)
    }
    pub(super) fn load_ref_storage(&mut self, cx: &mut Context<Self>) {
        if self.ref_storage.loading || self.ref_storage.info.is_some() {
            return;
        }
        let Some(path) = self
            .engine
            .repository_root()
            .map(std::path::Path::to_path_buf)
        else {
            return;
        };
        self.ref_storage.loading = true;
        self.ref_storage.failed = false;
        cx.spawn(async move |view, cx| {
            let source = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { crate::git::refs::inspect_backend(&path) })
                .await;
            let _ = view.update(cx, |v, cx| {
                if v.engine.repository_root() != Some(source.as_path()) {
                    return;
                }
                v.ref_storage.loading = false;
                match result {
                    Ok(info) => {
                        v.ref_storage.confirming =
                            v.ref_storage.review_requested && !info.reftable && info.can_migrate;
                        v.ref_storage.info = Some(info);
                    }
                    Err(error) => {
                        v.ref_storage.message = error;
                        v.ref_storage.failed = true;
                    }
                }
                v.ref_storage.review_requested = false;
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn ref_storage_action(&mut self, action: Action, cx: &mut Context<Self>) {
        match action {
            Action::Cancel if !self.ref_storage.busy => self.ref_storage.confirming = false,
            Action::Review
                if !self.ref_storage.loading
                    && !self.engine.repository_edit_busy
                    && !self.operations.modal_open()
                    && !self.updates.open
                    && self.engine.operations.conflict.is_none() =>
            {
                if self.ref_storage.info.is_none() {
                    self.ref_storage.review_requested = true;
                    self.ref_storage.message.clear();
                    self.load_ref_storage(cx);
                } else {
                    self.ref_storage.confirming = self
                        .ref_storage
                        .info
                        .as_ref()
                        .is_some_and(|info| !info.reftable && info.can_migrate);
                }
            }
            Action::Confirm if self.ref_storage.confirming && !self.ref_storage.busy => {
                if self.engine.operations.conflict.is_some() || !self.engine.begin_repository_edit()
                {
                    self.ref_storage.message =
                        "Wait for the active repository operation to finish, then try again."
                            .into();
                    self.ref_storage.failed = true;
                    cx.notify();
                    return;
                }
                self.ref_storage.busy = true;
                let path = self.engine.repository_root().unwrap().to_path_buf();
                cx.spawn(async move |view, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move { crate::git::refs::migrate_reftable(&path).map(|_| ()) })
                        .await;
                    let _ = view.update(cx, |v, cx| {
                        v.engine.end_repository_edit();
                        v.ref_storage.busy = false;
                        v.ref_storage.confirming = false;
                        v.ref_storage.failed = result.is_err();
                        v.ref_storage.message = match result {
                            Ok(()) => {
                                if let Some(info) = &mut v.ref_storage.info {
                                    info.reftable = true;
                                }
                                "Case-sensitive branch storage is enabled for this repository."
                                    .into()
                            }
                            Err(error) => error,
                        };
                        cx.notify();
                    });
                })
                .detach();
            }
            _ => {}
        }
        cx.notify();
    }

    pub(super) fn ref_storage_controls(&self, cx: &Context<Self>) -> Div {
        let ready = self.ref_storage.info.as_ref();
        let already_enabled = self.engine.reference_storage_reftable();
        column().mt_4().pt_3().gap_2().border_t_1().border_color(rgb(LINE))
            .child("Branch name compatibility")
            .child(caption("Case-sensitive storage allows names such as Feature and feature to coexist safely on Windows."))
            .when(ready.is_none() && already_enabled, |panel| panel.child(caption("Case-sensitive references enabled.")))
            .when(ready.is_none() && !already_enabled, |panel| panel.child(
                Button::new("check-case-sensitive-branches").small().label("Enable case-sensitive branch names…")
                    .disabled(self.ref_storage.loading || self.engine.repository_edit_busy || self.engine.operation_busy || self.engine.operations.conflict.is_some())
                    .on_click(cx.listener(|v, _, window, cx| v.action(super::Action::RefStorage(Action::Review), window, cx))),
            ))
            .when(self.ref_storage.loading, |panel| panel.child(caption("Checking Git support…")))
            .when_some(ready, |panel, info| {
                panel.child(caption(format!("{} · {}", info.git_version,
                    if info.reftable { "case-sensitive references enabled" } else { "standard reference files" })))
                    .when(!info.reftable, |panel| panel.child(
                        Button::new("enable-case-sensitive-branches").small().label("Enable case-sensitive branch names…")
                            .disabled(!info.can_migrate || self.engine.repository_edit_busy || self.engine.operation_busy || self.engine.app.settings.busy || self.engine.operations.conflict.is_some())
                            .on_click(cx.listener(|v, _, window, cx| v.action(super::Action::RefStorage(Action::Review), window, cx))),
                    ))
                    .when(!info.can_migrate && !info.reftable, |panel| panel.child(caption("Update Git for Windows to enable conversion.")))
            })
            .when(!self.ref_storage.message.is_empty(), |panel| panel.child(
                div().text_color(rgb(if self.ref_storage.failed { REMOVED } else { MUTED })).child(self.ref_storage.message.clone()),
            ))
    }

    pub(super) fn ref_storage_confirmation(&self, cx: &Context<Self>) -> Option<AnyElement> {
        self.ref_storage.confirming.then(|| dialog_overlay(
            dialog_body("Enable case-sensitive branch names?")
                .relative().children(self.todo_probe(8))
                .child("This converts this repository's references to Git's reftable format. Commits, staging and working files are preserved.")
                .child(caption("Other Git applications must support reftable. Enabling it starts a new Undo/Redo history."))
                .when(self.ref_storage.failed, |body| body.child(div().text_color(rgb(REMOVED)).child(self.ref_storage.message.clone())))
                .child(dialog_actions()
                    .child(Button::new("cancel-ref-conversion").label("Cancel").disabled(self.ref_storage.busy)
                        .on_click(cx.listener(|v, _, window, cx| v.action(super::Action::RefStorage(Action::Cancel), window, cx))))
                    .child(Button::new("confirm-ref-conversion").primary().label(if self.ref_storage.busy { "Converting…" } else { "Enable" })
                        .disabled(self.ref_storage.busy)
                        .on_click(cx.listener(|v, _, window, cx| v.action(super::Action::RefStorage(Action::Confirm), window, cx)))))
        ).into_any_element())
    }
}
