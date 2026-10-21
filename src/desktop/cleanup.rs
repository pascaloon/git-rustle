//! Asynchronous analysis and explicit review for branch/storage cleanup.
use super::*;
use crate::git::operations::{
    Request,
    cleanup::{Analysis, MAX_BATCH, Plan, Section},
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
    scroll: UniformListScrollHandle,
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
                        v.cleanup.selected = analysis
                            .candidates
                            .iter()
                            .filter(|c| c.section == Section::Matching && !c.protected)
                            .take(MAX_BATCH)
                            .map(|c| c.name.clone())
                            .collect();
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
                if !self.cleanup.selected.remove(&name) && self.cleanup.selected.len() < MAX_BATCH {
                    self.cleanup.selected.insert(name);
                }
            }
            Action::Clear if idle => self.cleanup.selected.clear(),
            Action::Remote(value) if idle => self.cleanup.delete_remotes = value,
            Action::SelectMatching if idle => {
                self.cleanup.selected = self
                    .cleanup
                    .analysis
                    .iter()
                    .flat_map(|a| &a.candidates)
                    .filter(|c| c.section == Section::Matching && !c.protected)
                    .take(MAX_BATCH)
                    .map(|c| c.name.clone())
                    .collect();
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
                        "Remote branches will be kept.".into()
                    },
                    if remote_count > 0 {
                        " Remote deletion cannot be undone by local History. Separate remote groups may partially complete if another group fails."
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
    pub(super) fn cleanup_content(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy =
            self.cleanup.loading || self.engine.operation_busy || self.engine.repository_edit_busy;
        let analysis = self.cleanup.analysis.as_ref();
        let count = analysis.map_or(0, |a| a.candidates.len());
        let list = uniform_list(
            "cleanup-candidates",
            count,
            cx.processor(move |v, range: std::ops::Range<usize>, _, cx| {
                range
                    .map(|index| {
                        let candidates = &v.cleanup.analysis.as_ref().unwrap().candidates;
                        let entry = &candidates[index];
                        let name = entry.name.clone();
                        let section_start =
                            index == 0 || candidates[index - 1].section != entry.section;
                        let date = crate::ui::time::format_absolute(entry.time, 0)
                            .chars()
                            .take(10)
                            .collect::<String>();
                        column()
                            .relative()
                            .children(if entry.name == "fixes/task-123" {
                                v.branch_cleanup_probe(4)
                            } else if entry.name == "old/local" {
                                v.branch_cleanup_probe(5)
                            } else {
                                None
                            })
                            .w_full()
                            .h(px(106.))
                            .px_4()
                            .py_2()
                            .gap_2()
                            .border_b_1()
                            .border_color(rgb(LINE))
                            .child(
                                caption(if section_start {
                                    if entry.section == Section::Matching {
                                        "MATCHING REMOTE COPIES · LOCAL REFS"
                                    } else {
                                        "CAUTION · LAST COMMIT AT LEAST 6 MONTHS AGO"
                                    }
                                } else {
                                    ""
                                })
                                .h(px(16.)),
                            )
                            .child(
                                row()
                                    .gap_3()
                                    .child(
                                        Checkbox::new(("cleanup-select", index))
                                            .checked(v.cleanup.selected.contains(&entry.name))
                                            .disabled(busy)
                                            .on_click(cx.listener(move |v, _, window, cx| {
                                                v.cleanup_action(
                                                    Action::Toggle(name.clone()),
                                                    window,
                                                    cx,
                                                )
                                            })),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .child(entry.name.clone()),
                                    )
                                    .when(entry.protected, |r| {
                                        r.child(caption("Protected by default"))
                                    })
                                    .child(caption(date)),
                            )
                            .child(
                                row()
                                    .pl(px(28.))
                                    .gap_3()
                                    .child(caption(entry.author.clone()))
                                    .child(caption(
                                        entry
                                            .tracking
                                            .as_ref()
                                            .map(|(remote, branch)| {
                                                format!(
                                                    "Matches {remote}/{branch}{}",
                                                    if entry.remote.is_none() {
                                                        " · remote deletion unavailable"
                                                    } else {
                                                        ""
                                                    }
                                                )
                                            })
                                            .unwrap_or_else(|| {
                                                "No matching cached remote copy".into()
                                            }),
                                    )),
                            )
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .size_full()
        .track_scroll(self.cleanup.scroll.clone());
        column().relative().children(self.branch_cleanup_probe(3)).flex_1().min_h_0()
            .child(row().px_4().py_3().gap_3().child(icon("broom")).child("Cleanup").child(div().flex_1())
                .child(Button::new("cleanup-analyze").small().label("Analyze again").disabled(busy)
                    .on_click(cx.listener(|v, _, w, cx| v.cleanup_action(Action::Analyze, w, cx))))
                .child(Button::new("cleanup-optimize").small().ghost().label("Optimize storage…").disabled(busy)
                    .on_click(cx.listener(|v, _, w, cx| v.cleanup_action(Action::Optimize, w, cx)))))
            .child(column().px_4().pb_3().gap_2()
                .child(caption("Analysis uses local branch and remote-tracking refs. Fetch first for fresher remote information."))
                .child(caption("Matching a remote is not proof commits are disposable. Removing references does not immediately free commit storage; recovery keeps those commits."))
                .when_some(analysis, |s, a| s.child(caption(format!("{} candidates · {} checked-out branches excluded · Git object storage {} KiB", a.candidates.len(), a.excluded_worktrees, a.storage_kib))))
                .children(analysis.into_iter().flat_map(|a| &a.warnings).cloned().map(caption)))
            .when(self.cleanup.loading, |s| s.child(row().px_4().py_3().child(gpui_component::spinner::Spinner::new().small()).child("Analyzing branches and storage…")))
            .when(!self.cleanup.message.is_empty(), |s| s.child(caption(self.cleanup.message.clone()).px_4().py_2()))
            .child(row().px_4().py_2().gap_2()
                .child(Button::new("cleanup-select-matching").small().ghost().label("Select matching copies").disabled(busy)
                    .on_click(cx.listener(|v, _, w, cx| v.cleanup_action(Action::SelectMatching, w, cx))))
                .child(Button::new("cleanup-clear").small().ghost().label("Clear").disabled(busy)
                    .on_click(cx.listener(|v, _, w, cx| v.cleanup_action(Action::Clear, w, cx))))
                .child(caption(format!("{} selected (up to {MAX_BATCH})", self.cleanup.selected.len())))
                .child(div().flex_1())
                .child(Button::new("cleanup-review").small().label("Review selected…").disabled(busy || self.cleanup.selected.is_empty())
                    .on_click(cx.listener(|v, _, w, cx| v.cleanup_action(Action::Review, w, cx)))))
            .child(row().px_4().pb_3().gap_2()
                .child(Checkbox::new("cleanup-remote").checked(self.cleanup.delete_remotes).disabled(busy)
                    .on_click(cx.listener(|v, checked, w, cx| v.cleanup_action(Action::Remote(*checked), w, cx))))
                .child(caption("Also delete matching remote branches (requires explicit review; cannot be undone here)")))
            .when(count == 0 && !self.cleanup.loading, |s| s.child(caption("No cleanup candidates found in the current local refs.").p_4()))
            .child(self.scroll_region(3, list, Scrollbar::vertical(&self.cleanup.scroll), cx)).into_any_element()
    }
}

impl Desktop {
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
