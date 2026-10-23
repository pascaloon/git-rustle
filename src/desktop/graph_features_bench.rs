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
    ghost_api: Option<crate::remote::files::fixture::Api>,
    ghost_started: Option<Instant>,
}
impl Desktop {
    fn graph_feature_name(&self) -> Option<&str> {
        self.scenario.as_deref().filter(|name| {
            matches!(
                *name,
                "graph-multiselect"
                    | "graph-selection-boundary"
                    | "ghost-files-github"
                    | "ghost-files-gitlab"
                    | "multi-details"
                    | "pending-graph"
                    | "stash-preview"
                    | "stash-file"
                    | "stash-pop"
                    | "dense-labels"
                    | "checkout-simple"
                    | "detached-commit"
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
    fn ghost_files_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::remote::{
            Provider,
            files::fixture::{Api, id, install},
        };
        use std::sync::atomic::Ordering;
        let phase = self.graph_features.phase;
        if phase == 0 {
            let app = &self.engine.app;
            if !app.history.complete
                || !app.pending.known
                || app
                    .details
                    .as_ref()
                    .is_none_or(|d| Some(d.id) != app.head_id())
            {
                return;
            }
            assert!(!app.pending.exists());
            let root = self.engine.repository_root().unwrap();
            assert_eq!(
                std::fs::read_to_string(root.join(".git/git-rustle-graph-fixture"))
                    .unwrap()
                    .trim(),
                "disposable graph feature fixture"
            );
            let api = Api::start(if self.scenario.as_deref() == Some("ghost-files-gitlab") {
                Provider::Gitlab
            } else {
                Provider::Github
            });
            install(&mut self.engine.app, &api.remote);
            self.graph_features.head = self.engine.app.head_id();
            self.graph_features.ghost_api = Some(api);
            self.graph_features.ghost_started = Some(Instant::now());
            self.engine.mark("graph feature requested");
            let index = self.engine.app.history.index_of(&id(1)).unwrap();
            self.action(Action::Select(index, false), window, cx);
            let index = self.engine.app.history.index_of(&id(2)).unwrap();
            self.action(Action::Select(index, true), window, cx);
            self.graph_features.phase = 1;
            return;
        }
        assert!(
            self.graph_features.ghost_started.unwrap().elapsed() < Duration::from_secs(15),
            "Ghost files stalled in phase {phase}"
        );
        if phase == 1 {
            if !self
                .graph_features
                .ghost_api
                .as_ref()
                .unwrap()
                .blocked
                .load(Ordering::Acquire)
            {
                return;
            }
            self.engine.mark("ghost comparison blocked");
            let index = self.engine.app.history.index_of(&id(3)).unwrap();
            self.action(Action::Select(index, false), window, cx);
        } else {
            let expected = match phase {
                2 | 5 => id(3),
                3 => id(2),
                4 => id(4),
                6 => id(3),
                _ => unreachable!(),
            };
            let Some(cs) = self
                .engine
                .app
                .details_changes
                .as_ref()
                .filter(|c| c.new == expected && c.loaded)
            else {
                return;
            };
            let paints = self.graph_features.paints.borrow();
            let Some(bounds) = paints.get(&(0, 7)) else {
                return;
            };
            assert!(bounds.size.width > px(100.) && bounds.size.height > px(30.));
            assert!(
                bounds.left() >= px(0.)
                    && bounds.top() >= px(0.)
                    && bounds.right() <= window.viewport_size().width
                    && bounds.bottom() <= window.viewport_size().height
            );
            assert_eq!(cs.error.is_some(), phase == 4);
            if phase != 4 {
                assert_eq!(cs.files.len(), 1);
                assert_eq!(cs.files[0].path, "remote-example.txt");
            }
            assert_eq!(
                self.selected_commits.count(self.engine.app.selected_id()),
                if phase == 6 { 2 } else { 1 }
            );
            assert_eq!(self.engine.app.head_id(), self.graph_features.head);
            drop(paints);
            match phase {
                2 => {
                    self.engine.mark("ghost single recovered frame");
                    let index = self.engine.app.history.index_of(&id(2)).unwrap();
                    self.action(Action::Select(index, false), window, cx);
                }
                3 => {
                    self.graph_features.ghost_api.as_ref().unwrap().unblock();
                    let index = self.engine.app.history.index_of(&id(4)).unwrap();
                    self.action(Action::Select(index, false), window, cx);
                }
                4 => {
                    let index = self.engine.app.history.index_of(&id(3)).unwrap();
                    self.action(Action::Select(index, false), window, cx);
                }
                5 => {
                    let index = self.engine.app.history.index_of(&id(2)).unwrap();
                    self.action(Action::Select(index, true), window, cx);
                }
                6 => {
                    assert!(
                        self.graph_features
                            .ghost_api
                            .as_ref()
                            .unwrap()
                            .requests
                            .load(Ordering::Acquire)
                            >= 6
                    );
                    self.graph_features.verified = true;
                    self.engine.mark("graph feature verified frame");
                    return;
                }
                _ => unreachable!(),
            }
        }
        self.graph_features.paints.borrow_mut().clear();
        self.graph_features.phase += 1;
    }

    fn graph_feature_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.graph_feature_name().unwrap().to_owned();
        if name == "detached-commit" {
            self.detached_commit_tick(window, cx);
            return;
        }
        if name.starts_with("ghost-files-") {
            self.ghost_files_tick(window, cx);
            return;
        }
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
                "graph-selection-boundary" => {
                    let app = &self.engine.app;
                    let target = app
                        .refs
                        .as_ref()
                        .unwrap()
                        .refs
                        .iter()
                        .find(|r| r.name == "selected-topic")
                        .unwrap()
                        .id;
                    let index = app.history.index_of(&target).unwrap();
                    self.action(Action::Select(index, false), window, cx);
                    self.graph_scroll.scroll_to_item(0, ScrollStrategy::Top);
                }
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
        if name == "graph-selection-boundary" {
            let app = &self.engine.app;
            assert!(app.history.complete);
            let selected = app.selected_id().unwrap();
            let mut expected_ids = std::collections::HashSet::new();
            let mut cursor = Some(selected);
            while let Some(id) = cursor {
                assert!(expected_ids.insert(id));
                cursor = app
                    .history
                    .index_of(&id)
                    .and_then(|i| app.history.parents(i).first().copied());
            }
            let mut highlighted = 0;
            let paints = self.graph_features.paints.borrow();
            for (index, record) in app.history.commits.iter().enumerate() {
                assert!(!app.pending.exists());
                let row = index;
                let Some(bounds) = paints.get(&(row, 0)) else {
                    return;
                };
                assert!(bounds.size.width > px(0.) && bounds.size.height > px(0.));
                assert!(bounds.left() >= px(0.) && bounds.top() >= px(0.));
                assert!(
                    bounds.right() <= window.viewport_size().width
                        && bounds.bottom() <= window.viewport_size().height
                );
                let expected = expected_ids.contains(&record.id);
                let actual = self
                    .graph_selected_path
                    .node_on_path(row, record.node_lane as usize);
                assert_eq!(
                    actual,
                    expected,
                    "Unrelated SHA highlighted: {}",
                    app.history.subject(index)
                );
                highlighted += usize::from(actual);
            }
            assert_eq!(
                highlighted,
                if self.graph_features.phase == 2 { 4 } else { 2 }
            );
            assert_eq!(self.selected_commits.count(app.selected_id()), 1);
            assert_eq!(app.head_id(), self.graph_features.head);
            drop(paints);
            if self.graph_features.phase < 3 {
                let name = if self.graph_features.phase == 1 {
                    "ended-topic"
                } else {
                    "selected-topic"
                };
                let target = app
                    .refs
                    .as_ref()
                    .unwrap()
                    .refs
                    .iter()
                    .find(|r| r.name == name)
                    .unwrap()
                    .id;
                let index = app.history.index_of(&target).unwrap();
                self.action(Action::Select(index, false), window, cx);
                self.graph_features.paints.borrow_mut().clear();
                self.graph_features.phase += 1;
                return;
            }
            self.graph_features.verified = true;
            self.engine.mark("graph feature verified frame");
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

    fn detached_commit_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let app = &self.engine.app;
        if !app.history.complete || !app.pending.known || app.pending.busy {
            return;
        }
        if self.graph_features.phase == 0 {
            if !app.pending.selected || !app.details_changes.as_ref().is_some_and(|c| c.loaded) {
                return;
            }
            let root = self.engine.repository_root().unwrap();
            assert_eq!(
                std::fs::read_to_string(root.join(".git/git-rustle-graph-fixture"))
                    .unwrap()
                    .trim(),
                "disposable graph feature fixture"
            );
            assert!(app.refs.as_ref().unwrap().head_branch.is_none());
            self.graph_features.head = app.head_id();
            self.engine.app.pending.description.value =
                "Detached commit stays above its parent".into();
            self.engine.app.pending.branch.value.clear();
            self.engine.mark("graph feature requested");
            self.action(Action::Commit, window, cx);
            assert!(self.engine.app.pending.request.is_some() || self.engine.app.pending.busy);
            self.graph_features.phase = 1;
            return;
        }
        assert!(!app.pending.error, "{}", app.pending.status);
        let Some(head) = app.head_id() else { return };
        if Some(head) == self.graph_features.head || app.pending.request.is_some() {
            return;
        }
        let Some(row) = app.history.index_of(&head) else {
            return;
        };
        let parent_row = app
            .history
            .index_of(&self.graph_features.head.unwrap())
            .unwrap();
        assert!(row < parent_row, "detached commit must precede its parent");
        assert_eq!(row, 0, "newest detached commit belongs at the top");
        assert!(app.refs.as_ref().unwrap().head_branch.is_none());
        if self.graph_features.phase == 1 {
            self.action(Action::Select(row, false), window, cx);
            self.graph_scroll.scroll_to_item(0, ScrollStrategy::Top);
            self.graph_features.paints.borrow_mut().clear();
            self.graph_features.phase = 2;
            return;
        }
        let paints = self.graph_features.paints.borrow();
        let (Some(sha), Some(subject)) = (paints.get(&(row, 0)), paints.get(&(row, 1))) else {
            return;
        };
        assert!(sha.size.width > px(0.) && subject.size.width > px(0.));
        assert!(sha.right() <= subject.left() + px(1.));
        assert!(subject.right() <= window.viewport_size().width);
        self.graph_features.verified = true;
        self.engine.mark("graph feature verified frame");
    }
}
