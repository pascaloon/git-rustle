//! Flat and slash-folder branch views; projections are cached by Browser updates.
use super::*;
type Paints = std::rc::Rc<std::cell::RefCell<[Option<(u64, Bounds<Pixels>)>; 7]>>;
#[derive(Default)]
pub(super) struct State {
    bench: Option<Box<Bench>>,
}
struct Bench {
    name: &'static str,
    phase: u8,
    epoch: u64,
    scheduled: bool,
    verified: bool,
    started: Instant,
    head: Option<gix::ObjectId>,
    paints: Paints,
}
impl Desktop {
    pub(super) fn branch_views_content(&self, cx: &mut Context<Self>) -> AnyElement {
        let browser = &self.engine.app.branches;
        let list = uniform_list(
            "branches",
            browser.display_count(),
            cx.processor(|v, range: std::ops::Range<usize>, _, cx| {
                v.viewport_rows[3] = v.viewport_rows[3].max(range.len());
                v.engine.app.branches.rows = range.len().max(1);
                v.engine.app.branches.scroll = range.start;
                let b = &v.engine.app.branches;
                range
                    .map(|i| {
                        let (i, display_name, depth) = if b.hierarchy {
                            match &b.tree_rows[i] {
                                crate::branches::TreeRow::Folder {
                                    path,
                                    name,
                                    depth,
                                    count,
                                } => {
                                    let path = path.clone();
                                    let toggle = path.clone();
                                    return row()
                                        .relative()
                                        .children(
                                            (path == "fixes")
                                                .then(|| v.branch_cleanup_probe(1))
                                                .flatten(),
                                        )
                                        .id(SharedString::from(format!("branch-folder-{path}")))
                                        .w_full()
                                        .h(px(40.))
                                        .pl(px(16. + *depth as f32 * 18.))
                                        .pr_4()
                                        .gap_2()
                                        .bg(rgb(PANEL))
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgb(HOVER)))
                                        .child(icon(
                                            if b.collapsed.contains(&path) && b.query.is_empty() {
                                                "right"
                                            } else {
                                                "down"
                                            },
                                        ))
                                        .child(icon("folder").text_color(rgb(ACCENT)))
                                        .child(name.clone())
                                        .child(caption(count.to_string()))
                                        .on_click(cx.listener(move |v, _, _, cx| {
                                            v.branch_folder_action(toggle.clone(), cx);
                                        }))
                                        .into_any_element();
                                }
                                crate::branches::TreeRow::Branch {
                                    visible,
                                    name,
                                    depth,
                                } => (*visible, name.clone(), *depth),
                            }
                        } else {
                            (i, b.entries[b.visible[i]].target.label(), 0)
                        };
                        let entry = &b.entries[b.visible[i]];
                        row()
                            .relative()
                            .children(
                                (entry.target.name == "fixes/task-123")
                                    .then(|| v.branch_cleanup_probe(2))
                                    .flatten(),
                            )
                            .id(("branch", i))
                            .tooltip({
                                let name = std::iter::once(entry.target.label())
                                    .chain(entry.paired_remotes.iter().map(|r| r.label()))
                                    .collect::<Vec<_>>()
                                    .join(" · ");
                                move |w, cx| {
                                    gpui_component::tooltip::Tooltip::new(name.clone()).build(w, cx)
                                }
                            })
                            .w_full()
                            .h(px(40.))
                            .px_4()
                            .pl(px(16. + depth as f32 * 18.))
                            .bg(rgb(if b.selected == i { SELECTED } else { BG }))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(HOVER)))
                            .child(
                                icon(if entry.current {
                                    "head"
                                } else if entry.target.remote.is_none() {
                                    "branch"
                                } else {
                                    "cloud"
                                })
                                .text_color(rgb(
                                    if entry.current {
                                        ACCENT
                                    } else if entry.target.remote.is_none() {
                                        MUTED
                                    } else {
                                        REMOTE_TEXT
                                    },
                                )),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .child(v.branch_matches.styled(display_name)),
                            )
                            .when(!entry.paired_remotes.is_empty(), |r| {
                                r.child(icon("cloud").text_color(rgb(REMOTE_TEXT))).child(
                                    div()
                                        .max_w(px(200.))
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_color(rgb(REMOTE_TEXT))
                                        .child(entry.paired_remotes[0].label()),
                                )
                            })
                            .children({
                                let remote = entry.paired_remotes.first().or_else(|| {
                                    entry.target.remote.as_ref().map(|_| &entry.target)
                                });
                                remote.and_then(|target| {
                                    gix::ObjectId::from_hex(target.sha.as_bytes())
                                        .ok()
                                        .and_then(|id| {
                                            v.provider_badge(
                                                id,
                                                crate::git::RefKind::Remote,
                                                &target.label(),
                                                cx,
                                            )
                                        })
                                })
                            })
                            .child(
                                div()
                                    .w(px(150.))
                                    .flex_shrink_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_color(rgb(MUTED))
                                    .child(
                                        v.branch_matches.styled(
                                            entry
                                                .activity
                                                .as_ref()
                                                .map(|a| a.author.clone())
                                                .unwrap_or_default(),
                                        ),
                                    ),
                            )
                            .child(
                                div()
                                    .w(px(190.))
                                    .flex_shrink_0()
                                    .text_color(rgb(MUTED))
                                    .text_size(px(12.))
                                    .child(
                                        entry
                                            .activity
                                            .as_ref()
                                            .map(|a| {
                                                format!(
                                                    "{}{} commits this week",
                                                    a.recent,
                                                    if a.limited { "+" } else { "" }
                                                )
                                            })
                                            .unwrap_or_else(|| "Loading activity…".into()),
                                    ),
                            )
                            .child(
                                div().w(px(90.)).flex_shrink_0().text_right().child(caption(
                                    entry
                                        .activity
                                        .as_ref()
                                        .map(|a| {
                                            crate::ui::time::format_relative(
                                                a.commit_time,
                                                crate::ui::time::now_unix(),
                                            )
                                        })
                                        .unwrap_or_default(),
                                )),
                            )
                            .on_click(cx.listener(move |v, e: &ClickEvent, window, cx| {
                                v.action(Action::Branch(i), window, cx);
                                if e.click_count() == 2 {
                                    v.action(Action::JumpBranch, window, cx);
                                }
                            }))
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |v, e: &MouseDownEvent, w, cx| {
                                    v.action(Action::Branch(i), w, cx);
                                    v.context_menu = Some(e.position);
                                    cx.stop_propagation();
                                    cx.notify();
                                }),
                            )
                            .into_any_element()
                    })
                    .collect()
            }),
        )
        .track_scroll(self.branches_scroll.clone())
        .size_full();
        column()
            .relative()
            .children(self.branch_cleanup_probe(0))
            .flex_1()
            .child(
                row()
                    .h(px(48.))
                    .flex_shrink_0()
                    .px_3()
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(caption(format!("{} branches", browser.matching_count)))
                    .child(
                        row()
                            .gap_0()
                            .border_1()
                            .border_color(rgb(LINE))
                            .rounded_md()
                            .child(
                                Button::new("branch-view-list")
                                    .small()
                                    .ghost()
                                    .label("List")
                                    .selected(!browser.hierarchy)
                                    .on_click(cx.listener(|v, _, _, cx| {
                                        v.branch_view_mode(false, cx);
                                    })),
                            )
                            .child(
                                Button::new("branch-view-hierarchy")
                                    .small()
                                    .ghost()
                                    .label("Folders")
                                    .selected(browser.hierarchy)
                                    .on_click(cx.listener(|v, _, _, cx| {
                                        v.branch_view_mode(true, cx);
                                    })),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(180.))
                            .max_w(px(420.))
                            .child(toolbar_filter(
                                &self.branch_filter,
                                !browser.query.is_empty(),
                                self.filter_probe(1),
                                cx,
                            )),
                    ),
            )
            .child(
                row()
                    .relative()
                    .children(self.todo_probe(6))
                    .h(px(34.))
                    .flex_shrink_0()
                    .px_4()
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(div().w(px(18.)).flex_shrink_0())
                    .child(div().flex_1().child(self.branch_sort_button(
                        "sort-branch-name",
                        "Name",
                        crate::branches::SortColumn::Name,
                        cx,
                    )))
                    .child(div().w(px(150.)).flex_shrink_0().child(caption("Author")))
                    .child(
                        div()
                            .w(px(190.))
                            .flex_shrink_0()
                            .child(self.branch_sort_button(
                                "sort-branch-activity",
                                "Activity",
                                crate::branches::SortColumn::Activity,
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .w(px(90.))
                            .flex_shrink_0()
                            .child(self.branch_sort_button(
                                "sort-branch-commit",
                                "Last commit",
                                crate::branches::SortColumn::LastCommit,
                                cx,
                            )),
                    ),
            )
            .when(browser.loading, |s| {
                s.child(caption("Refreshing branches…"))
            })
            .when(!browser.status.is_empty(), |s| {
                s.child(caption(browser.status.clone()))
            })
            .children(browser.errors.iter().cloned().map(caption))
            .child(self.scroll_region(3, list, Scrollbar::vertical(&self.branches_scroll), cx))
            .into_any_element()
    }
}

