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
                    | "pending-graph"
                    | "stash-preview"
                    | "stash-file"
                    | "stash-pop"
                    | "dense-labels"
                    | "checkout-simple"
                    | "diff-base"
                    | "diff-base-choose"
            )
        })
    }
    pub(super) fn graph_feature_probe(&self, row: usize, part: u8) -> Option<AnyElement> {
        self.graph_feature_name()?;
        if part == 4 && !self.multi_details_loaded(2) {
            return None;
        }
        if part == 7
            && !self
                .engine
                .app
                .details_changes
                .as_ref()
                .is_some_and(|cs| cs.loaded)
        {
            return None;
        }
        if part == 5
            && match self.graph_feature_name().unwrap() {
                "diff-base" | "diff-base-choose" | "stash-file" => {
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
                "diff-base" | "diff-base-choose" => {
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
                    self.action(
                        Action::Select((index + 1).min(self.engine.app.history.len() - 1), false),
                        window,
                        cx,
                    );
                    if name == "diff-base-choose" {
                        self.start_base_diff_choose_for(target, cx);
                    } else {
                        self.start_base_diff_for(target, None, cx);
                    }
                }
                "stash-preview" | "stash-file" | "stash-pop" => {
                    self.action(Action::Tab(Tab::Stashes), window, cx);
                }
                _ => {
                    if name == "pending-graph" {
                        assert!(self.engine.app.pending.exists());
                        assert!(self.engine.app.pending.head_index.unwrap() > 0);
                        let row = self.engine.app.pending.row();
                        self.action(Action::Select(0, false), window, cx);
                        self.action(Action::Select(1, true), window, cx);
                        assert_eq!(
                            self.selected_commits.count(self.engine.app.selected_id()),
                            2
                        );
                        self.action(Action::Select(row, true), window, cx);
                        assert!(self.engine.app.pending.selected);
                        assert_eq!(
                            self.selected_commits.count(self.engine.app.selected_id()),
                            0
                        );
                        self.action(Action::Select(0, true), window, cx);
                        assert!(!self.engine.app.pending.selected);
                        assert_eq!(
                            self.selected_commits.count(self.engine.app.selected_id()),
                            1
                        );
                        self.action(Action::Select(row, true), window, cx);
                        self.graph_scroll.scroll_to_item(0, ScrollStrategy::Top);
                        self.graph_features.paints.borrow_mut().clear();
                        self.graph_features.phase = 1;
                        return;
                    }
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
                        if name != "multi-details" {
                            self.action(Action::Select(2, true), window, cx);
                        }
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
        if name.starts_with("stash-") {
            let phase = self.graph_features.phase;
            if phase == 1 {
                if !self.stashes_ready() {
                    return;
                }
                let stash = self
                    .stash_entries()
                    .first()
                    .expect("populated stash fixture")
                    .clone();
                if name == "stash-pop" {
                    self.stash_action(stashes::Action::Pop(stash), window, cx);
                } else {
                    if let Some(other) = self.stash_entries().get(1).cloned() {
                        self.stash_action(stashes::Action::Select(other), window, cx);
                    }
                    self.stash_action(stashes::Action::Select(stash.clone()), window, cx);
                    assert_eq!(self.stash_selected_oid(), Some(stash.oid.as_str()));
                }
                self.graph_features.phase = 2;
                self.graph_features.paints.borrow_mut().clear();
                return;
            }
            if phase == 2 && name == "stash-pop" {
                if !self.operations.modal_open() {
                    return;
                }
                self.operation_action(operations::Action::Confirm, window, cx);
                self.graph_features.phase = 3;
                return;
            }
            if phase == 2 {
                let stash = self.stash_entries()[0].clone();
                let Some(files) = self.stash_preview_file_count(&stash) else {
                    return;
                };
                assert!(files >= 2, "tracked and untracked stash preview");
                assert_eq!(self.stash_selected_oid(), Some(stash.oid.as_str()));
                let paints = self.graph_features.paints.borrow();
                let Some(inspector) = paints.get(&(0, 9)) else {
                    return;
                };
                let Some(file) = paints.get(&(0, 8)) else {
                    return;
                };
                let (Some(avatar), Some(copy)) = (paints.get(&(0, 10)), paints.get(&(0, 11)))
                else {
                    return;
                };
                assert!((avatar.size.width - px(28.)).abs() <= px(1.));
                assert!((avatar.size.height - px(28.)).abs() <= px(1.));
                assert!(copy.size.width >= px(20.) && copy.size.height >= px(20.));
                for control in [avatar, copy] {
                    assert!(
                        control.left() >= inspector.left() && control.right() <= inspector.right()
                    );
                    assert!(
                        control.top() >= inspector.top()
                            && control.bottom() <= inspector.top() + px(44.)
                    );
                }
                assert!(
                    avatar.right() <= copy.left(),
                    "stash avatar and copy control do not overlap"
                );
                assert!(inspector.size.width >= px(240.));
                assert!(file.left() >= inspector.left() && file.right() <= inspector.right());
                assert!(inspector.right() <= window.viewport_size().width);
                drop(paints);
                if !self.graph_features.paints.borrow().contains_key(&(0, 8)) {
                    return;
                }
                if name == "stash-file" {
                    self.stash_action(stashes::Action::File(stash, 0), window, cx);
                    self.graph_features.phase = 3;
                    return;
                }
            } else if phase == 3 && name == "stash-file" {
                if !matches!(&self.engine.app.mode,Mode::Diff(view) if view.diff.is_some() && view.changes.loaded)
                {
                    return;
                }
                if !self.graph_features.paints.borrow().contains_key(&(0, 5)) {
                    return;
                }
            } else if phase == 3 && name == "stash-pop" {
                if self.stashes.open
                    || !self.engine.app.pending.selected
                    || self.engine.app.pending.loading
                    || self.engine.operation_busy
                {
                    return;
                }
                assert!(self.engine.app.pending.exists());
                assert_eq!(self.stash_entries().len(), 2);
                if !self.graph_features.paints.borrow().contains_key(&(0, 6)) {
                    return;
                }
            } else {
                return;
            }
            assert_eq!(self.engine.app.head_id(), self.graph_features.head);
            self.graph_features.verified = true;
            self.engine.mark("graph feature verified frame");
            return;
        }
        if self.graph_features.phase != 1 && name != "diff-base-choose" {
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
        } else if name == "diff-base" || name == "diff-base-choose" {
            if name == "diff-base-choose" && self.graph_features.phase == 1 {
                if self.base_diff.busy
                    || self.base_diff.choices.is_empty()
                    || !self.base_diff_scenario_filter("main", window, cx)
                {
                    return;
                }
                self.graph_features.phase = 2;
                self.graph_features.paints.borrow_mut().clear();
                return;
            }
            if name == "diff-base-choose" && self.graph_features.phase == 2 {
                let paints = self.graph_features.paints.borrow();
                let (Some(list), Some(cancel), Some(filter), Some(dialog), Some(compare)) = (
                    paints.get(&(0, 12)),
                    paints.get(&(0, 13)),
                    paints.get(&(0, 14)),
                    paints.get(&(0, 15)),
                    paints.get(&(0, 16)),
                ) else {
                    return;
                };
                assert!(list.size.height <= px(281.) && list.size.height >= px(200.));
                assert!(list.bottom() <= cancel.top() && list.bottom() <= compare.top());
                assert!(filter.bottom() <= list.top());
                assert!(cancel.bottom() <= dialog.bottom() && compare.right() <= dialog.right());
                assert!(dialog.top() >= px(0.) && dialog.bottom() <= window.viewport_size().height);
                drop(paints);
                if std::env::var("GIT_RUSTLE_BASE_CHOOSER_CAPTURE").as_deref() == Ok("1") {
                    self.graph_features.verified = true;
                    self.engine.mark("graph feature verified frame");
                    return;
                }
                self.base_diff_scenario_choose("main", cx);
                self.graph_features.phase = 3;
                self.graph_features.paints.borrow_mut().clear();
                return;
            }
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
            assert!(
                view.changes
                    .files
                    .iter()
                    .any(|file| file.path == "file.txt")
            );
            assert!(
                !view
                    .changes
                    .files
                    .iter()
                    .any(|file| file.path == "main.txt"),
                "branch diff uses common ancestor rather than the advanced default tip"
            );
            if !self.graph_features.paints.borrow().contains_key(&(0, 5)) {
                return;
            }
        } else if name == "pending-graph" {
            if !self.graph_features.paints.borrow().contains_key(&(0, 6)) {
                return;
            }
            assert!(self.engine.app.pending.selected);
            assert_eq!(
                self.selected_commits.count(self.engine.app.selected_id()),
                0
            );
            assert!(
                self.engine
                    .app
                    .details_changes
                    .as_ref()
                    .is_some_and(|cs| cs.checked.is_some())
            );
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
            if name == "multi-details" {
                let Some(cs) = &self.engine.app.details_changes else {
                    return;
                };
                if !cs.loaded || !paints.contains_key(&(0, 7)) {
                    return;
                }
                assert!(cs.error.is_none());
                let ids = self.selected_commits.ids(self.engine.app.selected_id());
                assert_eq!(ids.len(), 2);
                assert!(ids.contains(&cs.new) && ids.contains(&cs.old.unwrap()));
            } else if name == "graph-multiselect" {
                assert!(self.engine.app.details_changes.is_none());
            }
            if name != "dense-labels" {
                assert_eq!(
                    self.selected_commits
                        .ids(self.engine.app.selected_id())
                        .len(),
                    if name == "multi-details" { 2 } else { 3 }
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
