//! Opt-in real-action and paint validation for graph selection and inspector views.
use super::*;
use std::{cell::RefCell, collections::HashMap, rc::Rc};
type Paints = Rc<RefCell<HashMap<(usize, u8), Bounds<Pixels>>>>;
#[derive(Default)]
pub(super) struct State {
    phase: u8,
    scheduled: bool,
    pub(super) verified: bool,
    paints: Paints,
    head: Option<gix::ObjectId>,
}
impl Desktop {
    fn graph_feature_name(&self) -> Option<&str> {
        self.scenario.as_deref().filter(|name| {
            matches!(
                *name,
                "graph-multiselect"
                    | "multi-details"
                    | "dense-labels"
                    | "checkout-simple"
                    | "diff-base"
            )
        })
    }
    pub(super) fn graph_feature_probe(&self, row: usize, part: u8) -> Option<AnyElement> {
        self.graph_feature_name()?;
        if part == 4 && !self.multi_details_loaded(2) {
            return None;
        }
        if part == 5
            && match self.graph_feature_name().unwrap() {
                "diff-base" => {
                    !matches!(&self.engine.app.mode,Mode::Diff(view) if view.changes.loaded)
                }
                "checkout-simple" => {
                    self.engine
                        .app
                        .refs
                        .as_ref()
                        .and_then(|r| r.head_branch.as_deref())
                        != Some("topic")
                }
                _ => true,
            }
        {
            return None;
        }
        let paints = self.graph_features.paints.clone();
        Some(
            canvas(
                |bounds, _, _| bounds,
                move |bounds, _, _, _| {
                    paints.borrow_mut().insert((row, part), bounds);
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .into_any_element(),
        )
    }
    pub(super) fn graph_feature_schedule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.graph_feature_name().is_none()
            || !self.scenario_started
            || self.graph_features.verified
            || self.graph_features.scheduled
        {
            return;
        }
        self.graph_features.scheduled = true;
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = weak.update(cx, |v, cx| {
                v.graph_features.scheduled = false;
                v.graph_feature_tick(window, cx);
                cx.notify();
            });
        });
    }
    fn graph_feature_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.graph_feature_name().unwrap().to_owned();
        if self.graph_features.phase == 0 {
            if !self.engine.app.history.complete
                || self.engine.app.history.len() < 3
                || !self.engine.app.pending.known
            {
                return;
            }
            self.graph_features.head = self.engine.app.head_id();
            self.engine.mark("graph feature requested");
            match name.as_str() {
                "checkout-simple" => {
                    let root = self.engine.repository_root().unwrap();
                    assert_eq!(
                        std::fs::read_to_string(root.join(".git/git-rustle-graph-fixture"))
                            .unwrap()
                            .trim(),
                        "disposable graph feature fixture"
                    );
                    let target = self
                        .engine
                        .app
                        .refs
                        .as_ref()
                        .unwrap()
                        .refs
                        .iter()
                        .find(|r| r.kind == crate::git::RefKind::Branch && r.name == "topic")
                        .unwrap()
                        .id;
                    let index = self.engine.app.history.index_of(&target).unwrap();
                    self.action(Action::Select(index, false), window, cx);
                    self.action(Action::Checkout, window, cx);
                    assert!(self.engine.app.checkout_confirmation.is_none());
                }
                "diff-base" => {
                    let target = self
                        .engine
                        .app
                        .refs
                        .as_ref()
                        .unwrap()
                        .refs
                        .iter()
                        .find(|r| r.name == "topic")
                        .unwrap()
                        .id;
                    let index = self.engine.app.history.index_of(&target).unwrap();
                    self.action(Action::Select(index, false), window, cx);
                    self.action(Action::DiffBase, window, cx);
                }
                _ => {
                    self.action(Action::Select(0, false), window, cx);
                    if name == "dense-labels" {
                        let id = self.engine.app.history.commits[0].id;
                        self.engine.app.labels.insert(
                            id,
                            (0..8)
                                .map(|n| crate::app::Label {
                                    name: format!(
                                        "topic/very-long-reference-name-{n}-with-a-long-description"
                                    ),
                                    kind: crate::git::RefKind::Branch,
                                    remote_only: false,
                                })
                                .collect(),
                        );
                    } else {
                        self.action(Action::Select(1, true), window, cx);
                        self.action(Action::Select(2, true), window, cx);
                        if name == "multi-details" {
                            let id = self.selected_commits.ids(self.engine.app.selected_id())[0];
                            self.toggle_multi_detail(id, cx);
                        }
                    }
                    self.graph_scroll.scroll_to_item(0, ScrollStrategy::Top);
                }
            }
            self.graph_features.paints.borrow_mut().clear();
            self.graph_features.phase = 1;
            return;
        }
        if self.graph_features.phase != 1 {
            return;
        }
        if name == "checkout-simple" {
            if self.engine.operation_busy
                || self.engine.app.branches.busy
                || self
                    .engine
                    .app
                    .refs
                    .as_ref()
                    .and_then(|r| r.head_branch.as_deref())
                    != Some("topic")
            {
                return;
            }
            assert!(
                self.engine
                    .app
                    .checkout_notice
                    .as_ref()
                    .is_some_and(Result::is_ok)
            );
            if !self.graph_features.paints.borrow().contains_key(&(0, 5)) {
                return;
            }
        } else if name == "diff-base" {
            if self.base_diff.busy {
                return;
            }
            let Mode::Diff(view) = &self.engine.app.mode else {
                return;
            };
            if !view.changes.loaded {
                return;
            }
            assert!(view.changes.old.is_some());
            if !self.graph_features.paints.borrow().contains_key(&(0, 5)) {
                return;
            }
        } else {
            let paints = self.graph_features.paints.borrow();
            let Some(sha) = paints.get(&(0, 0)) else {
                return;
            };
            let Some(middle) = paints.get(&(0, 1)) else {
                return;
            };
            let Some(author) = paints.get(&(0, 2)) else {
                return;
            };
            let Some(time) = paints.get(&(0, 3)) else {
                return;
            };
            assert!(sha.size.width > px(0.) && middle.size.width > px(0.));
            assert!(
                middle.right() <= author.left() + px(1.) && author.right() <= time.left() + px(1.)
            );
            assert!(time.right() <= window.viewport_size().width);
            assert!(
                self.graph_selected_path
                    .node_on_path(0, self.engine.app.history.commits[0].node_lane as usize)
            );
            if name == "multi-details"
                && (!self.multi_details_loaded(2) || !paints.contains_key(&(0, 4)))
            {
                return;
            }
            if name != "dense-labels" {
                assert_eq!(
                    self.selected_commits
                        .ids(self.engine.app.selected_id())
                        .len(),
                    3
                );
            }
        }
        if name != "checkout-simple" {
            assert_eq!(self.engine.app.head_id(), self.graph_features.head);
        }
        self.graph_features.verified = true;
        self.engine.mark("graph feature verified frame");
    }
}
