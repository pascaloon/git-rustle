//! A two-click, file-bound destructive action in the pending file list.
use super::*;
use crate::git::worktree::DiscardRequest;
use std::{cell::Cell, rc::Rc};

#[derive(Default)]
pub(super) struct Scenario {
    phase: u8,
    scheduled: bool,
    paint: Rc<Cell<u8>>,
    preserved_index: Vec<u8>,
    preserved_untracked: Vec<u8>,
    pub(super) verified: bool,
}

pub(super) struct Menu {
    position: Point<Pixels>,
    repository: String,
    request: DiscardRequest,
    armed: bool,
}

impl Desktop {
    fn discard_idle(&self) -> bool {
        !self.engine.operation_busy
            && !self.engine.repository_edit_busy
            && !self.engine.app.pending.busy
            && !self.engine.app.branches.busy
            && !self.engine.app.settings.saving
            && !self.external_merge_busy()
            && self.engine.operations.conflict.is_none()
            && !self.engine.app.pending.snapshot.conflicts
    }

    pub(super) fn open_discard_menu(
        &mut self,
        node: usize,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.pending_discard = None;
        if !self.discard_idle() {
            return;
        }
        let changes = match &self.engine.app.mode {
            Mode::Diff(diff) => Some(&diff.changes),
            Mode::Graph if self.engine.app.pending.selected => {
                self.engine.app.details_changes.as_ref()
            }
            _ => None,
        };
        let Some(file) = changes.and_then(|changes| {
            changes
                .tree
                .nodes
                .get(node)
                .and_then(|node| node.file)
                .and_then(|i| changes.files.get(i))
        }) else {
            return;
        };
        let snapshot = &self.engine.app.pending.snapshot;
        if !file.worktree || !snapshot.files.contains(file) {
            return;
        }
        self.pending_discard = Some(Menu {
            position,
            repository: self.engine.repository_path(),
            request: DiscardRequest::new(snapshot, file),
            armed: false,
        });
        self.context_menu = None;
        cx.notify();
    }

    pub(super) fn discard_menu(&self, window: &Window, cx: &Context<Self>) -> Option<AnyElement> {
        let menu = self.pending_discard.as_ref()?;
        if menu.repository != self.engine.repository_path() {
            return None;
        }
        let valid = self.discard_idle()
            && menu.request.fingerprint == self.engine.app.pending.snapshot.fingerprint
            && self
                .engine
                .app
                .pending
                .snapshot
                .files
                .contains(&menu.request.file);
        let label = if menu.armed {
            "Confirm discard file — click again"
        } else {
            "Discard file"
        };
        let color = if !valid {
            MUTED
        } else if menu.armed {
            REMOVED
        } else {
            TEXT
        };
        let paint = self.discard_scenario.paint.clone();
        let phase = if menu.armed && color == REMOVED { 2 } else { 1 };
        Some(
            deferred(
                column()
                    .id("pending-file-menu")
                    .absolute()
                    .left(
                        menu.position
                            .x
                            .min(window.viewport_size().width - px(340.))
                            .max(px(0.)),
                    )
                    .top(
                        menu.position
                            .y
                            .min(window.viewport_size().height - px(88.))
                            .max(px(0.)),
                    )
                    .w(px(340.))
                    .p_1()
                    .bg(rgb(PANEL))
                    .border_1()
                    .border_color(rgb(LINE))
                    .rounded_md()
                    .shadow_lg()
                    .occlude()
                    .on_mouse_down_out(cx.listener(|v, _, _, cx| {
                        v.pending_discard = None;
                        cx.notify();
                    }))
                    .child(caption(menu.request.file.path.clone()).px_2().py_1())
                    .child(
                        row()
                            .id("pending-discard-file")
                            .h(px(36.))
                            .px_2()
                            .rounded_sm()
                            .text_color(rgb(color))
                            .child(label)
                            .child(
                                canvas(
                                    |_, _, _| (),
                                    move |bounds, _, _, _| {
                                        assert!(bounds.size.height > px(0.));
                                        paint.set(phase);
                                    },
                                )
                                .w(px(1.))
                                .h(px(30.)),
                            )
                            .when(valid, |s| {
                                s.cursor_pointer()
                                    .hover(|s| s.bg(rgb(SELECTED_HOVER)))
                                    .on_click(cx.listener(|v, _, window, cx| {
                                        v.confirm_discard_file(window, cx);
                                    }))
                            }),
                    ),
            )
            .into_any_element(),
        )
    }

