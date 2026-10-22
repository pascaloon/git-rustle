//! Opt-in graph menu/cherry-pick actions with native paint acknowledgements.
use super::*;
use std::{cell::RefCell, collections::HashMap, rc::Rc};
#[derive(Default)]
pub(super) struct State {
    phase: u8,
    scheduled: bool,
    pub(super) verified: bool,
    head: Option<gix::ObjectId>,
    expected: Vec<String>,
    paints: Rc<RefCell<HashMap<u8, Bounds<Pixels>>>>,
}
impl Desktop {
    fn graph_context_name(&self) -> Option<&str> {
        self.scenario.as_deref().filter(|s| {
            matches!(
                *s,
                "graph-commit-menu"
                    | "graph-branch-menu"
                    | "cherry-pick-one"
                    | "cherry-pick-many"
                    | "cherry-pick-review"
            )
        })
    }
    pub(super) fn graph_context_ready(&self) -> Option<bool> {
        self.graph_context_name()
            .map(|_| self.graph_context.verified)
    }
    pub(super) fn graph_context_probe(&self, part: u8) -> Option<AnyElement> {
        self.graph_context_name()?;
        let paints = self.graph_context.paints.clone();
        Some(
            canvas(
                |bounds, _, _| bounds,
                move |bounds, _, _, _| {
                    paints.borrow_mut().insert(part, bounds);
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .into_any_element(),
        )
    }
    pub(super) fn graph_context_schedule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.graph_context_name().is_none()
            || !self.scenario_started
            || self.graph_context.verified
            || self.graph_context.scheduled
        {
            return;
        }
        self.graph_context.scheduled = true;
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = weak.update(cx, |v, cx| {
                v.graph_context.scheduled = false;
                v.graph_context_tick(window, cx);
                cx.notify();
            });
        });
    }
    fn graph_context_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.graph_context_name().unwrap().to_owned();
        let phase = self.graph_context.phase;
        if phase == 0 {
            if !self.engine.app.history.complete
                || !self.engine.app.pending.known
                || self.engine.operation_busy
            {
                return;
            }
            let root = self.engine.repository_root().unwrap();
            assert_eq!(
                std::fs::read_to_string(root.join(".git/git-rustle-graph-fixture"))
                    .unwrap()
                    .trim(),
                "disposable graph feature fixture"
            );
            self.graph_context.head = self.engine.app.head_id();
            let first = self.engine.app.history.commits[(0..self.engine.app.history.len())
                .find(|i| self.engine.app.history.subject(*i) == "Topic commit 0")
                .unwrap()]
            .id;
            let last = self.engine.app.history.commits[(0..self.engine.app.history.len())
                .find(|i| self.engine.app.history.subject(*i) == "Topic commit 1")
                .unwrap()]
            .id;
            let selected = if name == "cherry-pick-one" {
                first
            } else {
                last
            };
            let index = self.engine.app.history.index_of(&selected).unwrap();
            let row = (0..self.engine.app.graph_len())
                .find(|row| self.engine.app.history_index(*row) == Some(index))
                .unwrap();
            self.action(Action::Select(row, false), window, cx);
            if matches!(name.as_str(), "cherry-pick-many" | "cherry-pick-review") {
                let index = self.engine.app.history.index_of(&first).unwrap();
                let row = (0..self.engine.app.graph_len())
                    .find(|row| self.engine.app.history_index(*row) == Some(index))
                    .unwrap();
                self.action(Action::Select(row, true), window, cx);
            }
            self.engine.mark("graph context requested");
            if name.starts_with("graph-") {
                self.context_menu = Some(point(px(220.), px(210.)));
                self.context_menu_group = Some(if name == "graph-commit-menu" {
                    "Commit"
                } else {
                    "Branch"
                });
            } else {
                let ids = self.graph_cherry_pick_oids();
                let expected = if name == "cherry-pick-one" {
                    vec![first.to_string()]
                } else {
                    vec![first.to_string(), last.to_string()]
                };
                assert_eq!(
                    ids, expected,
                    "Frozen replay order must be oldest first, regardless of click order"
                );
                self.graph_context.expected = ids.clone();
                self.operation_action(operations::Action::CherryPick(ids), window, cx);
                assert!(self.operations.confirmation.is_some());
            }
            self.graph_context.paints.borrow_mut().clear();
            self.graph_context.phase = 1;
            return;
        }
        if phase == 1 {
            if name.starts_with("graph-") {
                let paints = self.graph_context.paints.borrow();
                let (Some(root), Some(sub)) = (paints.get(&1), paints.get(&2)) else {
                    return;
                };
                // Probes fill the menu's inner content box: each 320px outer
                // column reserves 4px padding and 1px border per side. Permit
                // GPUI's absolute-child box convention up to the outer width,
                // but never accept content narrower than the intended 310px.
                for (name, bounds) in [("root", root), ("submenu", sub)] {
                    assert!(
                        bounds.size.width >= px(310.) && bounds.size.width <= px(320.),
                        "{name} menu content width must be 310..320px for a 320px padded/bordered outer column; root={root:?}, submenu={sub:?}, viewport={:?}",
                        window.viewport_size()
                    );
                    assert!(
                        bounds.size.height >= px(36.),
                        "{name} menu must paint at least one complete action row; bounds={bounds:?}"
                    );
                }
                let horizontal_offset = f32::from(sub.left() - root.left());
                assert!(
                    (horizontal_offset - 324.).abs() <= 1.,
                    "Submenu must start 324px after root (320px menu plus 4px gap); actual_offset={horizontal_offset}, root={root:?}, submenu={sub:?}"
                );
                assert!(
                    sub.right() <= window.viewport_size().width
                        && sub.bottom() <= window.viewport_size().height
                        && sub.top() >= px(0.)
                        && root.left() >= px(0.),
                    "Menu content must stay inside the viewport; root={root:?}, submenu={sub:?}, viewport={:?}",
                    window.viewport_size()
                );
                drop(paints);
                self.graph_context.verified = true;
                self.engine.mark("graph context verified frame");
                return;
            }
            if !self.graph_context.paints.borrow().contains_key(&4) {
                return;
            }
            let text = &self.operations.confirmation.as_ref().unwrap().1;
            for id in &self.graph_context.expected {
                assert!(text.contains(id));
            }
            if name == "cherry-pick-review" {
                self.graph_context.verified = true;
                self.engine.mark("graph context verified frame");
                return;
            }
            self.engine.mark("cherry pick confirmed");
            self.operation_action(operations::Action::Confirm, window, cx);
            self.graph_context.phase = 2;
            return;
        }
        if phase == 2 {
            if self.engine.operation_busy
                || self.engine.app.head_id() == self.graph_context.head
                || !self.engine.app.history.complete
            {
                return;
            }
            assert!(
                !self.engine.operation_error,
                "{}",
                self.engine.operation_status
            );
            assert!(self.engine.operations.conflict.is_none());
            let root = self.engine.repository_root().unwrap();
            let file = std::fs::read_to_string(root.join("file.txt")).unwrap();
            assert_eq!(
                file.trim(),
                if name == "cherry-pick-one" {
                    "topic 0"
                } else {
                    "topic 1"
                }
            );
            assert_eq!(
                std::fs::read_to_string(root.join("main.txt"))
                    .unwrap()
                    .trim(),
                "main advanced"
            );
            self.graph_context.paints.borrow_mut().clear();
            self.graph_context.phase = 3;
            return;
        }
        if phase == 3 {
            if !self.graph_context.paints.borrow().contains_key(&3) {
                return;
            }
            self.engine.mark("cherry pick applied frame");
            self.operation_action(
                operations::Action::Run(crate::git::operations::Request::MoveTo(0)),
                window,
                cx,
            );
            self.graph_context.phase = 4;
            return;
        }
        if phase == 4 {
            if self.engine.operation_busy
                || self.engine.app.head_id() != self.graph_context.head
                || !self.engine.app.history.complete
            {
                return;
            }
            assert!(
                !self.engine.operation_error,
                "{}",
                self.engine.operation_status
            );
            let root = self.engine.repository_root().unwrap();
            assert_eq!(
                std::fs::read_to_string(root.join("file.txt"))
                    .unwrap()
                    .trim(),
                "base 3"
            );
            self.graph_context.paints.borrow_mut().clear();
            self.graph_context.phase = 5;
            return;
        }
        if self.graph_context.paints.borrow().contains_key(&3) {
            self.graph_context.verified = true;
            self.engine.mark("graph context verified frame");
        }
    }
}
