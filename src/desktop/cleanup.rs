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
    SelectMatching,
    Clear,
    Remote(bool),
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
    delete_remotes: bool,
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
                        v.cleanup.selected = matching_selection(&analysis);
                        v.cleanup.analysis = Some(analysis);
                        v.cleanup.delete_remotes = false;
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
                if !self.cleanup.selected.remove(&name) {
                    self.cleanup.selected.insert(name);
                }
            }
            Action::Clear if idle => self.cleanup.selected.clear(),
            Action::Remote(value) if idle => self.cleanup.delete_remotes = value,
            Action::SelectMatching if idle => {
                self.cleanup.selected = self
                    .cleanup
                    .analysis
                    .as_ref()
                    .map(matching_selection)
                    .unwrap_or_default();
            }
            Action::Review if idle && !self.cleanup.selected.is_empty() => {
                let branches: Vec<_> = self
                    .cleanup
                    .analysis
                    .iter()
                    .flat_map(|a| &a.candidates)
                    .filter(|c| self.cleanup.selected.contains(&c.name))
                    .cloned()
                    .collect();
                let remote_count = if self.cleanup.delete_remotes {
                    branches.iter().filter(|c| c.remote.is_some()).count()
                } else {
                    0
                };
                let names = branches
                    .iter()
                    .take(8)
                    .map(|c| format!("• {}", c.name.chars().take(100).collect::<String>()))
                    .collect::<Vec<_>>()
                    .join("\n");
                let text = format!(
                    "Remove {} local branch reference(s)?\n{}{}\n\n{}\nLocal commits are preserved in recovery checkpoints and can be restored in History. Matching a remote does not prove these commits are disposable.{}",
                    branches.len(),
                    names,
                    if branches.len() > 8 {
                        format!("\n… and {} more selected branches", branches.len() - 8)
                    } else {
                        String::new()
                    },
                    if remote_count > 0 {
                        format!(
                            "ALSO DELETE {remote_count} REMOTE BRANCH(ES). This changes the server. Expected commit IDs are rechecked and protected by leases."
                        )
                    } else {
                        "Server branches and cached origin/… tracking refs will be kept.".into()
                    },
                    if remote_count > 0 {
                        " Remote deletion cannot be undone by local History. Remote groups or command-sized batches may partially complete if a later batch fails."
                    } else {
                        ""
                    }
                );
                self.operations.confirmation = Some((
                    Request::Cleanup(Plan {
                        branches,
                        delete_remotes: self.cleanup.delete_remotes,
                    }),
                    text,
                ));
            }
            Action::Optimize if idle => {
                self.operations.confirmation = Some((Request::OptimizeStorage,
                    "Repack repository storage? Git will consolidate objects and remove redundant copies while retaining all unreachable objects, reflogs and recovery checkpoints. No pruning or reflog expiry is requested. Storage may stay the same or grow; this can take time.".into()));
            }
            _ => {}
        }
        cx.notify();
    }
    fn cleanup_section(&self, section: Section, cx: &mut Context<Self>) -> AnyElement {
        let busy =
            self.cleanup.loading || self.engine.operation_busy || self.engine.repository_edit_busy;
        let indices: Vec<_> = self
            .cleanup
            .analysis
            .iter()
            .flat_map(|a| a.candidates.iter().enumerate())
            .filter(|(_, c)| c.section == section)
            .map(|(i, _)| i)
            .collect();
        let count = indices.len();
        let scroll = if section == Section::Matching {
            &self.cleanup.matching_scroll
        } else {
            &self.cleanup.caution_scroll
        };
        let list = uniform_list(
            if section == Section::Matching {
                "cleanup-matching"
            } else {
                "cleanup-caution"
            },
            count,
            cx.processor(move |v, range: std::ops::Range<usize>, _, cx| {
                range
                    .map(|index| {
                        let entry =
                            &v.cleanup.analysis.as_ref().unwrap().candidates[indices[index]];
                        let name = entry.name.clone();
                        branch_views::branch_row(false)
                            .relative()
                            .children(if entry.name == "fixes/task-123" {
                                v.branch_cleanup_probe(4)
                            } else if entry.name == "old/local" {
                                v.branch_cleanup_probe(5)
                            } else {
                                None
                            })
                            .gap_3()
                            .child(
                                Checkbox::new(("cleanup-select", indices[index]))
                                    .checked(v.cleanup.selected.contains(&entry.name))
                                    .disabled(busy)
                                    .on_click(cx.listener(move |v, _, window, cx| {
                                        v.cleanup_action(Action::Toggle(name.clone()), window, cx)
                                    })),
                            )
                            .child(icon("branch").text_color(rgb(MUTED)))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(entry.name.clone()),
                            )
                            .when_some(entry.tracking.as_ref(), |r, (remote, branch)| {
                                r.child(icon("cloud").text_color(rgb(REMOTE_TEXT))).child(
                                    div()
                                        .max_w(px(200.))
                                        .truncate()
                                        .text_color(rgb(REMOTE_TEXT))
                                        .child(format!("{remote}/{branch}")),
                                )
                            })
                            .when(entry.protected, |r| {
                                r.child(caption("Protected by default"))
                            })
                            .child(
                                div()
                                    .w(px(150.))
                                    .flex_shrink_0()
                                    .truncate()
                                    .text_color(rgb(MUTED))
                                    .child(entry.author.clone()),
                            )
                            .child(div().w(px(90.)).flex_shrink_0().text_right().child(caption(
                                crate::ui::time::format_relative(
                                    entry.time,
                                    crate::ui::time::now_unix(),
                                ),
                            )))
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .size_full()
        .track_scroll(scroll.clone());
        column()
            .relative()
            .children(self.branch_cleanup_probe(if section == Section::Matching { 7 } else { 8 }))
            .flex_1()
            .min_h_0()
            .border_t_1()
            .border_color(rgb(LINE))
            .child(
                row()
                    .h(px(32.))
                    .flex_shrink_0()
                    .px_4()
                    .child(caption(format!(
                        "{} · {count}",
                        if section == Section::Matching {
                            "MATCHING CACHED REMOTE COPIES"
                        } else {
                            "CAUTION · LAST COMMIT AT LEAST 6 MONTHS AGO"
                        }
                    ))),
            )
            .child(self.scroll_region(
                if section == Section::Matching { 3 } else { 4 },
                list,
                Scrollbar::vertical(scroll),
                cx,
            ))
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
                .child(caption("origin/… is a cached reference on this computer. Matching it does not verify that the server branch still exists; fetch first."))
                .child(caption("Default cleanup removes local branches and keeps cached copies and server branches. Commits remain recoverable in History."))
                .when_some(analysis, |s, a| s.child(caption(format!("{} candidates · {} checked-out branches excluded · Git object storage {} KiB", a.candidates.len(), a.excluded_worktrees, a.storage_kib))))
                .children(analysis.into_iter().flat_map(|a| &a.warnings).cloned().map(caption)))
            .when(self.cleanup.loading, |s| s.child(row().px_4().py_3().child(gpui_component::spinner::Spinner::new().small()).child("Analyzing branches and storage…")))
            .when(!self.cleanup.message.is_empty(), |s| s.child(caption(self.cleanup.message.clone()).px_4().py_2()))
            .child(row().px_4().py_2().gap_2()
                .child(Button::new("cleanup-select-matching").small().ghost().label("Select matching copies").disabled(busy)
                    .on_click(cx.listener(|v, _, w, cx| v.cleanup_action(Action::SelectMatching, w, cx))))
                .child(Button::new("cleanup-clear").small().ghost().label("Clear").disabled(busy)
                    .on_click(cx.listener(|v, _, w, cx| v.cleanup_action(Action::Clear, w, cx))))
                .child(caption(format!("{} selected", self.cleanup.selected.len()))))
            .child(self.cleanup_section(Section::Matching, cx))
            .child(self.cleanup_section(Section::OldLocal, cx))
            .child(row().relative().children(self.branch_cleanup_probe(9)).flex_shrink_0().px_4().py_3().gap_2().border_t_1().border_color(rgb(LINE))
                .child(div().flex_1())
                .child(Checkbox::new("cleanup-remote").checked(self.cleanup.delete_remotes).disabled(busy)
                    .on_click(cx.listener(|v, checked, w, cx| v.cleanup_action(Action::Remote(*checked), w, cx))))
                .child(caption("Also delete actual server branches (cannot be undone here)"))
                .child(Button::new("cleanup-review").small().primary().label("Review selected…").disabled(busy || self.cleanup.selected.is_empty())
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
        let count = self
            .cleanup
            .analysis
            .as_ref()
            .unwrap()
            .candidates
            .iter()
            .filter(|c| c.section == section)
            .count();
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
        assert!(self.cleanup.selected.contains("fixes/task-123"));
        assert!(!self.cleanup.selected.contains("master"));
        assert!(!self.cleanup.selected.contains("old/local"));
        assert!(!self.cleanup.delete_remotes);
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

fn matching_selection(analysis: &Analysis) -> HashSet<String> {
    analysis
        .candidates
        .iter()
        .filter(|c| c.section == Section::Matching && !c.protected)
        .map(|c| c.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Analysis, Section, matching_selection};
    #[test]
    fn matching_selection_checks_every_eligible_branch_without_a_batch_limit() {
        let mut analysis = Analysis::default();
        for index in 0..1500 {
            analysis
                .candidates
                .push(crate::git::operations::cleanup::Candidate {
                    name: format!("candidate/{index}"),
                    oid: "a".repeat(40),
                    author: "Owner".into(),
                    time: 1,
                    section: if index == 1499 {
                        Section::OldLocal
                    } else {
                        Section::Matching
                    },
                    protected: index == 1498,
                    remote: None,
                    upstream: None,
                    tracking: None,
                });
        }
        let selected = matching_selection(&analysis);
        assert_eq!(selected.len(), 1498);
        assert!(selected.contains("candidate/1497"));
        assert!(!selected.contains("candidate/1498"));
        assert!(!selected.contains("candidate/1499"));
    }
}
