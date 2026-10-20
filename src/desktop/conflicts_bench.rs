//! Opt-in proof of real conflict UI actions on marked disposable repositories.
use super::*;
use crate::git::operations::{Request, Resolution};
use std::{cell::RefCell, rc::Rc};

const MARKER: &str = "disposable conflict benchmark fixture";

#[derive(Clone)]
struct Paint {
    phase: u8,
    bounds: Bounds<Pixels>,
    fingerprint: Option<u64>,
    inspected: Option<gix::ObjectId>,
    selected_path: Option<String>,
    continue_enabled: bool,
    stale: bool,
    operation_error: bool,
}

struct Run {
    kind: &'static str,
    phase: u8,
    scheduled: bool,
    started: Instant,
    verified: bool,
    checks: usize,
    epoch: u64,
    paints: Rc<RefCell<Vec<Option<Paint>>>>,
}

#[derive(Default)]
pub(super) struct State {
    run: Option<Run>,
}

impl Desktop {
    pub(super) fn conflicts_start(&mut self, cx: &mut Context<Self>) {
        let kind = match self.scenario.as_deref() {
            Some("conflicts-merge") => "merge",
            Some("conflicts-rebase") => "rebase",
            Some("conflicts-stash") => "stash",
            Some("conflicts-abort") => "abort",
            _ => return,
        };
        let root = self.engine.repository_root().expect("conflict fixture");
        assert_eq!(
            std::fs::read_to_string(root.join(".git/git-rustle-conflicts-fixture")).unwrap(),
            MARKER
        );
        assert!(self.conflicts_bench.run.is_none());
        self.conflicts_bench.run = Some(Run {
            kind,
            phase: 100,
            scheduled: false,
            started: Instant::now(),
            verified: false,
            checks: 0,
            epoch: self.engine.operation_epoch,
            paints: Rc::new(RefCell::new(vec![None; 10])),
        });
        cx.notify();
    }

