//! Filename/content search UI; workers own indexing and text scanning.
use super::*;
use crate::diff::search::{Hit, Worker};
use std::sync::Arc;
type ScenarioPaints =
    std::rc::Rc<std::cell::RefCell<std::collections::HashMap<u8, Bounds<Pixels>>>>;

#[derive(Default)]
pub(super) struct State {
    scenario_phase: u8,
    scenario_scheduled: bool,
    scenario_verified: bool,
    scenario_paints: Option<ScenarioPaints>,
    input: Option<Entity<InputState>>,
    subscription: Option<Subscription>,
    worker: Option<Worker>,
    debounce: Option<Task<()>>,
    notifications: Option<Task<()>>,
    view: Option<u32>,
    files: Option<Arc<Vec<diff::FileChange>>>,
    query: String,
    changed: Option<Instant>,
    sequence: u64,
    hits: Vec<Hit>,
    busy: bool,
    skipped: usize,
    limited: bool,
    selected: Option<Hit>,
    jump: bool,
    selection_visible: bool,
    scroll: UniformListScrollHandle,
}
impl Desktop {
    pub(super) fn prepare_diff_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.diff_find.input.is_none() {
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder("Search filenames and contents…")
            });
            self.diff_find.subscription = Some(cx.subscribe_in(
                &input,
                window,
                |v, input, event, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        v.diff_find.query = input.read(cx).value().to_string();
                        if let Some(worker) = &v.diff_find.worker {
                            worker.cancel();
                        }
                        v.diff_find.changed = Some(Instant::now());
                        v.diff_find.hits.clear();
                        v.diff_find.selected = None;
                        v.diff_find.busy = !v.diff_find.query.trim().is_empty();
                        v.diff_find.skipped = 0;
                        v.diff_find.limited = false;
                        v.diff_find.debounce = Some(cx.spawn_in(window, async move |view, cx| {
                            Timer::after(Duration::from_millis(65)).await;
                            let _ = view.update_in(cx, |v, window, cx| {
                                v.sync_diff_search(window, cx);
                                cx.notify();
                            });
                        }));
                        cx.notify();
                    }
                },
            ));
            self.diff_find.input = Some(input);
        }
        self.sync_diff_search(window, cx);
    }
    pub(super) fn sync_diff_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut requested = false;
        let mut completed = false;
        let Mode::Diff(view) = &self.engine.app.mode else {
            return;
        };
        if self.diff_find.view != Some(view.id) {
            if let Some(worker) = &self.diff_find.worker {
                worker.cancel();
            }
            self.diff_find.view = Some(view.id);
            self.diff_find.files = None;
            self.diff_find.query.clear();
            self.diff_find.hits.clear();
            self.diff_find.selected = None;
            self.diff_find.busy = false;
            self.diff_find.changed = None;
            self.diff_find.skipped = 0;
            self.diff_find.limited = false;
            if let Some(input) = self.diff_find.input.clone() {
                input.update(cx, |s, cx| s.set_value("", window, cx));
            }
        }
        if !view.changes.loaded {
            return;
        }
        if self
            .diff_find
            .files
            .as_ref()
            .is_none_or(|old| !Arc::ptr_eq(old, &view.changes.files))
        {
            if let Some(worker) = &self.diff_find.worker {
                worker.cancel();
            }
            self.diff_find.hits.clear();
            self.diff_find.selected = None;
            self.diff_find.busy = !self.diff_find.query.trim().is_empty();
            self.diff_find.files = Some(view.changes.files.clone());
            // A refreshed file list is already debounced by its producer.
            self.diff_find.changed = Some(Instant::now() - Duration::from_millis(65));
        }
        if !self.diff_find.query.trim().is_empty()
            && self
                .diff_find
                .changed
                .is_some_and(|t| t.elapsed() >= Duration::from_millis(65))
            && let Some(root) = self.engine.repository_root()
        {
            let worker = self.diff_find.worker.get_or_insert_with(|| {
                let (tx, rx) = async_channel::bounded(1);
                self.diff_find.notifications = Some(cx.spawn_in(window, async move |view, cx| {
                    while rx.recv().await.is_ok() {
                        if view
                            .update_in(cx, |v, window, cx| {
                                v.sync_diff_search(window, cx);
                                cx.notify();
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }));
                Worker::with_notify(tx)
            });
            self.diff_find.sequence = worker.search(
                root.to_path_buf(),
                view.changes.files.clone(),
                self.diff_find.query.clone(),
            );
            self.diff_find.changed = None;
            self.diff_find.busy = true;
            requested = true;
        }
        if let Some(worker) = &self.diff_find.worker {
            for result in worker.rx.try_iter() {
                if result.sequence == self.diff_find.sequence
                    && self.diff_find.changed.is_none()
                    && !self.diff_find.query.trim().is_empty()
                {
                    self.diff_find.hits = result.hits;
                    self.diff_find.busy = !result.done;
                    self.diff_find.skipped = result.skipped;
                    self.diff_find.limited = result.limited;
                    completed |= result.done;
                    cx.notify();
                }
            }
        }
        if self.diff_find.jump
            && let Some(hit) = &self.diff_find.selected
            && view.diff_for == Some(hit.file)
            && let Some(diff) = &view.diff
        {
            if let Some(line) = hit.line {
                let target = diff.rows.iter().position(|row| match row {
                    diff::Row::Line { left, right } => if hit.right { right } else { left }
                        .as_ref()
                        .is_some_and(|l| l.no == line),
                    _ => false,
                });
                self.diff_find.selection_visible = target.is_some();
                if let Some(target) = target {
                    self.diff_scroll.scroll_to_item(
                        if self.word_wrap {
                            self.wrap_layout.visual(target)
                        } else {
                            target
                        },
                        ScrollStrategy::Center,
                    );
                }
            }
            self.diff_find.jump = false;
            cx.notify();
        }
        if requested {
            self.engine.mark("diff search requested");
        }
        if completed {
            self.engine.mark("diff search ready");
        }
    }
    pub(super) fn diff_search_active(&self) -> bool {
        !self.diff_find.query.trim().is_empty()
    }
    pub(super) fn focus_diff_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prepare_diff_search(window, cx);
        if let Some(input) = &self.diff_find.input {
            input.update(cx, |s, cx| s.focus(window, cx));
        }
    }
    pub(super) fn clear_diff_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = &self.diff_find.input {
            input.update(cx, |s, cx| s.set_value("", window, cx));
        }
    }
    pub(super) fn diff_search_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(input) = &self.diff_find.input else {
            return div().into_any_element();
        };
        let status = if self.diff_find.busy {
            "Searching…".to_owned()
        } else if self.diff_search_active() {
            format!(
                "{}{} matches",
                self.diff_find.hits.len(),
                if self.diff_find.limited { "+" } else { "" }
            )
        } else {
            "".into()
        };
        row().w_full().flex_shrink_0().p_2()
            .child(div().flex_1().child(toolbar_filter(input, self.diff_search_active(), None, cx)))
            .child(caption(status))
            .when(self.diff_find.skipped > 0, |s| s.child(caption(format!("{} files: names only", self.diff_find.skipped)).id("diff-search-skipped")
                .tooltip(|w,cx| gpui_component::tooltip::Tooltip::new("Binary, unavailable or oversized contents are omitted. The content index is limited to 64 MiB.").build(w,cx))))
            .into_any_element()
    }
    pub(super) fn diff_search_results(&self, cx: &mut Context<Self>) -> AnyElement {
        let list = uniform_list(
            "diff-search-results",
            self.diff_find.hits.len(),
            cx.processor(|v, range: std::ops::Range<usize>, _, cx| {
                let Mode::Diff(view) = &v.engine.app.mode else {
                    return Vec::new();
                };
                range
                    .map(|i| {
                        let hit = &v.diff_find.hits[i];
                        let path = view
                            .changes
                            .files
                            .get(hit.file)
                            .map(|f| f.path.clone())
                            .unwrap_or_default();
                        let label = hit.line.map_or_else(
                            || "Filename".to_owned(),
                            |line| format!("{}:{line}", if hit.right { "After" } else { "Before" }),
                        );
                        column()
                            .id(("diff-search-result", i))
                            .w_full()
                            .h(px(66.))
                            .px_2()
                            .py_1()
                            .overflow_hidden()
                            .bg(rgb(if v.diff_find.selected.as_ref() == Some(hit) {
                                SELECTED
                            } else {
                                BG
                            }))
                            .hover(|s| s.bg(rgb(HOVER)))
                            .cursor_pointer()
                            .child(div().truncate().child(path))
                            .child(caption(format!("{label} · {}", hit.preview)).truncate())
                            .on_click(cx.listener(move |v, _, _, cx| {
                                v.select_diff_search_hit(i, cx);
                            }))
                            .into_any_element()
                    })
                    .collect()
            }),
        )
        .h_full()
        .track_scroll(self.diff_find.scroll.clone());
        column()
            .relative()
            .children(self.ui_feature_probe(1))
            .w(px(self.diff_sidebar))
            .h_full()
            .flex_shrink_0()
            .when(self.diff_find.hits.is_empty(), |s| {
                s.child(
                    caption(if self.diff_find.busy {
                        "Searching filenames and both file versions…"
                    } else {
                        "No matches"
                    })
                    .p_3(),
                )
            })
            .child(
                div().relative().flex_1().min_h_0().child(list).child(
                    Scrollbar::vertical(&self.diff_find.scroll)
                        .scrollbar_show(ScrollbarShow::Scrolling),
                ),
            )
            .into_any_element()
    }
    fn select_diff_search_hit(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(hit) = self.diff_find.hits.get(index).cloned() else {
            return;
        };
        if let Mode::Diff(view) = &mut self.engine.app.mode {
            view.show(Some(hit.file));
        }
        self.diff_find.selected = Some(hit);
        self.diff_find.jump = true;
        self.diff_find.selection_visible = false;
        self.changed(cx);
    }
    pub(super) fn diff_search_selection(&self) -> Option<AnyElement> {
        let hit = self.diff_find.selected.as_ref()?;
        let line = hit.line?;
        let visible = self.diff_find.selection_visible;
        Some(
            column()
                .relative()
                .children(self.ui_feature_probe(2))
                .w_full()
                .p_2()
                .flex_shrink_0()
                .bg(rgb(PANEL))
                .child(caption(format!(
                    "{} line {line}{}",
                    if hit.right { "After" } else { "Before" },
                    if visible {
                        ""
                    } else {
                        " · unchanged context outside the displayed hunks"
                    }
                )))
                .child(
                    div()
                        .relative()
                        .w_full()
                        .min_w_0()
                        .children(self.ui_feature_probe(4))
                        .child(
                            div()
                                .w_full()
                                .font_family("Cascadia Mono")
                                .text_size(px(12.))
                                .child(hit.preview.clone()),
                        ),
                )
                .into_any_element(),
        )
    }
}

