//! The update affordance shares the footer, modal shell and orange primary buttons.
use super::*;
use crate::update::{self, Available};
use std::path::PathBuf;

pub(super) struct Updates {
    pub available: Option<Available>,
    pub open: bool,
    pub installing: bool,
    installed: bool,
    checking: bool,
    next_check: Instant,
    error: String,
    exe: Option<PathBuf>,
    fixture: bool,
}
impl Default for Updates {
    fn default() -> Self {
        Self {
            available: None,
            open: false,
            installing: false,
            installed: false,
            checking: false,
            next_check: Instant::now(),
            error: String::new(),
            exe: std::env::current_exe().ok(),
            fixture: false,
        }
    }
}
impl Desktop {
    pub(super) fn restart_busy(&self) -> bool {
        self.composer.as_ref().is_some_and(|c| c.busy())
            || self.engine.operation_busy
            || self.engine.recents_loading
            || self.engine.app.pending.busy
            || self.engine.app.branches.busy
            || self.engine.app.settings.saving
            || self.picking_directory
    }

    pub(super) fn poll_updates(&mut self, cx: &mut Context<Self>) {
        if !self.engine.has_presented() || self.updates.checking || self.updates.installed {
            return;
        }
        // Diagnostic scenarios are deterministic and never download/install updates.
        if self.scenario.as_deref() == Some("update-available") && !self.updates.fixture {
            self.updates.fixture = true;
            self.updates.available = Some(Available {
                version: "99.0.0".into(),
                checksum: "0".repeat(64),
            });
            cx.notify();
        }
        if self.scenario.is_some() || Instant::now() < self.updates.next_check {
            return;
        }
        self.updates.checking = true;
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async { update::check() })
                .await;
            let _ = view.update(cx, |v, cx| {
                v.updates.checking = false;
                // Offline, unpublished releases and rate limits are quiet. Retry later.
                match result {
                    Ok(checked) => {
                        v.updates.next_check = Instant::now() + checked.next_check_in;
                        v.updates.available = checked.available;
                        cx.notify();
                    }
                    Err(_) => v.updates.next_check = Instant::now() + Duration::from_secs(60 * 60),
                }
            });
        })
        .detach();
    }

    pub(super) fn update_button(&self, cx: &Context<Self>) -> Option<AnyElement> {
        self.updates.available.as_ref()?;
        Some(
            Button::new("update-available")
                .xsmall()
                .primary()
                .label(if self.updates.installed {
                    "Restart to update"
                } else {
                    "Update available"
                })
                .tooltip("Install the latest Git Rustle release")
                .on_click(cx.listener(|v, _, _, cx| {
                    v.status_panel = None;
                    v.updates.open = true;
                    cx.notify();
                }))
                .into_any_element(),
        )
    }

    fn accept_update(&mut self, cx: &mut Context<Self>) {
        if self.updates.installing || self.restart_busy() || self.updates.fixture {
            return;
        }
        if self.updates.installed {
            self.restart_updated(cx);
            return;
        }
        let Some(candidate) = self.updates.available.clone() else {
            return;
        };
        let Some(exe) = self.updates.exe.clone() else {
            self.updates.error = "Cannot locate the running executable.".into();
            cx.notify();
            return;
        };
        self.updates.installing = true;
        self.updates.error.clear();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { update::install(&candidate, &exe) })
                .await;
            let _ = view.update(cx, |v, cx| {
                v.updates.installing = false;
                match result {
                    Ok(()) => {
                        v.updates.installed = true;
                        // A queued operation may have started just before the modal appeared.
                        if !v.restart_busy() {
                            v.restart_updated(cx);
                        }
                    }
                    Err(error) => v.updates.error = format!("Update failed: {error}"),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn restart_updated(&mut self, cx: &mut Context<Self>) {
        if self.restart_busy() {
            return;
        }
        let Some(exe) = &self.updates.exe else {
            return;
        };
        let draft = update::CommitDraft::capture(&self.engine.app);
        let mut command = update::command(exe);
        let path = self.engine.repository_path();
        if !path.is_empty() {
            command.arg(path);
        } else if let Ok(path) = std::env::current_dir() {
            command.arg(path);
        }
        if self.engine.no_cache() {
            command.arg("--no-cache");
        }
        command.env(update::RESUME, serde_json::to_string(&draft).unwrap());
        // Keep benchmark/diagnostic controls out of the user's restarted session.
        for key in [
            "GIT_RUSTLE_AUTOQUIT",
            "GIT_RUSTLE_SCENARIO",
            "GIT_RUSTLE_DUMP",
            "GIT_RUSTLE_FRAME_SAMPLES",
        ] {
            command.env_remove(key);
        }
        match command.spawn() {
            Ok(_) => cx.quit(),
            Err(error) => {
                self.updates.error = format!(
                    "The update is installed, but restart failed: {error}. You can retry or reopen Git Rustle manually."
                )
            }
        }
    }

    pub(super) fn update_dialog(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if !self.updates.open {
            return None;
        }
        let available = self.updates.available.as_ref()?;
        let busy = self.restart_busy();
        let installing = self.updates.installing;
        Some(dialog_overlay(dialog_body("Update Git Rustle")
            .child(caption(format!("Version {} → {}", update::VERSION, available.version)))
            .child(if installing { "Downloading and verifying the update…" }
                else if self.updates.installed { "The update is installed and ready to restart." }
                else { "Download the latest release and restart Git Rustle." })
            .child(caption("Your repository and commit draft will reopen. Other unsaved forms close on restart."))
            .when(busy, |s| s.child(caption("Waiting for the active operation to finish…")))
            .when(self.updates.fixture, |s| s.child(caption("Preview only — installation is disabled.")))
            .when(!self.updates.error.is_empty(), |s| s.child(div().text_color(rgb(REMOVED)).child(self.updates.error.clone())))
            .child(dialog_actions()
                .child(Button::new("update-later").label("Later").disabled(installing)
                    .on_click(cx.listener(|v, _, _, cx| { v.updates.open = false; cx.notify(); })))
                .child(Button::new("update-accept").primary()
                    .label(if installing { "Updating…" } else if self.updates.installed { "Restart" } else { "Update and restart" })
                    .disabled(busy || installing || self.updates.fixture)
                    .on_click(cx.listener(|v, _, _, cx| v.accept_update(cx))))))
            .into_any_element())
    }
}
