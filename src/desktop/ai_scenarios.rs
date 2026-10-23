//! Real handlers and paint assertions; only runs on marked disposable fixtures.
use super::*;
use std::{cell::RefCell, rc::Rc};
type Paints = Rc<RefCell<[Option<(u8, Bounds<Pixels>)>; 6]>>;
#[derive(Default)]
pub(super) struct State {
    phase: u8,
    paints: Paints,
    started: Option<Instant>,
    verified: bool,
}
impl Desktop {
    fn ai_scenario_name(&self) -> Option<&str> {
        self.scenario.as_deref().filter(|s| {
            matches!(
                *s,
                "ai-settings"
                    | "ai-commit"
                    | "ai-summary"
                    | "ai-rebase"
                    | "ai-rebase-cancel"
                    | "ai-rebase-error"
                    | "ai-merge"
                    | "ai-merge-cancel"
                    | "ai-merge-error"
                    | "ai-conflict"
                    | "ai-conflict-rebase"
                    | "ai-conflict-pull"
                    | "ai-stale"
                    | "ai-cancel"
                    | "ai-error"
            )
        })
    }
    pub(in crate::desktop) fn ai_probe(&self, slot: usize) -> Option<AnyElement> {
        self.ai_scenario_name()?;
        let paints = self.ai.scenario.paints.clone();
        let phase = self.ai.scenario.phase;
        Some(
            canvas(
                |bounds, _, _| bounds,
                move |bounds, _, _, _| paints.borrow_mut()[slot] = Some((phase, bounds)),
            )
            .absolute()
            .inset_0()
            .into_any_element(),
        )
    }
    fn ai_painted(&self, slot: usize, window: &Window) -> bool {
        self.ai.scenario.paints.borrow()[slot].is_some_and(|(phase, bounds)| {
            phase == self.ai.scenario.phase
                && bounds.size.width > px(8.)
                && bounds.size.height > px(8.)
                && bounds.left() >= px(0.)
                && bounds.top() >= px(0.)
                && bounds.right() <= window.viewport_size().width
                && bounds.bottom() <= window.viewport_size().height
        })
    }
    pub(in crate::desktop) fn ai_scenario_ready(&self) -> Option<bool> {
        self.ai_scenario_name().map(|_| self.ai.scenario.verified)
    }
    pub(in crate::desktop) fn ai_scenario_tick(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = self.ai_scenario_name().map(str::to_owned) else {
            return;
        };
        if !self.scenario_started || self.ai.scenario.verified {
            return;
        }
        let started = self.ai.scenario.started.get_or_insert_with(Instant::now);
        assert!(
            started.elapsed() < Duration::from_secs(45),
            "AI scenario timeout: {name}, phase {}, status {}",
            self.ai.scenario.phase,
            self.ai.status
        );
        if self.ai.loading || self.engine.operation_busy || self.engine.app.pending.loading {
            return;
        }
        let phase = self.ai.scenario.phase;
        match phase {
            0 => {
                let root = self.engine.repository_root().unwrap();
                assert_eq!(
                    std::fs::read_to_string(root.join(".git/git-rustle-ai-fixture")).unwrap(),
                    "disposable AI fixture"
                );
                assert_eq!(self.ai.settings.as_ref().unwrap().model, "fixture-model");
                match name.as_str() {
                    "ai-settings" => {
                        self.action(super::super::Action::Settings, window, cx);
                    }
                    "ai-rebase" | "ai-rebase-cancel" | "ai-rebase-error" => {
                        self.ai_action(
                            Action::Rebase("ai-destination".into(), "ai-destination".into()),
                            window,
                            cx,
                        );
                    }
                    "ai-merge" | "ai-merge-cancel" | "ai-merge-error" => {
                        self.ai_action(
                            Action::Merge("ai-destination".into(), "ai-destination".into()),
                            window,
                            cx,
                        );
                    }
                    "ai-conflict" | "ai-conflict-rebase" | "ai-conflict-pull" => {
                        self.operation_action(operations::Action::ShowConflicts, window, cx);
                    }
                    _ => {
                        let row = self.engine.app.pending.row();
                        self.action(super::super::Action::Select(row, false), window, cx);
                    }
                }
                self.ai.scenario.phase = 1;
            }
            1 => {
                match name.as_str() {
                    "ai-settings" => {
                        if self.engine.app.settings.busy {
                            return;
                        }
                        self.action(super::super::Action::SettingsTab(SETTINGS_PAGE), window, cx);
                        self.ai_action(Action::Toggle(0), window, cx);
                        self.ai_action(Action::Toggle(2), window, cx);
                        self.ai.prompts[2].update(cx, |s, cx| s.set_value("Summarize with a focus on behavior.\nUse concise Markdown bullets.", window, cx));
                        self.ai_action(Action::Save, window, cx);
                    }
                    "ai-rebase" | "ai-merge" => {
                        if self.ai.busy || self.engine.refresh_in_progress() {
                            return;
                        }
                        assert!(!self.ai.error, "{}", self.ai.result);
                        assert!(self.ai.status.contains(if name == "ai-merge" {
                            "AI merge completed"
                        } else {
                            "AI rebase completed"
                        }));
                        assert!(!self.operations.rebase_open);
                    }
                    "ai-rebase-cancel" | "ai-merge-cancel" => {
                        if !std::path::Path::new(
                            &std::env::var("GIT_RUSTLE_AI_FIXTURE_LOG").unwrap(),
                        )
                        .is_file()
                        {
                            return;
                        }
                        assert!(self.ai.busy);
                        self.ai_action(Action::Cancel, window, cx);
                    }
                    "ai-rebase-error" | "ai-merge-error" => {
                        if self.ai.busy {
                            return;
                        }
                        assert!(self.ai.error && self.ai.show_result);
                    }
                    "ai-conflict" | "ai-conflict-rebase" | "ai-conflict-pull" => {
                        if self.conflict_editor_value(cx).is_none() {
                            return;
                        }
                        self.ai_action(Action::Run(Task::Conflict), window, cx);
                    }
                    _ => {
                        if self.engine.app.active_changes().is_none_or(|c| !c.loaded) {
                            return;
                        }
                        self.action(super::super::Action::CheckAll(false), window, cx);
                        let node = {
                            let cs = self.engine.app.active_changes().unwrap();
                            cs.tree
                                .nodes
                                .iter()
                                .position(|n| {
                                    n.file.is_some_and(|i| cs.files[i].path == "README.md")
                                })
                                .unwrap()
                        };
                        self.action(super::super::Action::Check(node), window, cx);
                        if name == "ai-summary" {
                            self.action(super::super::Action::Diff, window, cx);
                        } else {
                            self.ai_action(Action::Run(Task::Commit), window, cx);
                        }
                    }
                }
                self.ai.scenario.phase = 2;
            }
            2 => {
                if name == "ai-summary" {
                    if !matches!(&self.engine.app.mode, Mode::Diff(v) if v.diff_for == v.current && v.diff.is_some())
                    {
                        return;
                    }
                    self.ai_action(Action::Run(Task::Summary), window, cx);
                    self.ai.scenario.phase = 3;
                } else if name == "ai-stale" {
                    assert!(self.ai.busy);
                    self.description
                        .update(cx, |s, cx| s.set_value("Keep my newer message", window, cx));
                    self.ai.scenario.phase = 3;
                } else if name == "ai-cancel" {
                    assert!(self.ai.busy);
                    self.ai_action(Action::Cancel, window, cx);
                    self.ai.scenario.phase = 3;
                } else {
                    self.ai.scenario.phase = 3;
                }
            }
            3 => {
                if self.ai.busy || self.ai.saving {
                    return;
                }
                let slot = match name.as_str() {
                    "ai-settings" => {
                        assert!(!self.ai.error, "{}", self.ai.status);
                        assert!(self.ai.settings.as_ref().unwrap().prompts[2].contains("behavior"));
                        0
                    }
                    "ai-summary" => {
                        assert!(self.ai.show_result);
                        assert!(self.ai.result.contains("Summary fixture"));
                        5
                    }
                    "ai-error" => {
                        assert!(self.ai.error && self.ai.show_result);
                        assert!(self.ai.result.contains("Sign in first"));
                        5
                    }
                    "ai-stale" => {
                        assert_eq!(
                            self.description.read(cx).value().as_ref(),
                            "Keep my newer message"
                        );
                        assert!(self.ai.show_result);
                        assert!(self.ai.result.contains("Describe selected documentation"));
                        5
                    }
                    "ai-cancel" => {
                        assert!(self.ai.status.contains("cancelled"));
                        assert!(self.description.read(cx).value().is_empty());
                        4
                    }
                    "ai-conflict" | "ai-conflict-rebase" | "ai-conflict-pull" => {
                        assert!(!self.ai.error, "{}", self.ai.result);
                        assert_eq!(
                            self.conflict_editor_value(cx).as_deref(),
                            Some("resolved AI fixture\n")
                        );
                        assert!(!self.conflict_continue_enabled());
                        1
                    }
                    "ai-rebase" | "ai-merge" => {
                        assert!(!self.ai.error);
                        assert!(self.engine.operations.conflict.is_none());
                        2
                    }
                    "ai-rebase-cancel" | "ai-merge-cancel" => {
                        assert!(self.ai.cancel.load(Ordering::Acquire));
                        assert!(
                            self.engine
                                .operations
                                .conflict
                                .as_ref()
                                .is_some_and(|c| c.kind
                                    == if name == "ai-merge-cancel" {
                                        "Merge"
                                    } else {
                                        "Rebase"
                                    })
                        );
                        2
                    }
                    "ai-rebase-error" | "ai-merge-error" => {
                        assert!(
                            self.engine
                                .operations
                                .conflict
                                .as_ref()
                                .is_some_and(|c| c.kind
                                    == if name == "ai-merge-error" {
                                        "Merge"
                                    } else {
                                        "Rebase"
                                    })
                        );
                        assert!(self.ai.error && self.ai.show_result);
                        5
                    }
                    _ => {
                        assert!(!self.ai.error, "{}", self.ai.result);
                        assert_eq!(
                            self.description.read(cx).value().as_ref(),
                            "Describe selected documentation\n\nExplain the pending README update."
                        );
                        4
                    }
                };
                if !self.ai_painted(slot, window) {
                    return;
                }
                if !self.ai_painted(2, window) {
                    return;
                }
                let (_, bounds) = self.ai.scenario.paints.borrow()[2].unwrap();
                // Canvas bounds exclude the footer's one physical-pixel top border.
                assert!((super::super::status::HEIGHT - f32::from(bounds.size.height)).abs() <= 1.);
                assert!((f32::from(bounds.bottom() - window.viewport_size().height)).abs() <= 1.);
                self.ai.scenario.verified = true;
                self.engine.mark("AI scenario verified after native paint");
            }
            _ => unreachable!(),
        }
        cx.notify();
    }
}
