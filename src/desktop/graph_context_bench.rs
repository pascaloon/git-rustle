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
    safe_action: RefCell<Option<Action>>,
    menu_position: Option<Point<Pixels>>,
    pub(super) submenu_scroll: ScrollHandle,
    submenu_viewport: Option<Bounds<Pixels>>,
}
impl Desktop {
    pub(super) fn graph_context_reset_actions(&self) {
        if self.graph_context_name().is_some() {
            self.graph_context.safe_action.borrow_mut().take();
        }
    }
    pub(super) fn graph_context_safe_probe(&self, action: &Action) -> Option<AnyElement> {
        self.graph_context_name()?;
        if self.graph_context.safe_action.borrow().is_some() {
            return None;
        }
        let request_case = self
            .graph_context_name()
            .is_some_and(|name| name.starts_with("graph-request-"));
        let safe = if request_case {
            matches!(action, Action::NewRequest(_))
        } else {
            matches!(action, Action::Operation(operation) if matches!(operation.as_ref(), operations::Action::Open(operations::Dialog::CreateTag | operations::Dialog::Rename, ..)))
        };
        if !safe {
            return None;
        }
        *self.graph_context.safe_action.borrow_mut() = Some(action.clone());
        self.graph_context_probe(8)
    }
    fn graph_context_name(&self) -> Option<&str> {
        self.scenario.as_deref().filter(|s| {
            matches!(
                *s,
                "graph-commit-menu"
                    | "graph-request-menu"
                    | "graph-request-compose"
                    | "graph-branch-menu"
                    | "graph-commit-menu-edge"
                    | "graph-branch-menu-edge"
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
        if name.starts_with("graph-request-") && phase > 0 {
            if phase == 1 || phase == 3 {
                let paints = self.graph_context.paints.borrow();
                let (Some(root), Some(item)) = (paints.get(&1), paints.get(&8)) else {
                    return;
                };
                assert_eq!(
                    self.context_menu_group, None,
                    "Request action must be directly available in the row menu"
                );
                assert_eq!(item.size.height, px(36.));
                assert!(item.top() >= root.top() && item.bottom() <= root.bottom());
                assert!(item.right() <= window.viewport_size().width);
                drop(paints);
                if phase == 3 {
                    self.graph_context.verified = true;
                    self.engine.mark("graph context verified frame");
                    return;
                }
                let action = self.graph_context.safe_action.borrow().clone().unwrap();
                assert!(
                    matches!(&action,Action::NewRequest(source) if source == &self.graph_context.expected[0])
                );
                self.graph_context.menu_position = self.context_menu;
                self.graph_context.paints.borrow_mut().clear();
                self.graph_menu_activate(action, window, cx);
                assert!(self.context_menu.is_none());
                self.graph_context.phase = 2;
                return;
            }
            if phase == 2 {
                if !self.graph_request_source_painted(&self.graph_context.expected[0], cx) {
                    return;
                }
                self.engine.mark("graph request composer verified frame");
                if name == "graph-request-compose" {
                    self.graph_context.verified = true;
                    self.engine.mark("graph context verified frame");
                } else {
                    self.composer = None;
                    self.context_menu = self.graph_context.menu_position;
                    self.context_menu_group = None;
                    self.graph_context.paints.borrow_mut().clear();
                    self.graph_context.phase = 3;
                }
                return;
            }
        }
        if phase == 10 {
            let expected = if name.starts_with("graph-commit-") {
                "Commit"
            } else {
                "Branch"
            };
            assert_eq!(
                self.context_menu_group,
                Some(expected),
                "Shared hover handler must retain the submenu across its painted horizontal path"
            );
            let Some(bounds) = self.graph_context.paints.borrow().get(&8).copied() else {
                return;
            };
            assert_eq!(
                bounds.size.height,
                px(36.),
                "Safe child action must retain its full row height"
            );
            let action = self.graph_context.safe_action.borrow().clone().unwrap();
            self.graph_context.menu_position = self.context_menu;
            self.graph_context.paints.borrow_mut().clear();
            self.graph_menu_activate(action, window, cx);
            assert!(self.operations.modal_open());
            let expected_dialog = if expected == "Commit" {
                "dialog=Some(CreateTag)"
            } else {
                "dialog=Some(Rename)"
            };
            assert!(self.operations.summary().contains(expected_dialog));
            self.graph_context.phase = 11;
            return;
        }
        if phase == 11 {
            if !self.graph_context.paints.borrow().contains_key(&9) {
                return;
            }
            self.operation_action(operations::Action::Cancel, window, cx);
            assert!(!self.operations.modal_open());
            self.context_menu = self.graph_context.menu_position;
            self.context_menu_group = Some(if name.starts_with("graph-commit-") {
                "Commit"
            } else {
                "Branch"
            });
            self.graph_context.paints.borrow_mut().clear();
            self.graph_context.phase = 12;
            return;
        }
        if phase == 12 {
            let paints = self.graph_context.paints.borrow();
            let part = if name.starts_with("graph-commit-") {
                5
            } else {
                6
            };
            let (Some(parent), Some(child)) = (paints.get(&part), paints.get(&7)) else {
                return;
            };
            assert!((f32::from(parent.top() - child.top())).abs() <= 1.);
            assert!(!self.operations.modal_open());
            if name == "graph-branch-menu-edge" {
                let Some(viewport) = paints.get(&2).copied() else {
                    return;
                };
                self.graph_context.submenu_viewport = Some(viewport);
                drop(paints);
                assert!(
                    self.graph_context.submenu_scroll.max_offset().height > px(36.),
                    "Long submenu fixture must actually overflow"
                );
                self.graph_context.submenu_scroll.scroll_to_bottom();
                self.graph_context.paints.borrow_mut().clear();
                self.graph_context.phase = 13;
                return;
            }
            self.graph_context.verified = true;
            self.engine.mark("graph context verified frame");
            return;
        }
        if phase == 13 {
            let paints = self.graph_context.paints.borrow();
            let Some(last) = paints.get(&10) else {
                return;
            };
            let viewport = self.graph_context.submenu_viewport.unwrap();
            assert!(
                (f32::from(last.size.height) - 36.).abs() <= 1.,
                "Last submenu row must retain full height: {last:?}"
            );
            assert!(
                last.top() >= viewport.top() - px(1.)
                    && last.bottom() <= viewport.bottom() + px(1.),
                "Scrolling must paint the complete last row within the submenu viewport: last={last:?}, viewport={viewport:?}"
            );
            assert!(self.graph_context.submenu_scroll.offset().y < px(0.));
            drop(paints);
            self.graph_context
                .submenu_scroll
                .set_offset(point(px(0.), px(0.)));
            self.graph_context.paints.borrow_mut().clear();
            self.graph_context.phase = 14;
            return;
        }
        if phase == 14 {
            let paints = self.graph_context.paints.borrow();
            let (Some(parent), Some(first)) = (paints.get(&6), paints.get(&7)) else {
                return;
            };
            assert!((f32::from(parent.top() - first.top())).abs() <= 1.);
            assert_eq!(self.graph_context.submenu_scroll.offset().y, px(0.));
            self.graph_context.verified = true;
            self.engine.mark("graph context verified frame");
            return;
        }
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
            if name.starts_with("graph-request-") {
                let source = self
                    .engine
                    .app
                    .refs
                    .as_ref()
                    .unwrap()
                    .head_branch
                    .clone()
                    .unwrap();
                let selected = self.engine.app.head_id().unwrap();
                let index = self.engine.app.history.index_of(&selected).unwrap();
                let row = (0..self.engine.app.graph_len())
                    .find(|row| self.engine.app.history_index(*row) == Some(index))
                    .unwrap();
                self.action(Action::Select(row, false), window, cx);
                self.graph_context.expected = vec![source];
                self.engine.mark("graph context requested");
                self.context_menu = Some(point(px(220.), px(210.)));
                self.context_menu_group = None;
                self.graph_context.paints.borrow_mut().clear();
                self.graph_context.phase = 1;
                return;
            }
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
                self.context_menu = Some(if name.ends_with("-edge") {
                    point(
                        window.viewport_size().width - px(12.),
                        window.viewport_size().height - px(12.),
                    )
                } else {
                    point(px(220.), px(210.))
                });
                self.context_menu_group = None;
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
                let group = if name.starts_with("graph-commit-") {
                    "Commit"
                } else {
                    "Branch"
                };
                let part = if group == "Commit" { 5 } else { 6 };
                if self.context_menu_group.is_none() {
                    let Some(bounds) = self.graph_context.paints.borrow().get(&part).copied()
                    else {
                        return;
                    };
                    assert!(bounds.size.height >= px(36.));
                    self.graph_menu_group_hover(group, cx);
                    return;
                }
                let paints = self.graph_context.paints.borrow();
                let (Some(root), Some(sub), Some(parent), Some(first)) = (
                    paints.get(&1),
                    paints.get(&2),
                    paints.get(&part),
                    paints.get(&7),
                ) else {
                    return;
                };
                assert!(
                    (f32::from(parent.top() - first.top())).abs() <= 1.,
                    "Submenu first action must align with its actual painted parent row: parent={parent:?}, first={first:?}"
                );
                assert_eq!(
                    first.size.height,
                    px(36.),
                    "First submenu row must never shrink to fit its scroll viewport"
                );
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
                    (horizontal_offset.abs() - 324.).abs() <= 1.,
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
                let y = parent.center().y;
                let edge = if horizontal_offset > 0. {
                    root.right()
                } else {
                    root.left()
                };
                let gap = if horizontal_offset > 0. {
                    edge + px(7.)
                } else {
                    edge - px(7.)
                };
                let destination = first.center().x;
                drop(paints);
                for x in [edge, gap, destination] {
                    assert!(x >= px(0.) && x <= window.viewport_size().width);
                    assert!(y >= px(0.) && y <= window.viewport_size().height);
                    self.graph_menu_group_hover(group, cx);
                }
                self.graph_context.phase = 10;
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
