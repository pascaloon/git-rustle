//! The update affordance shares the footer, modal shell and orange primary buttons.
use super::*;
use crate::update::{self, Available};
use std::path::PathBuf;

#[derive(Default, Debug, PartialEq, Eq)]
pub(super) enum CheckState {
    #[default]
    Idle,
    Checking,
    Current,
    Available(String),
    Failed(String),
}

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
    pub(super) check_state: CheckState,
    pub(super) check_fixture: Option<(String, PathBuf)>,
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
            check_state: CheckState::Idle,
            check_fixture: None,
        }
    }
}
impl Updates {
    fn can_check(&self) -> bool {
        !self.checking && !self.installing && !self.installed && !self.open
    }

    fn can_accept(&self) -> bool {
        !self.checking && !self.installing && !self.fixture
    }

    fn begin_check(&mut self, manual: bool) -> bool {
        if !self.can_check() || (!manual && Instant::now() < self.next_check) {
            return false;
        }
        self.checking = true;
        self.check_state = CheckState::Checking;
        true
    }

    fn finish_check(&mut self, result: Result<update::Checked, String>) {
        self.checking = false;
        // Keep the staged binary's version and Restart action stable even if an
        // older worker result is delivered after installation started.
        if self.installing || self.installed {
            self.check_state = self.available.as_ref().map_or(CheckState::Idle, |a| {
                CheckState::Available(a.version.clone())
            });
            return;
        }
        match result {
            Ok(checked) => {
                self.next_check = Instant::now() + checked.next_check_in;
                self.check_state = checked.available.as_ref().map_or(CheckState::Current, |a| {
                    CheckState::Available(a.version.clone())
                });
                self.available = checked.available;
            }
            Err(error) => {
                self.next_check = Instant::now() + Duration::from_secs(60 * 60);
                self.check_state = CheckState::Failed(error);
            }
        }
    }

    pub(super) fn set_check_fixture(&mut self, api: String, path: PathBuf) {
        self.fixture = true;
        self.check_fixture = Some((api, path));
        self.next_check = Instant::now() + update::CHECK_INTERVAL;
    }

    pub(super) fn check_deferred(&self) -> bool {
        Instant::now() < self.next_check
    }
}