// Native unattended scenarios. These dispatch GPUI events inside the isolated app;
// they never synthesize operating-system input or interact with another window.
impl Desktop {
    fn ui_feature_name(&self) -> Option<&str> {
        self.scenario
            .as_deref()
            .filter(|name| matches!(*name, "diff-search" | "filter-escape" | "project-settings"))
    }
    pub(super) fn ui_feature_ready(&self) -> Option<bool> {
        self.ui_feature_name()
            .map(|_| self.diff_find.scenario_verified)
    }
    pub(super) fn ui_feature_probe(&self, slot: u8) -> Option<AnyElement> {
        self.ui_feature_name()?;
        let paints = self.diff_find.scenario_paints.clone()?;
        Some(
            canvas(
                |bounds, _, _| bounds,
                move |bounds, _, _, _| {
                    paints.borrow_mut().insert(slot, bounds);
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .into_any_element(),
        )
    }
    pub(super) fn ui_feature_schedule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ui_feature_name().is_none()
            || !self.scenario_started
            || self.diff_find.scenario_scheduled
            || self.diff_find.scenario_verified
        {
            return;
        }
        // Do not drive artificial frames while queries run: the real debounce
        // timer and worker notifications must wake the app, as during idle typing.
        if self.diff_find.busy
            && ((self.ui_feature_name() == Some("diff-search")
                && matches!(self.diff_find.scenario_phase, 2 | 3))
                || (self.ui_feature_name() == Some("filter-escape")
                    && self.diff_find.scenario_phase == 6))
        {
            return;
        }
        self.diff_find
            .scenario_paints
            .get_or_insert_with(Default::default);
        self.diff_find.scenario_scheduled = true;
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = weak.update(cx, |v, cx| {
                v.diff_find.scenario_scheduled = false;
                v.ui_feature_tick(window, cx);
                cx.notify();
            });
        });
    }
    fn ui_feature_advance(&mut self, phase: u8) {
        self.diff_find.scenario_phase = phase;
        self.diff_find
            .scenario_paints
            .as_ref()
            .unwrap()
            .borrow_mut()
            .clear();
    }
    fn ui_feature_finish(&mut self) {
        self.diff_find.scenario_verified = true;
        self.engine.mark("ui feature verified frame");
    }
    fn ui_feature_escape(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.select_focus.focus(window);
        window.defer(cx, |window, cx| {
            assert!(window.dispatch_keystroke(Keystroke::parse("escape").unwrap(), cx));
        });
    }
    fn ui_feature_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.prepare_diff_search(window, cx);
        self.diff_find
            .input
            .as_ref()
            .unwrap()
            .update(cx, |s, cx| s.set_value(query.to_owned(), window, cx));
    }
    fn ui_feature_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.ui_feature_name().unwrap().to_owned();
        let phase = self.diff_find.scenario_phase;
        if phase == 0 {
            if !self.engine.app.history.complete
                || self.engine.app.history.len() < 2
                || !self.engine.app.pending.known
            {
                return;
            }
            let root = self.engine.repository_root().unwrap();
            assert_eq!(
                std::fs::read_to_string(root.join(".git/git-rustle-ui-fixture"))
                    .unwrap()
                    .trim(),
                "disposable UI feature fixture"
            );
            assert_eq!(self.title_project, self.engine.app.repo_name);
            self.engine.mark("ui feature requested");
            match name.as_str() {
                "project-settings" => {
                    self.action(Action::Settings, window, cx);
                    self.action(
                        Action::SettingsTab(crate::settings::GENERAL_PAGE),
                        window,
                        cx,
                    );
                }
                "filter-escape" => {
                    self.search
                        .update(cx, |s, cx| s.set_value("m: needle", window, cx));
                }
                _ => {
                    self.action(Action::Head, window, cx);
                    self.action(Action::Diff, window, cx);
                }
            }
            self.ui_feature_advance(1);
            return;
        }
        if name == "project-settings" {
            if self.engine.app.settings.busy {
                return;
            }
            if self.engine.app.settings.page != crate::settings::GENERAL_PAGE {
                self.action(
                    Action::SettingsTab(crate::settings::GENERAL_PAGE),
                    window,
                    cx,
                );
                self.ui_feature_advance(2);
                return;
            }
            let paints = self.diff_find.scenario_paints.as_ref().unwrap().borrow();
            let Some(links) = paints.get(&3) else {
                return;
            };
            assert!(links.size.width > px(200.) && links.size.height >= px(20.));
            assert!(
                links.right() <= window.viewport_size().width
                    && links.bottom() <= window.viewport_size().height
            );
            drop(paints);
            self.ui_feature_finish();
            return;
        }
        if name == "filter-escape" {
            match phase {
                1 if self
                    .engine
                    .app
                    .search
                    .result
                    .as_ref()
                    .is_some_and(|r| r.done) =>
                {
                    assert!(self.engine.app.search.active());
                    // A modal takes precedence: the filter must remain untouched.
                    self.updates.open = true;
                    assert!(!self.clear_active_filter(window, cx));
                    self.updates.open = false;
                    self.ui_feature_escape(window, cx);
                    self.ui_feature_advance(2);
                }
                2 if !self.engine.app.search.active()
                    && self.search.read(cx).value().is_empty() =>
                {
                    self.action(Action::Branches, window, cx);
                    self.branch_filter
                        .update(cx, |s, cx| s.set_value("feature/", window, cx));
                    self.ui_feature_advance(3);
                }
                3 if self.engine.app.branches.query == "feature/"
                    && !self.engine.app.branches.loading =>
                {
                    assert!(!self.engine.app.branches.visible.is_empty());
                    self.ui_feature_escape(window, cx);
                    self.ui_feature_advance(4);
                }
                4 if self.engine.app.branches.query.is_empty()
                    && self.branch_filter.read(cx).value().is_empty() =>
                {
                    self.action(Action::Tab(Tab::Graph), window, cx);
                    self.action(Action::Head, window, cx);
                    self.action(Action::Diff, window, cx);
                    self.ui_feature_advance(5);
                }
                5 if matches!(&self.engine.app.mode,Mode::Diff(v) if v.changes.loaded && v.diff.is_some()) =>
                {
                    self.ui_feature_query("needle", window, cx);
                    self.ui_feature_advance(6);
                }
                6 if !self.diff_find.busy && !self.diff_find.hits.is_empty() => {
                    self.ref_storage.confirming = true;
                    self.ui_feature_escape(window, cx);
                    self.ui_feature_advance(61);
                }
                61 if !self.ref_storage.confirming => {
                    assert!(self.diff_search_active());
                    assert!(matches!(self.engine.app.mode, Mode::Diff(_)));
                    self.ui_feature_escape(window, cx);
                    self.ui_feature_advance(7);
                }
                7 if !self.diff_search_active() => {
                    assert!(
                        matches!(self.engine.app.mode, Mode::Diff(_)),
                        "first Escape clears search while keeping diff open"
                    );
                    self.ui_feature_escape(window, cx);
                    self.ui_feature_advance(8);
                }
                8 if matches!(self.engine.app.mode, Mode::Graph) => self.ui_feature_finish(),
                _ => {}
            }
            return;
        }
        match phase {
            1 if matches!(&self.engine.app.mode,Mode::Diff(v) if v.changes.loaded && v.diff.is_some()) =>
            {
                self.engine.mark("diff filename requested");
                self.ui_feature_query("needle-file", window, cx);
                self.ui_feature_advance(2);
            }
            2 if !self.diff_find.busy && !self.diff_find.hits.is_empty() => {
                assert!(self.diff_find.hits.iter().any(|h| h.line.is_none()));
                self.engine.mark("diff filename completed");
                self.ui_feature_query("stale-no-match", window, cx);
                self.ui_feature_query("MAGIC_NEEDLE", window, cx);
                self.engine.mark("diff cached content requested");
                self.ui_feature_advance(3);
            }
            3 if !self.diff_find.busy
                && self.diff_find.query == "MAGIC_NEEDLE"
                && !self.diff_find.hits.is_empty() =>
            {
                let index = self
                    .diff_find
                    .hits
                    .iter()
                    .position(|h| h.line.is_some() && h.right)
                    .unwrap();
                assert!(
                    self.diff_find
                        .hits
                        .iter()
                        .all(|h| h.preview.contains("MAGIC_NEEDLE"))
                );
                self.engine.mark("diff cached content completed");
                self.select_diff_search_hit(index, cx);
                self.ui_feature_advance(4);
            }
            4 if !self.diff_find.jump && self.diff_find.selected.is_some() => {
                let paints = self.diff_find.scenario_paints.as_ref().unwrap().borrow();
                let (Some(results), Some(preview), Some(text)) =
                    (paints.get(&1), paints.get(&2), paints.get(&4))
                else {
                    return;
                };
                assert!(results.size.width >= px(200.) && results.size.height >= px(100.));
                assert!(text.size.width >= px(250.) && text.size.height >= px(14.));
                assert!(
                    preview.size.height >= px(20.)
                        && preview.right() <= window.viewport_size().width
                );
                assert!(
                    self.diff_find.selection_visible,
                    "changed content hit scrolls into a displayed hunk"
                );
                drop(paints);
                self.ui_feature_finish();
            }
            _ => {}
        }
    }
}