impl Desktop {
    fn branch_view_mode(&mut self, hierarchy: bool, cx: &mut Context<Self>) {
        self.engine.app.branches.set_hierarchy(hierarchy);
        self.branches_scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }
    fn branch_folder_action(&mut self, path: String, cx: &mut Context<Self>) {
        self.engine.app.branches.toggle_folder(path);
        cx.notify();
    }
    pub(super) fn branch_cleanup_probe(&self, slot: usize) -> Option<AnyElement> {
        let run = self.branch_views.bench.as_ref()?;
        let paints = run.paints.clone();
        let epoch = run.epoch;
        Some(
            canvas(
                move |bounds, _, _| bounds,
                move |bounds, _, _, _| {
                    paints.borrow_mut()[slot] = Some((epoch, bounds));
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .into_any_element(),
        )
    }
    fn branch_cleanup_painted(&self, slot: usize, window: &Window) -> bool {
        let run = self.branch_views.bench.as_ref().unwrap();
        run.paints.borrow()[slot].is_some_and(|(epoch, b)| {
            epoch == run.epoch
                && b.size.width >= px(200.)
                && b.size.height >= px(24.)
                && b.left() >= px(0.)
                && b.right() <= window.viewport_size().width
                && b.top() >= px(0.)
                && b.bottom() <= window.viewport_size().height
        })
    }
    pub(super) fn branch_cleanup_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = match self.scenario.as_deref() {
            Some("branches-hierarchy") => "branches-hierarchy",
            Some("cleanup-review") => "cleanup-review",
            Some("cleanup-delete") => "cleanup-delete",
            _ => return,
        };
        let root = self.engine.repository_root().unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join(".git/git-rustle-cleanup-fixture")).unwrap(),
            "disposable branch cleanup fixture"
        );
        self.branch_views.bench = Some(Box::new(Bench {
            name,
            phase: 1,
            epoch: 1,
            scheduled: false,
            verified: false,
            started: Instant::now(),
            head: self.engine.app.head_id(),
            paints: std::rc::Rc::new(std::cell::RefCell::new([None; 7])),
        }));
        self.engine.mark("branch cleanup requested");
        self.action(
            Action::Tab(if name == "branches-hierarchy" {
                Tab::Branches
            } else {
                Tab::Cleanup
            }),
            window,
            cx,
        );
    }
    pub(super) fn branch_cleanup_schedule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(run) = self
            .branch_views
            .bench
            .as_mut()
            .filter(|r| !r.verified && !r.scheduled)
        else {
            return;
        };
        run.scheduled = true;
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = weak.update(cx, |v, cx| {
                if let Some(run) = &mut v.branch_views.bench {
                    run.scheduled = false;
                }
                v.branch_cleanup_tick(window, cx);
            });
        });
    }
    fn branch_cleanup_advance(&mut self, phase: u8) {
        let run = self.branch_views.bench.as_mut().unwrap();
        run.phase = phase;
        run.epoch += 1;
    }
    fn branch_cleanup_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(run) = self.branch_views.bench.as_ref().filter(|r| !r.verified) else {
            return;
        };
        assert!(
            run.started.elapsed() < Duration::from_secs(45),
            "{} stalled phase{} epoch{} paints={:?}",
            run.name,
            run.phase,
            run.epoch,
            run.paints
                .borrow()
                .iter()
                .map(|p| p.map(|(e, b)| (e, f32::from(b.size.width), f32::from(b.size.height))))
                .collect::<Vec<_>>()
        );
        let (name, phase) = (run.name, run.phase);
        if name == "branches-hierarchy" {
            if phase == 1
                && !self.engine.app.branches.loading
                && self
                    .engine
                    .app
                    .branches
                    .entries
                    .iter()
                    .any(|e| e.target.remote.is_none() && e.target.name == "fixes/task-123")
                && self.branch_cleanup_painted(0, window)
            {
                let browser = &mut self.engine.app.branches;
                let entry = browser
                    .entries
                    .iter()
                    .find(|e| e.target.remote.is_none() && e.target.name == "fixes/task-123")
                    .unwrap();
                assert_eq!(entry.paired_remotes.len(), 1);
                assert!(
                    !browser
                        .entries
                        .iter()
                        .any(|e| e.target.remote.as_deref() == Some("origin")
                            && e.target.name == "fixes/task-123")
                );
                browser.set_sort(crate::branches::SortColumn::Name);
                self.branch_view_mode(true, cx);
                self.branch_cleanup_advance(2);
            } else if phase == 2
                && self.branch_cleanup_painted(1, window)
                && self.branch_cleanup_painted(2, window)
            {
                assert!(self.engine.app.branches.hierarchy);
                self.branch_folder_action("fixes".into(), cx);
                self.branch_cleanup_advance(3);
            } else if phase == 3 && self.branch_cleanup_painted(0, window) {
                let browser = &self.engine.app.branches;
                assert!(
                    !browser
                        .visible
                        .iter()
                        .any(|&i| browser.entries[i].target.name == "fixes/task-123")
                );
                self.branch_folder_action("fixes".into(), cx);
                self.branch_cleanup_advance(4);
            } else if phase == 4
                && self.branch_cleanup_painted(1, window)
                && self.branch_cleanup_painted(2, window)
            {
                self.branch_cleanup_finish();
            }
        } else if phase == 1
            && self.cleanup_scenario_ready()
            && self.branch_cleanup_painted(3, window)
            && self.branch_cleanup_painted(4, window)
            && self.branch_cleanup_painted(5, window)
        {
            self.cleanup_scenario_assert_defaults();
            if name == "cleanup-review" {
                self.branch_cleanup_finish();
            } else {
                self.cleanup_action(cleanup::Action::Clear, window, cx);
                self.cleanup_action(cleanup::Action::Toggle("fixes/task-123".into()), window, cx);
                self.cleanup_action(cleanup::Action::Remote(true), window, cx);
                self.cleanup_action(cleanup::Action::Review, window, cx);
                assert!(
                    self.operations
                        .confirmation
                        .as_ref()
                        .unwrap()
                        .1
                        .contains("ALSO DELETE 1")
                );
                self.branch_cleanup_advance(2);
            }
        } else if phase == 2 && self.branch_cleanup_painted(6, window) {
            self.engine.mark("cleanup delete requested");
            self.operation_action(operations::Action::Confirm, window, cx);
            self.branch_cleanup_advance(3);
        } else if phase == 3
            && !self.engine.operation_busy
            && self.cleanup_scenario_ready()
            && self.cleanup_scenario_deleted()
            && self.branch_cleanup_painted(3, window)
            && self.branch_cleanup_painted(5, window)
        {
            assert!(
                !self.engine.operation_error,
                "{}",
                self.engine.operation_status
            );
            self.engine.mark("cleanup delete frame");
            self.branch_cleanup_finish();
        }
        cx.notify();
    }
    fn branch_cleanup_finish(&mut self) {
        let run = self.branch_views.bench.as_mut().unwrap();
        run.verified = true;
        self.engine.mark("branch cleanup verified frame");
    }
    pub(super) fn branch_cleanup_ready(&self) -> Option<bool> {
        self.branch_views.bench.as_ref().map(|r| r.verified)
    }
    pub(super) fn branch_cleanup_assert(&self) {
        if let Some(run) = &self.branch_views.bench {
            assert!(run.verified);
            assert_eq!(self.engine.app.head_id(), run.head);
        }
    }
    pub(super) fn branch_cleanup_summary(&self) -> String {
        self.branch_views
            .bench
            .as_ref()
            .map(|r| {
                format!(
                    "branch_cleanup_verified={} branch_cleanup_phase={}",
                    r.verified, r.phase
                )
            })
            .unwrap_or_default()
    }
}