fn fixture_check(api: &str, path: &std::path::Path) -> Result<update::Checked, String> {
    let url = url::Url::parse(api).map_err(|e| e.to_string())?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Update diagnostic fixture must use a loopback HTTP address".into());
    }
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    update::check_and_record_at(api, update::VERSION, path, time)
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
        if !self.engine.has_presented() {
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
        if self.scenario.is_some() || !self.updates.begin_check(false) {
            return;
        }
        self.spawn_update_check(false, cx);
    }

    pub(super) fn update_check_enabled(&self) -> bool {
        self.updates.can_check()
    }

    pub(super) fn request_update_check(&mut self, cx: &mut Context<Self>) {
        if !self.engine.app.settings.open
            || self.engine.app.settings.page != crate::settings::GENERAL_PAGE
            || !self.updates.begin_check(true)
        {
            return;
        }
        self.spawn_update_check(true, cx);
    }

    fn spawn_update_check(&mut self, manual: bool, cx: &mut Context<Self>) {
        self.engine.mark(if manual {
            "manual update check requested"
        } else {
            "update check requested"
        });
        let fixture = self.updates.check_fixture.clone();
        let diagnostic = self.scenario.is_some();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    match fixture {
                        Some((api, path)) if diagnostic => fixture_check(&api, &path),
                        Some(_) => {
                            Err("Update fixtures are restricted to diagnostic scenarios".into())
                        }
                        None => update::check(),
                    }
                })
                .await;
            let _ = view.update(cx, |v, cx| {
                v.updates.finish_check(result);
                v.engine.mark(if manual {
                    "manual update check completed"
                } else {
                    "update check completed"
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn update_controls(&self, cx: &Context<Self>) -> Div {
        let (message, error) = if self.updates.installed {
            (
                "The update is installed. Use “Restart to update” below to finish.".into(),
                false,
            )
        } else {
            match &self.updates.check_state {
                CheckState::Idle => (
                    "Check for the latest stable Windows release.".to_string(),
                    false,
                ),
                CheckState::Checking => ("Checking for updates…".into(), false),
                CheckState::Current => ("You're up to date.".into(), false),
                CheckState::Available(version) => (
                    format!(
                        "Version {version} is available. Use the update button below to review it."
                    ),
                    false,
                ),
                CheckState::Failed(error) => {
                    (format!("Could not check for updates: {error}"), true)
                }
            }
        };
        column()
            .gap_2()
            .child(div().text_color(rgb(TEXT)).child("Updates"))
            .child(
                row()
                    .child(caption(format!("Git Rustle {}", update::VERSION)))
                    .child(div().flex_1())
                    .child(self.button(
                        "check-updates",
                        if self.updates.checking {
                            "Checking…"
                        } else {
                            "Check for updates"
                        },
                        Action::CheckUpdates,
                        cx,
                    )),
            )
            .child(
                div()
                    .text_color(rgb(if error { REMOVED } else { MUTED }))
                    .child(message),
            )
    }

    pub(super) fn update_check_summary(&self) -> String {
        let state = if self.updates.installed {
            "installed"
        } else {
            match self.updates.check_state {
                CheckState::Idle => "idle",
                CheckState::Checking => "checking",
                CheckState::Current => "current",
                CheckState::Available(_) => "available",
                CheckState::Failed(_) => "error",
            }
        };
        format!(
            "update_check_state={state} update_check_enabled={} update_installing={}",
            self.update_check_enabled(),
            self.updates.installing
        )
    }

    pub(super) fn update_button(&self, cx: &Context<Self>) -> Option<AnyElement> {
        self.updates.available.as_ref()?;
        Some(
            Button::new("update-available")
                .xsmall()
                .primary()
                .disabled(self.updates.checking)
                .label(if self.updates.installed {
                    "Restart to update"
                } else {
                    "Update available"
                })
                .tooltip("Install the latest Git Rustle release")
                .on_click(cx.listener(|v, _, _, cx| {
                    if v.updates.checking {
                        return;
                    }
                    v.status_panel = None;
                    v.updates.open = true;
                    cx.notify();
                }))
                .into_any_element(),
        )
    }

    fn accept_update(&mut self, cx: &mut Context<Self>) {
        if !self.updates.can_accept() || self.restart_busy() {
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
                    .disabled(busy || !self.updates.can_accept())
                    .on_click(cx.listener(|v, _, _, cx| v.accept_update(cx))))))
            .into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::{Available, CheckState, Instant, Updates, fixture_check, update};

    #[test]
    fn manual_check_bypasses_schedule_and_refuses_duplicate_or_installing_checks() {
        let mut updates = Updates {
            next_check: Instant::now() + update::CHECK_INTERVAL,
            ..Default::default()
        };
        assert!(!updates.begin_check(false));
        assert!(updates.begin_check(true));
        assert!(!updates.begin_check(true));
        updates.finish_check(Ok(update::Checked {
            available: None,
            next_check_in: update::CHECK_INTERVAL,
        }));
        assert_eq!(updates.check_state, CheckState::Current);
        updates.installing = true;
        assert!(!updates.begin_check(true));
    }

    #[test]
    fn check_outcomes_are_visible_without_installing_or_opening_confirmation() {
        let mut updates = Updates::default();
        let available = Available {
            version: "99.0.0".into(),
            checksum: "0".repeat(64),
        };
        updates.finish_check(Ok(update::Checked {
            available: Some(available),
            next_check_in: update::CHECK_INTERVAL,
        }));
        assert_eq!(updates.check_state, CheckState::Available("99.0.0".into()));
        assert!(!updates.open && !updates.installing && !updates.installed);
        updates.finish_check(Err("HTTP 502".into()));
        assert_eq!(updates.check_state, CheckState::Failed("HTTP 502".into()));
        assert!(updates.available.is_some());
        assert!(!updates.open && !updates.installing && !updates.installed);
    }

    #[test]
    fn checks_and_confirmation_installation_are_mutually_exclusive() {
        let mut updates = Updates::default();
        assert!(updates.begin_check(true));
        assert!(
            !updates.can_accept(),
            "install must wait for a pending check"
        );
        updates.finish_check(Ok(update::Checked {
            available: Some(Available {
                version: "99.0.0".into(),
                checksum: "0".repeat(64),
            }),
            next_check_in: update::CHECK_INTERVAL,
        }));
        updates.open = true;
        assert!(updates.can_accept());
        assert!(!updates.begin_check(false));
        assert!(
            !updates.begin_check(true),
            "confirmation must keep its candidate stable"
        );
        updates.installing = true;
        assert!(!updates.can_accept());
        updates.installing = false;
        updates.installed = true;
        updates.open = false;
        assert!(!updates.can_check());
        assert!(
            !updates.begin_check(true),
            "restart-ready state must not check again"
        );
        assert!(
            updates.can_accept(),
            "restart remains available after installation"
        );
    }

    #[test]
    fn late_checks_preserve_installing_and_restart_ready_candidates() {
        for installed in [false, true] {
            let mut updates = Updates {
                available: Some(Available {
                    version: "99.0.0".into(),
                    checksum: "0".repeat(64),
                }),
                open: true,
                installed,
                installing: !installed,
                ..Default::default()
            };
            for result in [
                Ok(update::Checked {
                    available: None,
                    next_check_in: update::CHECK_INTERVAL,
                }),
                Ok(update::Checked {
                    available: Some(Available {
                        version: "100.0.0".into(),
                        checksum: "1".repeat(64),
                    }),
                    next_check_in: update::CHECK_INTERVAL,
                }),
                Err("HTTP 502".into()),
            ] {
                updates.checking = true;
                updates.finish_check(result);
                assert!(!updates.checking);
                assert_eq!(updates.available.as_ref().unwrap().version, "99.0.0");
                assert_eq!(updates.available.as_ref().unwrap().checksum, "0".repeat(64));
                assert_eq!(updates.check_state, CheckState::Available("99.0.0".into()));
                assert!(updates.open, "a staged update must retain its confirmation");
            }
        }
    }

    #[test]
    fn diagnostic_checks_refuse_non_loopback_destinations() {
        for api in [
            "https://api.github.com",
            "http://example.invalid:8080",
            "http://127.0.0.1:80/path",
            "http://name:token@127.0.0.1:80",
        ] {
            assert!(fixture_check(api, std::path::Path::new("unused.json")).is_err());
        }
    }
}