    pub(super) fn confirm_discard_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.discard_idle() {
            self.pending_discard = None;
            cx.notify();
            return;
        }
        let Some(menu) = self.pending_discard.as_mut() else {
            return;
        };
        if menu.request.fingerprint != self.engine.app.pending.snapshot.fingerprint {
            self.pending_discard = None;
            self.notice = "Pending changes changed. Open Discard file again.".into();
        } else if !menu.armed {
            menu.armed = true;
        } else {
            let menu = self.pending_discard.take().unwrap();
            if menu.repository == self.engine.repository_path() {
                self.action(
                    Action::Operation(Box::new(operations::Action::Run(
                        crate::git::operations::Request::DiscardFile(menu.request),
                    ))),
                    window,
                    cx,
                );
            }
        }
        cx.notify();
    }

    pub(super) fn discard_scenario_schedule(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.scenario.as_deref() != Some("pending-discard")
            || !self.scenario_started
            || self.discard_scenario.verified
            || self.discard_scenario.scheduled
        {
            return;
        }
        self.discard_scenario.scheduled = true;
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = weak.update(cx, |v, cx| {
                v.discard_scenario.scheduled = false;
                v.discard_scenario_tick(window, cx);
                cx.notify();
            });
        });
    }

    fn discard_scenario_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root = std::path::PathBuf::from(self.engine.repository_path());
        match self.discard_scenario.phase {
            0 => {
                assert_eq!(
                    std::fs::read_to_string(root.join(".git/git-rustle-discard-fixture"))
                        .unwrap()
                        .trim(),
                    "disposable discard benchmark fixture"
                );
                if !self.engine.app.pending.known || !self.discard_idle() {
                    return;
                }
                self.action(Action::Head, window, cx);
                if !self.engine.app.pending.selected {
                    return;
                }
                let Some(changes) = &self.engine.app.details_changes else {
                    return;
                };
                let Some(node) =
                    changes.tree.nodes.iter().position(|node| {
                        node.file.is_some_and(|i| changes.files[i].path == "a.txt")
                    })
                else {
                    return;
                };
                self.discard_scenario.preserved_index =
                    crate::git::command::checked(&root, &["show", ":b.txt"]).unwrap();
                self.discard_scenario.preserved_untracked =
                    std::fs::read(root.join("untracked.txt")).unwrap();
                self.engine.mark("desktop discard menu requested");
                self.open_discard_menu(node, point(px(700.), px(300.)), cx);
                assert!(self.pending_discard.is_some());
                self.discard_scenario.phase = 1;
            }
            1 if self.discard_scenario.paint.get() == 1 => {
                self.engine.mark("desktop discard menu painted");
                self.confirm_discard_file(window, cx);
                assert!(self.pending_discard.as_ref().unwrap().armed);
                assert!(!self.engine.operation_busy);
                self.discard_scenario.phase = 2;
            }
            2 if self.discard_scenario.paint.get() == 2 => {
                self.engine.mark("desktop discard armed red painted");
                self.engine.mark("desktop discard requested");
                self.confirm_discard_file(window, cx);
                assert!(self.pending_discard.is_none());
                self.discard_scenario.phase = 3;
            }
            3 if !self.engine.operation_busy && !self.engine.app.pending.busy => {
                if self
                    .engine
                    .app
                    .pending
                    .snapshot
                    .files
                    .iter()
                    .any(|f| f.path == "a.txt")
                {
                    return;
                }
                let head = crate::git::command::checked(&root, &["show", "HEAD:a.txt"]).unwrap();
                assert_eq!(std::fs::read(root.join("a.txt")).unwrap(), head);
                assert_eq!(
                    crate::git::command::checked(&root, &["show", ":b.txt"]).unwrap(),
                    self.discard_scenario.preserved_index
                );
                assert_eq!(
                    std::fs::read(root.join("untracked.txt")).unwrap(),
                    self.discard_scenario.preserved_untracked
                );
                self.engine.mark("desktop discard verified frame");
                self.discard_scenario.verified = true;
            }
            _ => {}
        }
    }
}
