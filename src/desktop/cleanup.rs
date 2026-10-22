//! Asynchronous analysis and explicit review for branch/storage cleanup.
use super::*;
use crate::git::operations::{
    Request,
    cleanup::{Analysis, Plan, Section},
};
use std::collections::HashSet;

#[derive(Clone)]
pub(super) enum Action {
    Open,
    Analyze,
    Toggle(String),
    SelectSection(Section, bool),
    Folder(Section, String),
    Collapse(Section, String),
    Clear,
    Review,
    Optimize,
}
#[derive(Default)]
pub(super) struct State {
    pub open: bool,
    analysis: Option<Analysis>,
    loading: bool,
    generation: u64,
    selected: HashSet<String>,
    collapsed: HashSet<String>,
    counts: std::collections::HashMap<String, (usize, usize)>,
    rows: [Vec<TreeRow>; 3],
    fetched_scroll: UniformListScrollHandle,
    message: String,
    waiting_epoch: Option<u64>,
    matching_scroll: UniformListScrollHandle,
    caution_scroll: UniformListScrollHandle,
}
impl Desktop {
    pub(super) fn cleanup_mutation_started(&mut self) {
        self.cleanup.waiting_epoch = Some(self.engine.operation_epoch);
    }
    pub(super) fn sync_cleanup(&mut self, cx: &mut Context<Self>) {
        if self
            .cleanup
            .waiting_epoch
            .is_some_and(|epoch| epoch != self.engine.operation_epoch)
            && !self.engine.operation_busy
        {
            self.cleanup.waiting_epoch = None;
            self.cleanup.message = self.engine.operation_status.clone();
            self.load_cleanup(cx);
        }
    }
    fn load_cleanup(&mut self, cx: &mut Context<Self>) {
        if self.cleanup.loading {
            return;
        }
        let Some(path) = self
            .engine
            .repository_root()
            .map(std::path::Path::to_path_buf)
        else {
            return;
        };
        self.cleanup.loading = true;
        self.cleanup.generation = self.cleanup.generation.wrapping_add(1);
        let generation = self.cleanup.generation;
        let session = self.engine.messages();
        cx.spawn(async move |view, cx| {
            let source = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::git::operations::cleanup::analyze(&path, crate::ui::time::now_unix())
                })
                .await;
            let _ = view.update(cx, |v, cx| {
                if !session.same_channel(&v.engine.messages())
                    || v.engine.repository_root() != Some(source.as_path())
                    || v.cleanup.generation != generation
                {
                    return;
                }
                v.cleanup.loading = false;
                match result {
                    Ok(analysis) => {
                        v.cleanup.selected = default_selection(&analysis);
                        v.cleanup.analysis = Some(analysis);
                        v.cleanup.rebuild_rows();
                    }
                    Err(error) => v.cleanup.message = error,
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn cleanup_action(
        &mut self,
        action: Action,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let idle = !self.cleanup.loading
            && !self.engine.operation_busy
            && !self.engine.repository_edit_busy
            && !self.engine.app.pending.busy
            && !self.engine.app.branches.busy
            && !self.engine.app.settings.saving
            && self.engine.operations.conflict.is_none();
        match action {
            Action::Open => {
                self.cleanup.open = true;
                if self.cleanup.analysis.is_none() {
                    self.load_cleanup(cx);
                }
            }
            Action::Analyze if idle => {
                self.cleanup.message.clear();
                self.load_cleanup(cx);
            }
            Action::Toggle(name) if idle => {
                if self.cleanup.analysis.as_ref().is_some_and(|a| {
                    a.candidates
                        .iter()
                        .any(|c| candidate_key(c) == name && !c.protected)
                }) && !self.cleanup.selected.remove(&name)
                {
                    self.cleanup.selected.insert(name);
                }
            }
            Action::Clear if idle => self.cleanup.selected.clear(),
            Action::SelectSection(section, selected) if idle => {
                self.cleanup.select_group(section, "", selected)
            }
            Action::Folder(section, path) if idle => {
                let (selected, eligible) = self.cleanup.group_counts(section, &path);
                self.cleanup
                    .select_group(section, &path, selected != eligible);
            }
            Action::Collapse(section, path) if idle => {
                let key = format!("{}:{path}", section_index(section));
                if !self.cleanup.collapsed.remove(&key) {
                    self.cleanup.collapsed.insert(key);
                }
                self.cleanup.rebuild_rows();
            }
            Action::Review if idle && !self.cleanup.selected.is_empty() => {
                let branches: Vec<_> = self
                    .cleanup
                    .analysis
                    .iter()
                    .flat_map(|a| &a.candidates)
                    .filter(|c| self.cleanup.selected.contains(&candidate_key(c)) && !c.protected)
                    .cloned()
                    .collect();
                let names = branches
                    .iter()
                    .flat_map(local_refs)
                    .map(|name| format!("• {name}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                let text = format!(
                    "Remove these {} local references?\n\nThis only changes references on this computer. No server branches will be deleted. Fetched references are local copies. Cleaning matching copies removes the local branch and any fetched reference shown below.\n\nCommits remain recoverable through Undo and History. Old local branches may contain unique work; review them carefully.\n\nExact references to remove:\n{names}",
                    branches.iter().flat_map(local_refs).count()
                );
                self.operations.confirmation = Some((Request::Cleanup(Plan { branches }), text));
            }
            Action::Optimize if idle => {
                self.operations.confirmation = Some((Request::OptimizeStorage,
                    "Repack repository storage? Git will consolidate objects and remove redundant copies while retaining all unreachable objects, reflogs and recovery checkpoints. No pruning or reflog expiry is requested. Storage may stay the same or grow; this can take time.".into()));
            }
            _ => {}
        }
        self.cleanup.rebuild_counts();
        cx.notify();
    }
    fn cleanup_section(&self, section: Section, cx: &mut Context<Self>) -> AnyElement {
        let busy =
            self.cleanup.loading || self.engine.operation_busy || self.engine.repository_edit_busy;
        let slot = section_index(section);
        let scroll = match section {
            Section::FetchedOnly => &self.cleanup.fetched_scroll,
            Section::Matching => &self.cleanup.matching_scroll,
            Section::OldLocal => &self.cleanup.caution_scroll,
        };
        let count = self.cleanup.rows[slot].len();
        let (selected, eligible) = self.cleanup.group_counts(section, "");
        let (title, detail, color) = match section {
            Section::FetchedOnly => (
                "Fetched only",
                "Fetched references without a local branch",
                0x38975c,
            ),
            Section::Matching => (
                "Matching copies",
                "Local branches whose latest commit matches the server",
                0x90a83d,
            ),
            Section::OldLocal => (
                "Old local branches",
                "Last commit at least 1 year ago · may contain unique work",
                0xcf4949,
            ),
        };
        let list = uniform_list(
            SharedString::from(format!("cleanup-section-{slot}")),
            count,
            cx.processor(move |v, range: std::ops::Range<usize>, _, cx| {
                range
                    .map(|index| {
                        let tree = v.cleanup.rows[slot][index].clone();
                        match tree {
                            TreeRow::Folder { path, label, depth } => {
                                let (checked, total) = v.cleanup.group_counts(section, &path);
                                let toggle = path.clone();
                                let select = path.clone();
                                let collapsed =
                                    v.cleanup.collapsed.contains(&format!("{slot}:{path}"));
                                branch_views::branch_row(false)
                                    .pl(px(16. + depth as f32 * 18.))
                                    .gap_2()
                                    .child(
                                        Button::new(SharedString::from(format!(
                                            "cleanup-expand-{slot}-{path}"
                                        )))
                                        .small()
                                        .ghost()
                                        .icon(gpui_component::Icon::default().path(if collapsed {
                                            "rustle/next"
                                        } else {
                                            "rustle/down"
                                        }))
                                        .on_click(
                                            cx.listener(move |v, _, w, cx| {
                                                v.cleanup_action(
                                                    Action::Collapse(section, toggle.clone()),
                                                    w,
                                                    cx,
                                                )
                                            }),
                                        ),
                                    )
                                    .child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "cleanup-folder-hit-{slot}-{path}"
                                            )))
                                            .relative()
                                            .size(px(16.))
                                            .on_click(cx.listener(move |v, _, w, cx| {
                                                v.cleanup_action(
                                                    Action::Folder(section, select.clone()),
                                                    w,
                                                    cx,
                                                )
                                            }))
                                            .child(
                                                Checkbox::new(SharedString::from(format!(
                                                    "cleanup-folder-{slot}-{path}"
                                                )))
                                                .checked(checked > 0)
                                                .disabled(busy || total == 0),
                                            )
                                            .when(checked > 0 && checked < total, |s| {
                                                s.child(
                                                    div()
                                                        .absolute()
                                                        .inset_0()
                                                        .rounded_sm()
                                                        .bg(rgb(ACCENT))
                                                        .text_color(rgb(BG))
                                                        .text_center()
                                                        .child("−"),
                                                )
                                            }),
                                    )
                                    .child(icon("folder").text_color(rgb(MUTED)))
                                    .child(div().flex_1().min_w_0().truncate().child(label))
                                    .child(caption(format!("{checked}/{total}")).flex_shrink_0())
                                    .into_any_element()
                            }
                            TreeRow::Branch {
                                candidate,
                                label,
                                depth,
                            } => {
                                let entry =
                                    &v.cleanup.analysis.as_ref().unwrap().candidates[candidate];
                                let key = candidate_key(entry);
                                branch_views::branch_row(false)
                                    .relative()
                                    .gap_2()
                                    .pl(px(48. + depth as f32 * 18.))
                                    .children(if entry.name == "fixes/task-123" {
                                        v.branch_cleanup_probe(4)
                                    } else if entry.name == "old/local" {
                                        v.branch_cleanup_probe(5)
                                    } else {
                                        None
                                    })
                                    .child(
                                        Checkbox::new(("cleanup-select", candidate))
                                            .checked(v.cleanup.selected.contains(&key))
                                            .disabled(busy || entry.protected)
                                            .on_click(cx.listener(move |v, _, w, cx| {
                                                v.cleanup_action(Action::Toggle(key.clone()), w, cx)
                                            })),
                                    )
                                    .child(
                                        icon(if section == Section::FetchedOnly {
                                            "computer"
                                        } else {
                                            "branch"
                                        })
                                        .text_color(rgb(MUTED)),
                                    )
                                    .child(div().flex_1().min_w_0().truncate().child(label))
                                    .when(entry.protected, |r| r.child(caption("Protected")))
                                    .child(
                                        div()
                                            .w(px(125.))
                                            .flex_shrink_0()
                                            .truncate()
                                            .text_color(rgb(MUTED))
                                            .child(entry.author.clone()),
                                    )
                                    .child(div().w(px(90.)).flex_shrink_0().text_right().child(
                                        caption(crate::ui::time::format_relative(
                                            entry.time,
                                            crate::ui::time::now_unix(),
                                        )),
                                    ))
                                    .into_any_element()
                            }
                        }
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .size_full()
        .track_scroll(scroll.clone());
        column()
            .relative()
            .children(self.branch_cleanup_probe(if section == Section::Matching {
                7
            } else if section == Section::OldLocal {
                8
            } else {
                0
            }))
            .flex_1()
            .min_h_0()
            .border_t_1()
            .border_color(rgb(LINE))
            .child(
                row()
                    .h(px(34.))
                    .flex_shrink_0()
                    .px_4()
                    .gap_2()
                    .child(
                        icon(if section == Section::OldLocal {
                            "shield-alert"
                        } else {
                            "shield-check"
                        })
                        .text_color(rgb(color)),
                    )
                    .child(title)
                    .child(caption(format!("{selected}/{eligible} selected")))
                    .child(div().flex_1())
                    .child(
                        Button::new(SharedString::from(format!("cleanup-select-all-{slot}")))
                            .small()
                            .ghost()
                            .label("Select all")
                            .disabled(busy)
                            .on_click(cx.listener(move |v, _, w, cx| {
                                v.cleanup_action(Action::SelectSection(section, true), w, cx)
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("cleanup-unselect-all-{slot}")))
                            .small()
                            .ghost()
                            .label("Unselect all")
                            .disabled(busy)
                            .on_click(cx.listener(move |v, _, w, cx| {
                                v.cleanup_action(Action::SelectSection(section, false), w, cx)
                            })),
                    ),
            )
            .child(caption(detail).px_4().pb_1())
            .child(self.scroll_region(3 + slot, list, Scrollbar::vertical(scroll), cx))
            .into_any_element()
    }
    pub(super) fn cleanup_content(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy =
            self.cleanup.loading || self.engine.operation_busy || self.engine.repository_edit_busy;
        let analysis = self.cleanup.analysis.as_ref();
        column().relative().children(self.branch_cleanup_probe(3)).flex_1().min_h_0()
            .child(row().px_4().py_3().gap_3().child(icon("broom")).child("Cleanup").child(div().flex_1())
                .child(Button::new("cleanup-analyze").small().ghost().icon(gpui_component::Icon::default().path("rustle/refresh")).tooltip("Refresh cleanup analysis").disabled(busy)
                    .on_click(cx.listener(|v, _, w, cx| v.cleanup_action(Action::Analyze, w, cx))))
                .child(Button::new("cleanup-optimize").small().ghost().label("Optimize storage…").disabled(busy)
                    .on_click(cx.listener(|v, _, w, cx| v.cleanup_action(Action::Optimize, w, cx)))))
            .child(column().px_4().pb_3().gap_2()
                .child(caption("Fetched references live on this computer. Matching copies are verified against the server during analysis."))
                .child(caption("Only fetched-only references are selected initially. Everything here is local to this computer; server branches stay intact."))
                .when_some(analysis, |s, a| s.child(caption(format!("{} candidates · {} checked-out branches excluded · Git object storage {} KiB", a.candidates.len(), a.excluded_worktrees, a.storage_kib))))
                .when(analysis.is_some_and(|a| !a.warnings.is_empty()), |s| s
                    .child(caption(format!("{} analysis warning(s)", analysis.unwrap().warnings.len())))
                    .child(column().id("cleanup-warnings").max_h(px(96.)).overflow_y_scroll()
                        .children(analysis.into_iter().flat_map(|a| &a.warnings).cloned().map(caption)))))
            .when(self.cleanup.loading, |s| s.child(row().px_4().py_3().child(gpui_component::spinner::Spinner::new().small()).child("Analyzing branches and storage…")))
            .when(!self.cleanup.message.is_empty(), |s| s.child(caption(self.cleanup.message.clone()).px_4().py_2()))
            .child(self.cleanup_section(Section::FetchedOnly, cx))
            .child(self.cleanup_section(Section::Matching, cx))
            .child(self.cleanup_section(Section::OldLocal, cx))
            .child(row().relative().children(self.branch_cleanup_probe(9)).flex_shrink_0().px_4().py_3().gap_2().border_t_1().border_color(rgb(LINE))
                .child(div().flex_1())
                .child(Button::new("cleanup-review").small().primary().label(format!("Review selected ({})…", self.cleanup.selected.len())).disabled(busy || self.cleanup.selected.is_empty())
                    .on_click(cx.listener(|v, _, w, cx| v.cleanup_action(Action::Review, w, cx)))))
            .into_any_element()
    }
}

impl Desktop {
    pub(super) fn cleanup_scenario_scroll(&self, caution: bool, reset: bool) {
        let section = if caution {
            Section::OldLocal
        } else {
            Section::Matching
        };
        let count = self.cleanup.rows[section_index(section)].len();
        let scroll = if caution {
            &self.cleanup.caution_scroll
        } else {
            &self.cleanup.matching_scroll
        };
        scroll.scroll_to_item(
            if reset { 0 } else { count.saturating_sub(1) },
            ScrollStrategy::Top,
        );
    }
    pub(super) fn cleanup_scenario_scroll_fetched(&self, reset: bool) {
        self.cleanup.fetched_scroll.scroll_to_item(
            if reset {
                0
            } else {
                self.cleanup.rows[0].len().saturating_sub(1)
            },
            ScrollStrategy::Top,
        );
    }
    pub(super) fn cleanup_scenario_assert_fetched_scroll(&self) {
        assert!(
            self.cleanup
                .fetched_scroll
                .0
                .borrow()
                .base_handle
                .offset()
                .y
                < px(0.)
        );
        assert_eq!(
            self.cleanup
                .matching_scroll
                .0
                .borrow()
                .base_handle
                .offset()
                .y,
            px(0.)
        );
        assert_eq!(
            self.cleanup
                .caution_scroll
                .0
                .borrow()
                .base_handle
                .offset()
                .y,
            px(0.)
        );
    }
    pub(super) fn cleanup_scenario_assert_scroll(&self, caution: bool) {
        let matching = self
            .cleanup
            .matching_scroll
            .0
            .borrow()
            .base_handle
            .offset()
            .y;
        let old = self
            .cleanup
            .caution_scroll
            .0
            .borrow()
            .base_handle
            .offset()
            .y;
        assert!(matching < px(0.), "matching list must actually scroll");
        assert!(
            self.cleanup
                .fetched_scroll
                .0
                .borrow()
                .base_handle
                .offset()
                .y
                < px(0.),
            "fetched-only list must independently scroll"
        );
        if caution {
            assert!(old < px(0.), "caution list must independently scroll");
        } else {
            assert_eq!(
                old,
                px(0.),
                "scrolling matching must preserve caution offset"
            );
        }
    }
    pub(super) fn cleanup_scenario_exercise_selection(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for section in [Section::FetchedOnly, Section::Matching, Section::OldLocal] {
            let total = self.cleanup.group_counts(section, "").1;
            assert!(total > 0);
            self.cleanup_action(Action::SelectSection(section, true), window, cx);
            assert_eq!(self.cleanup.group_counts(section, ""), (total, total));
            self.cleanup_action(Action::SelectSection(section, false), window, cx);
            assert_eq!(self.cleanup.group_counts(section, ""), (0, total));
            let folder = self.cleanup.rows[section_index(section)]
                .iter()
                .find_map(|r| match r {
                    TreeRow::Folder { path, .. }
                        if self.cleanup.group_counts(section, path).1 > 1 =>
                    {
                        Some(path.clone())
                    }
                    _ => None,
                })
                .unwrap();
            self.cleanup_action(Action::Folder(section, folder.clone()), window, cx);
            let group = self.cleanup.group_counts(section, &folder);
            assert_eq!(group.0, group.1);
            let key = self
                .cleanup
                .analysis
                .as_ref()
                .unwrap()
                .candidates
                .iter()
                .find(|c| {
                    c.section == section
                        && !c.protected
                        && c.name.starts_with(&format!("{folder}/"))
                })
                .map(candidate_key)
                .unwrap();
            self.cleanup_action(Action::Toggle(key), window, cx);
            let mixed = self.cleanup.group_counts(section, &folder);
            assert!(mixed.0 > 0 && mixed.0 < mixed.1);
            let count = self.cleanup.rows[section_index(section)].len();
            self.cleanup_action(Action::Collapse(section, folder.clone()), window, cx);
            assert!(self.cleanup.rows[section_index(section)].len() < count);
            self.cleanup_action(Action::Collapse(section, folder), window, cx);
            assert_eq!(self.cleanup.rows[section_index(section)].len(), count);
            self.cleanup_action(Action::SelectSection(section, false), window, cx);
        }
        self.cleanup_action(
            Action::SelectSection(Section::FetchedOnly, true),
            window,
            cx,
        );
    }
    pub(super) fn cleanup_scenario_ready(&self) -> bool {
        self.cleanup.analysis.is_some()
            && !self.cleanup.loading
            && self.cleanup.waiting_epoch.is_none()
    }
    pub(super) fn cleanup_scenario_deleted(&self) -> bool {
        self.cleanup
            .analysis
            .as_ref()
            .is_some_and(|a| !a.candidates.iter().any(|c| c.name == "fixes/task-123"))
    }
    pub(super) fn cleanup_scenario_assert_defaults(&self) {
        let analysis = self.cleanup.analysis.as_ref().unwrap();
        assert!(analysis.candidates.iter().any(|c| c.name == "old/local"
            && c.section == Section::OldLocal
            && !c.author.is_empty()));
        assert!(
            analysis
                .candidates
                .iter()
                .any(|c| c.name == "master" && c.protected)
        );
        let stale_fetched = analysis
            .candidates
            .iter()
            .find(|candidate| candidate.name == "origin/zz-fetched/000")
            .expect("server-ahead fetched reference must remain eligible");
        assert_eq!(stale_fetched.section, Section::FetchedOnly);
        assert_eq!(
            stale_fetched.tracking_oid.as_deref(),
            Some(stale_fetched.oid.as_str())
        );
        assert_ne!(
            stale_fetched.oid,
            self.engine.app.head_id().unwrap().to_string(),
            "fixture cached tip must differ from the current server/HEAD tip"
        );
        assert!(
            self.cleanup
                .selected
                .contains(&candidate_key(stale_fetched)),
            "stale fetched reference must be preselected"
        );
        assert!(
            !analysis
                .candidates
                .iter()
                .any(|candidate| candidate.name == "origin/zz-stale/missing")
        );
        assert!(
            analysis
                .warnings
                .iter()
                .any(|warning| warning.contains("server branches no longer exist")),
            "missing server reference must have a retention warning"
        );
        assert!(!self.cleanup.selected.contains("refs/heads/fixes/task-123"));
        assert!(!self.cleanup.selected.contains("refs/heads/master"));
        assert!(!self.cleanup.selected.contains("refs/heads/old/local"));
        assert!(
            analysis
                .candidates
                .iter()
                .filter(|c| self.cleanup.selected.contains(&candidate_key(c)))
                .all(|c| c.section == Section::FetchedOnly && !c.protected)
        );
        assert!(
            !self.cleanup.selected.is_empty(),
            "fixture must exercise fetched-only defaults"
        );
        let root = self.engine.repository_root().unwrap();
        let expected = root
            .parent()
            .unwrap()
            .join("origin.git")
            .canonicalize()
            .unwrap();
        let target = analysis
            .candidates
            .iter()
            .find(|c| c.name == "fixes/task-123")
            .unwrap()
            .remote
            .as_ref()
            .unwrap();
        assert_eq!(
            std::path::Path::new(&target.url).canonicalize().unwrap(),
            expected
        );
    }
}

fn default_selection(analysis: &Analysis) -> HashSet<String> {
    analysis
        .candidates
        .iter()
        .filter(|c| c.section == Section::FetchedOnly && !c.protected)
        .map(candidate_key)
        .collect()
}

fn section_index(section: Section) -> usize {
    match section {
        Section::FetchedOnly => 0,
        Section::Matching => 1,
        Section::OldLocal => 2,
    }
}
fn candidate_key(c: &crate::git::operations::cleanup::Candidate) -> String {
    if c.section == Section::FetchedOnly {
        format!("refs/remotes/{}", c.name)
    } else {
        format!("refs/heads/{}", c.name)
    }
}
fn local_refs(c: &crate::git::operations::cleanup::Candidate) -> Vec<String> {
    let mut refs = vec![candidate_key(c)];
    if c.section == Section::Matching
        && c.tracking_oid.is_some()
        && let Some((remote, branch)) = &c.tracking
    {
        refs.push(format!("refs/remotes/{remote}/{branch}"));
    }
    refs
}
#[derive(Clone)]
enum TreeRow {
    Folder {
        path: String,
        label: String,
        depth: usize,
    },
    Branch {
        candidate: usize,
        label: String,
        depth: usize,
    },
}
impl State {
    fn group_counts(&self, section: Section, path: &str) -> (usize, usize) {
        self.counts
            .get(&format!("{}:{path}", section_index(section)))
            .copied()
            .unwrap_or_default()
    }
    fn rebuild_counts(&mut self) {
        self.counts.clear();
        for c in self
            .analysis
            .iter()
            .flat_map(|a| &a.candidates)
            .filter(|c| !c.protected)
        {
            let slot = section_index(c.section);
            let selected = usize::from(self.selected.contains(&candidate_key(c)));
            let parts: Vec<_> = c.name.split('/').collect();
            for depth in 0..parts.len() {
                let path = parts[..depth].join("/");
                let count = self.counts.entry(format!("{slot}:{path}")).or_default();
                count.0 += selected;
                count.1 += 1;
            }
        }
    }
    fn select_group(&mut self, section: Section, path: &str, selected: bool) {
        let prefix = format!("{path}/");
        for c in self
            .analysis
            .iter()
            .flat_map(|a| &a.candidates)
            .filter(|c| {
                c.section == section
                    && !c.protected
                    && (path.is_empty() || c.name.starts_with(&prefix))
            })
        {
            if selected {
                self.selected.insert(candidate_key(c));
            } else {
                self.selected.remove(&candidate_key(c));
            }
        }
    }
    fn rebuild_rows(&mut self) {
        self.rebuild_counts();
        for section in [Section::FetchedOnly, Section::Matching, Section::OldLocal] {
            let slot = section_index(section);
            let mut entries: Vec<_> = self
                .analysis
                .iter()
                .flat_map(|a| a.candidates.iter().enumerate())
                .filter(|(_, c)| c.section == section)
                .collect();
            entries.sort_by(|a, b| a.1.name.cmp(&b.1.name));
            let mut rows = Vec::new();
            let mut seen = HashSet::new();
            for (candidate, c) in entries {
                let parts: Vec<_> = c.name.split('/').collect();
                let mut hidden = false;
                for depth in 0..parts.len().saturating_sub(1) {
                    let path = parts[..=depth].join("/");
                    if seen.insert(path.clone()) {
                        rows.push(TreeRow::Folder {
                            path: path.clone(),
                            label: parts[depth].into(),
                            depth,
                        });
                    }
                    if self.collapsed.contains(&format!("{slot}:{path}")) {
                        hidden = true;
                        break;
                    }
                }
                if !hidden {
                    rows.push(TreeRow::Branch {
                        candidate,
                        label: parts.last().unwrap_or(&"").to_string(),
                        depth: parts.len().saturating_sub(1),
                    });
                }
            }
            self.rows[slot] = rows;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Analysis, Section, State, candidate_key, default_selection, local_refs};
    use crate::git::operations::cleanup::Candidate;
    fn candidate(name: &str, section: Section) -> Candidate {
        Candidate {
            name: name.into(),
            oid: "a".repeat(40),
            author: "Owner".into(),
            time: 1,
            section,
            remote: None,
            protected: false,
            upstream: None,
            tracking: None,
            tracking_oid: None,
        }
    }
    #[test]
    fn full_ref_keys_keep_local_and_fetched_names_separate() {
        assert_ne!(
            candidate_key(&candidate("origin/topic", Section::FetchedOnly)),
            candidate_key(&candidate("origin/topic", Section::OldLocal))
        );
    }
    #[test]
    fn tree_selection_has_no_batch_limit_and_preserves_protected_refs() {
        let mut analysis = Analysis::default();
        for i in 0..1500 {
            analysis.candidates.push(candidate(
                &format!("origin/folder/{i:04}"),
                Section::FetchedOnly,
            ));
        }
        let mut protected = candidate("origin/main", Section::FetchedOnly);
        protected.protected = true;
        analysis.candidates.push(protected);
        analysis
            .candidates
            .push(candidate("origin/folder/0000", Section::OldLocal));
        let mut state = State {
            selected: default_selection(&analysis),
            analysis: Some(analysis),
            ..State::default()
        };
        state.rebuild_rows();
        assert_eq!(state.selected.len(), 1500);
        assert!(!state.selected.contains("refs/remotes/origin/main"));
        assert!(!state.selected.contains("refs/heads/origin/folder/0000"));
        assert_eq!(
            state.group_counts(Section::FetchedOnly, "origin/folder"),
            (1500, 1500)
        );
        state.selected.remove("refs/remotes/origin/folder/0000");
        state.rebuild_counts();
        assert_eq!(
            state.group_counts(Section::FetchedOnly, "origin/folder"),
            (1499, 1500)
        );
        state.select_group(Section::FetchedOnly, "origin", false);
        state.rebuild_counts();
        assert_eq!(state.group_counts(Section::FetchedOnly, ""), (0, 1500));
        state.select_group(Section::FetchedOnly, "", true);
        state.rebuild_counts();
        assert_eq!(state.selected.len(), 1500);
        state.collapsed.insert("0:origin/folder".into());
        state.rebuild_rows();
        assert_eq!(state.rows[0].len(), 3);
        let refs: Vec<_> = state
            .analysis
            .as_ref()
            .unwrap()
            .candidates
            .iter()
            .filter(|c| state.selected.contains(&candidate_key(c)))
            .flat_map(local_refs)
            .collect();
        assert_eq!(refs.len(), 1500);
        assert!(refs.contains(&"refs/remotes/origin/folder/1499".into()));
    }
    #[test]
    fn review_lists_only_present_fetched_refs() {
        let mut c = candidate("topic", Section::Matching);
        c.tracking = Some(("origin".into(), "topic".into()));
        assert_eq!(local_refs(&c), ["refs/heads/topic"]);
        c.tracking_oid = Some("b".repeat(40));
        assert_eq!(
            local_refs(&c),
            ["refs/heads/topic", "refs/remotes/origin/topic"]
        );
    }
}