    pub(super) fn conflicts_schedule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(run) = self
            .conflicts_bench
            .run
            .as_mut()
            .filter(|r| !r.verified && !r.scheduled)
        else {
            return;
        };
        run.scheduled = true;
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = weak.update(cx, |view, cx| {
                view.conflicts_bench.run.as_mut().unwrap().scheduled = false;
                view.conflicts_tick(window, cx);
                cx.notify();
            });
        });
    }

    pub(super) fn conflicts_probe(&self, slot: usize) -> Option<AnyElement> {
        let run = self.conflicts_bench.run.as_ref()?;
        let paints = run.paints.clone();
        let phase = run.phase;
        let fingerprint = self
            .engine
            .operations
            .conflict
            .as_ref()
            .map(|c| c.fingerprint);
        let inspected = self.engine.app.inspected_id();
        let selected_path = self.conflict_selected_path().map(str::to_owned);
        let continue_enabled = self.conflict_continue_enabled();
        let stale = self.conflict_editor_stale();
        let operation_error = self.engine.operation_error;
        Some(
            canvas(
                |bounds, _, _| bounds,
                move |bounds, _, _, _| {
                    paints.borrow_mut()[slot] = Some(Paint {
                        phase,
                        bounds,
                        fingerprint,
                        inspected,
                        selected_path: selected_path.clone(),
                        continue_enabled,
                        stale,
                        operation_error,
                    });
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .into_any_element(),
        )
    }

    fn conflicts_painted(&self, slot: usize, window: &Window) -> bool {
        let run = self.conflicts_bench.run.as_ref().unwrap();
        run.paints.borrow()[slot].as_ref().is_some_and(|p| {
            p.phase == run.phase
                && p.fingerprint
                    == self
                        .engine
                        .operations
                        .conflict
                        .as_ref()
                        .map(|c| c.fingerprint)
                && p.inspected == self.engine.app.inspected_id()
                && p.selected_path.as_deref() == self.conflict_selected_path()
                && p.continue_enabled == self.conflict_continue_enabled()
                && p.stale == self.conflict_editor_stale()
                && p.operation_error == self.engine.operation_error
                && p.bounds.size.width > px(0.)
                && p.bounds.size.height > px(0.)
                && p.bounds.top() >= px(0.)
                && p.bounds.bottom() <= window.viewport_size().height
                && p.bounds.left() >= px(0.)
                && p.bounds.right() <= window.viewport_size().width
        })
    }

    fn conflicts_phase(&mut self, phase: u8) {
        self.conflicts_bench.run.as_mut().unwrap().phase = phase;
    }

    fn conflicts_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let run = self.conflicts_bench.run.as_ref().unwrap();
        assert!(
            run.started.elapsed() < Duration::from_secs(50),
            "conflict scenario stalled in phase {}",
            run.phase
        );
        let phase = run.phase;
        let kind = run.kind;
        let idle = !self.engine.operation_busy;
        match phase {
            100 if self.engine.external_tools_idle() && self.engine.app.history.complete => {
                assert!(self.engine.operations.conflict.is_none());
                assert!(!self.engine.app.pending.exists());
                self.engine.mark("conflicts setup requested");
                let request = match kind {
                    "rebase" => Request::Rebase("topic".into()),
                    "stash" => Request::ListStashes,
                    _ => Request::Merge("topic".into()),
                };
                self.operation_action(operations::Action::Run(request), window, cx);
                self.conflicts_phase(if kind == "stash" { 101 } else { 0 });
            }
            101 if idle
                && self
                    .engine
                    .operations
                    .stashes
                    .as_ref()
                    .is_some_and(|s| s.len() == 1) =>
            {
                let stash = self.engine.operations.stashes.as_ref().unwrap()[0].clone();
                self.operation_action(
                    operations::Action::Run(Request::ApplyStash(stash)),
                    window,
                    cx,
                );
                self.conflicts_phase(102);
            }
            102 if self.conflicts_painted(9, window) => {
                self.operation_action(operations::Action::Confirm, window, cx);
                self.conflicts_phase(0);
            }
            0 if idle
                && self.engine.operations.conflict.as_ref().is_some_and(|c| {
                    c.files.len() == 3 && !c.incoming.is_empty() && !c.local.is_empty()
                }) =>
            {
                assert!(!self.conflict_continue_enabled());
                self.engine.mark("desktop scenario requested");
                self.engine.mark("conflicts open requested");
                self.operation_action(operations::Action::ShowConflicts, window, cx);
                self.conflicts_phase(1);
            }
            1 if (0..=2).all(|slot| self.conflicts_painted(slot, window))
                && self.conflicts_painted(4, window) =>
            {
                assert!(self.conflict_abort_enabled());
                assert!(!self.conflict_continue_enabled());
                self.engine.mark("conflicts open frame");
                if kind == "abort" {
                    self.engine.mark("conflicts abort requested");
                    self.operation_action(operations::Action::Run(Request::Abort), window, cx);
                    self.conflicts_phase(20);
                    return;
                }
                let entry = &self.engine.operations.conflict.as_ref().unwrap().incoming[0];
                let (id, parent) = (entry.id, entry.parents.first().copied());
                self.engine.mark("conflicts incoming requested");
                self.select_conflict_commit(id, parent, window, cx);
                self.conflicts_phase(2);
            }
            2 if self.conflicts_painted(7, window) => {
                let entry = &self.engine.operations.conflict.as_ref().unwrap().incoming[0];
                if self.engine.app.inspected_id() != Some(entry.id)
                    || self
                        .engine
                        .app
                        .details
                        .as_ref()
                        .is_none_or(|d| d.id != entry.id)
                {
                    return;
                }
                self.engine.mark("conflicts incoming frame");
                let entry = &self.engine.operations.conflict.as_ref().unwrap().local[0];
                let (id, parent) = (entry.id, entry.parents.first().copied());
                self.engine.mark("conflicts local requested");
                self.select_conflict_commit(id, parent, window, cx);
                self.conflicts_phase(3);
            }
            3 if self.conflicts_painted(7, window) => {
                let entry = &self.engine.operations.conflict.as_ref().unwrap().local[0];
                if self.engine.app.inspected_id() != Some(entry.id)
                    || self
                        .engine
                        .app
                        .details
                        .as_ref()
                        .is_none_or(|d| d.id != entry.id)
                {
                    return;
                }
                self.engine.mark("conflicts local frame");
                self.action(Action::Tab(Tab::Graph), window, cx);
                self.conflicts_phase(31);
            }
            31 => {
                assert!(self.engine.app.inspected_id().is_none());
                self.operation_action(operations::Action::ShowConflicts, window, cx);
                self.conflicts_phase(32);
            }
            32 if self.conflicts_painted(7, window) => {
                let entry = &self.engine.operations.conflict.as_ref().unwrap().incoming[0];
                if self.engine.app.inspected_id() != Some(entry.id)
                    || self
                        .engine
                        .app
                        .details
                        .as_ref()
                        .is_none_or(|d| d.id != entry.id)
                {
                    return;
                }
                self.engine.mark("conflicts whole requested");
                let choice = if kind == "rebase" {
                    Resolution::Ours
                } else {
                    Resolution::Theirs
                };
                self.operation_action(
                    operations::Action::Run(Request::Resolve {
                        path: "a.txt".into(),
                        choice,
                    }),
                    window,
                    cx,
                );
                self.conflicts_phase(30);
            }
            30 if self.conflicts_painted(9, window) => {
                self.operation_action(operations::Action::Confirm, window, cx);
                self.conflicts_phase(4);
            }
            4 if idle
                && self
                    .engine
                    .operations
                    .conflict
                    .as_ref()
                    .is_some_and(|c| !c.files.iter().any(|p| p == "a.txt"))
                && self.conflicts_painted(2, window) =>
            {
                self.engine.mark("conflicts whole frame");
                self.engine.mark("conflicts file requested");
                self.operation_action(
                    operations::Action::SelectConflictFile("b.txt".into()),
                    window,
                    cx,
                );
                self.conflicts_phase(5);
            }
            5 if idle
                && self.conflict_selected_path() == Some("b.txt")
                && self.conflict_editor_value(cx).is_some()
                && self.conflicts_painted(3, window)
                && self.conflicts_painted(8, window) =>
            {
                self.engine.mark("conflicts file frame");
                assert!(!self.conflict_editor_stale());
                self.conflict_editor_edit("unsaved benchmark draft\n", window, cx);
                self.engine.mark("conflicts stale requested");
                let root = self.engine.repository_root().unwrap().to_path_buf();
                self.conflicts_phase(51);
                cx.spawn(async move |_, cx| {
                    cx.background_executor()
                        .spawn(async move {
                            std::fs::write(root.join("b.txt"), "external draft update\n").unwrap();
                        })
                        .await;
                })
                .detach();
            }
            51 if idle && self.conflict_editor_stale() && self.conflicts_painted(3, window) => {
                assert_eq!(
                    self.conflict_editor_value(cx).as_deref(),
                    Some("unsaved benchmark draft\n")
                );
                self.engine.mark("conflicts stale frame");
                self.engine.mark("conflicts reject requested");
                self.conflicts_bench.run.as_mut().unwrap().epoch = self.engine.operation_epoch;
                self.operation_action(operations::Action::SaveConflict, window, cx);
                self.conflicts_phase(52);
            }
            52 if idle
                && self.engine.operation_epoch
                    != self.conflicts_bench.run.as_ref().unwrap().epoch
                && self.conflicts_painted(3, window) =>
            {
                assert!(self.engine.operation_error, "stale save must be rejected");
                assert_eq!(
                    self.conflict_editor_value(cx).as_deref(),
                    Some("unsaved benchmark draft\n")
                );
                self.engine.mark("conflicts reject frame");
                let root = self.engine.repository_root().unwrap().to_path_buf();
                self.conflicts_phase(53);
                cx.spawn_in(window, async move |view, cx| {
                    cx.background_executor()
                        .spawn(async move {
                            assert_eq!(
                                std::fs::read_to_string(root.join("b.txt")).unwrap(),
                                "external draft update\n"
                            );
                        })
                        .await;
                    let _ = view.update_in(cx, |v, window, cx| {
                        v.engine.mark("conflicts reload requested");
                        v.operation_action(operations::Action::ReloadConflict, window, cx);
                        v.conflicts_phase(54);
                        cx.notify();
                    });
                })
                .detach();
            }
            54 if idle
                && !self.conflict_editor_stale()
                && self.conflict_editor_value(cx).as_deref() == Some("external draft update\n")
                && self.conflicts_painted(3, window) =>
            {
                self.engine.mark("conflicts reload frame");
                self.engine.mark("conflicts save requested");
                self.conflict_editor_edit("resolved editor\n", window, cx);
                self.operation_action(operations::Action::SaveConflict, window, cx);
                self.conflicts_phase(6);
            }
            6 if idle
                && self
                    .engine
                    .operations
                    .conflict
                    .as_ref()
                    .is_some_and(|c| c.files == ["c.txt"])
                && self.conflicts_painted(2, window) =>
            {
                self.engine.mark("conflicts save frame");
                self.operation_action(
                    operations::Action::SelectConflictFile("c.txt".into()),
                    window,
                    cx,
                );
                self.conflicts_phase(61);
            }
            61 if idle
                && self.conflict_selected_path() == Some("c.txt")
                && self.conflicts_painted(3, window) =>
            {
                self.operation_action(
                    operations::Action::SelectConflictFile("b.txt".into()),
                    window,
                    cx,
                );
                self.conflicts_phase(62);
            }
            62 if idle
                && self.conflict_selected_path() == Some("b.txt")
                && self.conflicts_painted(3, window) =>
            {
                assert_eq!(
                    self.conflict_editor_value(cx).as_deref(),
                    Some("resolved editor\n")
                );
                assert!(!self.conflict_continue_enabled());
                self.engine.mark("conflicts external requested");
                let root = self.engine.repository_root().unwrap().to_path_buf();
                self.conflicts_phase(7);
                cx.spawn(async move |_, cx| {
                    cx.background_executor()
                        .spawn(async move {
                            assert_eq!(
                                std::fs::read_to_string(
                                    root.join(".git/git-rustle-conflicts-fixture")
                                )
                                .unwrap(),
                                MARKER
                            );
                            std::fs::write(root.join("c.txt"), "resolved external\n").unwrap();
                            crate::git::command::checked(&root, &["add", "--", "c.txt"]).unwrap();
                        })
                        .await;
                })
                .detach();
            }
            7 if idle && self.conflict_continue_enabled() && self.conflicts_painted(5, window) => {
                assert!(
                    self.engine
                        .operations
                        .conflict
                        .as_ref()
                        .unwrap()
                        .files
                        .is_empty()
                );
                self.engine.mark("conflicts external frame");
                self.engine.mark("conflicts continue requested");
                self.conflicts_bench.run.as_mut().unwrap().epoch = self.engine.operation_epoch;
                self.operation_action(operations::Action::Run(Request::Continue), window, cx);
                self.conflicts_phase(8);
            }
            8 if idle
                && self.engine.operations.conflict.is_none()
                && self.engine.operation_epoch
                    != self.conflicts_bench.run.as_ref().unwrap().epoch =>
            {
                assert!(
                    !self.engine.operation_error,
                    "{}",
                    self.engine.operation_status
                );
                self.engine.mark("conflicts continue frame");
                self.conflicts_finish();
            }
            20 if self.conflicts_painted(9, window) => {
                self.engine.mark("conflicts abort modal");
                self.conflicts_bench.run.as_mut().unwrap().epoch = self.engine.operation_epoch;
                self.operation_action(operations::Action::Confirm, window, cx);
                self.conflicts_phase(21);
            }
            21 if idle
                && self.engine.operations.conflict.is_none()
                && self.engine.operation_epoch
                    != self.conflicts_bench.run.as_ref().unwrap().epoch =>
            {
                assert!(
                    !self.engine.operation_error,
                    "{}",
                    self.engine.operation_status
                );
                self.engine.mark("conflicts abort frame");
                self.conflicts_finish();
            }
            _ => {}
        }
    }

    fn conflicts_finish(&mut self) {
        self.engine.mark("conflicts final frame");
        let run = self.conflicts_bench.run.as_mut().unwrap();
        run.checks = if run.kind == "abort" { 7 } else { 18 };
        run.verified = true;
    }

    pub(super) fn conflicts_ready(&self) -> Option<bool> {
        self.conflicts_bench.run.as_ref().map(|r| r.verified)
    }
    pub(super) fn conflicts_assert(&self) {
        if let Some(run) = &self.conflicts_bench.run {
            assert!(run.verified && run.checks > 0 && self.engine.operations.conflict.is_none());
        }
    }
    pub(super) fn conflicts_summary(&self) -> String {
        self.conflicts_bench
            .run
            .as_ref()
            .map(|r| {
                format!(
                    "conflicts_verified={} conflicts_kind={} conflicts_checks={}",
                    r.verified, r.kind, r.checks
                )
            })
            .unwrap_or_default()
    }
}
