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
    scenario_rows_before_reveal: usize,
    scenario_first_long_hit: Option<Hit>,
    pub(super) input: Option<Entity<InputState>>,
    subscription: Option<Subscription>,
    worker: Option<Worker>,
    debounce: Option<Task<()>>,
    notifications: Option<Task<()>>,
    view: Option<u32>,
    files: Option<Arc<Vec<diff::FileChange>>>,
    pub(super) query: String,
    changed: Option<Instant>,
    sequence: u64,
    hits: Vec<Hit>,
    pub(super) matching_files: Vec<usize>,
    pub(super) busy: bool,
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
                        let was_active = !v.diff_find.query.trim().is_empty();
                        v.diff_find.query = input.read(cx).value().to_string();
                        if was_active && v.diff_find.query.trim().is_empty() {
                            v.reveal_current_diff_file();
                        }
                        if let Some(worker) = &v.diff_find.worker {
                            worker.cancel();
                        }
                        v.diff_find.changed = Some(Instant::now());
                        v.diff_find.hits.clear();
                        v.diff_find.matching_files.clear();
                        v.diff_find.scroll.scroll_to_item(0, ScrollStrategy::Top);
                        v.diff_find.jump = false;
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
        let mut auto_selected = false;
        let repository_root = self
            .engine
            .repository_root()
            .map(std::path::Path::to_path_buf);
        let Mode::Diff(view) = &mut self.engine.app.mode else {
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
            self.diff_find.matching_files.clear();
            self.diff_find.jump = false;
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
            self.diff_find.matching_files.clear();
            self.diff_find.jump = false;
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
            && let Some(root) = repository_root
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
                    self.diff_find.matching_files = result.matching_files;
                    self.diff_find.busy = !result.done;
                    self.diff_find.skipped = result.skipped;
                    self.diff_find.limited = result.limited;
                    if result.done
                        && view.current.is_none_or(|file| {
                            self.diff_find.matching_files.binary_search(&file).is_err()
                        })
                    {
                        if let Some(file) = self.diff_find.matching_files.first().copied() {
                            view.show(Some(file));
                            auto_selected = true;
                        }
                        self.diff_find.selected = None;
                        self.diff_find.jump = false;
                    }
                    completed |= result.done;
                    cx.notify();
                }
            }
        }
        let single = view
            .current_change()
            .is_some_and(|c| matches!(c.kind, diff::ChangeKind::Added | diff::ChangeKind::Deleted));
        if self.diff_find.jump
            && self
                .diff_find
                .selected
                .as_ref()
                .is_some_and(|hit| view.current != Some(hit.file))
        {
            self.diff_find.jump = false;
            self.diff_find.selected = None;
        }
        if self.diff_find.jump
            && let Some(hit) = &self.diff_find.selected
            && view.diff_for == Some(hit.file)
            && view.current == Some(hit.file)
            && let Some(diff) = &mut view.diff
        {
            if let Some(line) = hit.line {
                // Reveal only the selected unchanged line, retaining the hunk layout.
                if !diff.rows.iter().any(|row| matches!(row, diff::Row::Line { left, right } if (if hit.right { right } else { left }).as_ref().is_some_and(|l| l.no == line))) {
                    let side = usize::from(hit.right);
                    let text = diff.source[side].split_inclusive(|b| *b == b'\n').nth(line.saturating_sub(1) as usize).map(diff::sidebyside::decode);
                    if let Some(text) = text {
                        let insert = diff.rows.iter().position(|row| matches!(row, diff::Row::Line { left, right } if (if hit.right { right } else { left }).as_ref().is_some_and(|l| l.no > line))).unwrap_or(diff.rows.len());
                        diff.max_width = diff.max_width.max(text.chars().count());
                        let paired_no = diff.rows.iter().filter_map(|row| match row {
                            diff::Row::Line { left: Some(left), right: Some(right) } if left.kind == diff::LineKind::Context && right.kind == diff::LineKind::Context => {
                                let (near, other) = if hit.right { (right.no, left.no) } else { (left.no, right.no) };
                                Some((near.abs_diff(line), i64::from(line) + i64::from(other) - i64::from(near)))
                            }
                            _ => None,
                        }).min_by_key(|(distance, _)| *distance).and_then(|(_, no)| u32::try_from(no).ok()).filter(|no| *no > 0);
                        let paired = paired_no.and_then(|no| diff.source[1 - side].split_inclusive(|b| *b == b'\n').nth(no as usize - 1).map(diff::sidebyside::decode).filter(|other| other == &text).map(|text| diff::Line { no, text, kind: diff::LineKind::Context, hl: Vec::new() }));
                        let revealed = Some(diff::Line { no: line, text, kind: diff::LineKind::Context, hl: Vec::new() });
                        let row = if hit.right { diff::Row::Line { left: paired, right: revealed } } else { diff::Row::Line { left: revealed, right: paired } };
                        // A separator is split around the newly visible line so its
                        // skipped count remains accurate on repeated navigation.
                        if insert > 0 && matches!(diff.rows[insert - 1], diff::Row::Separator { .. }) {
                            let previous = diff.rows[..insert - 1].iter().rev().find_map(|row| match row {
                                diff::Row::Line { left, right } => (if hit.right { right } else { left }).as_ref().map(|line| line.no),
                                _ => None,
                            }).unwrap_or(0);
                            let diff::Row::Separator { skipped } = diff.rows[insert - 1] else { unreachable!() };
                            let before = line.saturating_sub(previous + 1).min(skipped);
                            let after = skipped.saturating_sub(before + 1);
                            let mut rows = Vec::with_capacity(3);
                            if before > 0 { rows.push(diff::Row::Separator { skipped: before }); }
                            rows.push(row);
                            if after > 0 { rows.push(diff::Row::Separator { skipped: after }); }
                            diff.rows.splice(insert - 1..insert, rows);
                        } else {
                            diff.rows.insert(insert, row);
                        }
                        // Replacing a one-line separator does not change row count,
                        // which is part of the wrap cache key. Invalidate explicitly.
                        self.wrap_layout = Default::default();
                    }
                }
                let target = diff.rows.iter().position(|row| match row {
                    diff::Row::Line { left, right } => if hit.right { right } else { left }
                        .as_ref()
                        .is_some_and(|l| l.no == line),
                    _ => false,
                });
                self.diff_find.selection_visible = target.is_some();
                if let Some(target) = target {
                    let match_offset = hit.offset.unwrap_or(0);
                    if self.word_wrap {
                        let width =
                            (f32::from(window.viewport_size().width) - self.diff_sidebar - 6.)
                                / if single { 1. } else { 2. }
                                - 68.;
                        self.wrap_layout
                            .update(view.id, hit.file, diff, width, window);
                    } else {
                        let width = self.diff_horizontal.bounds.get().size.width;
                        self.diff_horizontal.configure(
                            width,
                            px(diff.max_width as f32 * 7.4),
                            single,
                        );
                        if let diff::Row::Line { left, right } = &diff.rows[target]
                            && let Some(line) = if hit.right { right } else { left }
                        {
                            let column = line.text[..match_offset].chars().count();
                            self.diff_horizontal
                                .set_x(px(-(column as f32 * 7.4 - 40.).max(0.)));
                        }
                    }
                    self.diff_scroll.scroll_to_item(
                        if self.word_wrap {
                            self.wrap_layout
                                .rows
                                .iter()
                                .position(|row| {
                                    row.source == target
                                        && row.sides[usize::from(hit.right)]
                                            .as_ref()
                                            .is_some_and(|range| range.contains(&match_offset))
                                })
                                .unwrap_or_else(|| self.wrap_layout.visual(target))
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
        if auto_selected {
            self.reset_diff_file_scroll();
            self.changed(cx);
            if self.ui_feature_name() == Some("diff-search") {
                let Mode::Diff(view) = &mut self.engine.app.mode else {
                    unreachable!()
                };
                assert!(
                    view.pending().is_empty(),
                    "filtered file diff was dispatched in the result callback without waiting for the periodic engine tick"
                );
                self.engine.mark("diff filtered file dispatched");
            }
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
            .child(Button::new("diff-search-previous").xsmall().ghost().child(icon("back")).tooltip("Previous match").disabled(self.diff_find.hits.is_empty() || self.diff_find.busy).on_click(cx.listener(|v, _, _, cx| v.navigate_diff_search(false, cx))))
            .child(Button::new("diff-search-next").xsmall().ghost().child(icon("next")).tooltip("Next match").disabled(self.diff_find.hits.is_empty() || self.diff_find.busy).on_click(cx.listener(|v, _, _, cx| v.navigate_diff_search(true, cx))))
            .child(caption(status).id("diff-search-count").tooltip(|w,cx| gpui_component::tooltip::Tooltip::new("Navigation lists up to 1,000 matches. The file list includes every matching filename or indexed file, including files beyond that navigation limit.").build(w,cx)))
            .when(self.diff_find.skipped > 0, |s| s.child(caption(format!("{} files: names only", self.diff_find.skipped)).id("diff-search-skipped")
                .tooltip(|w,cx| gpui_component::tooltip::Tooltip::new("Binary, unavailable or oversized contents are omitted. The content index is limited to 64 MiB.").build(w,cx))))
            .into_any_element()
    }
    pub(super) fn diff_search_results(&self, cx: &mut Context<Self>) -> AnyElement {
        let list = uniform_list(
            "diff-search-results",
            self.diff_find.matching_files.len(),
            cx.processor(|v, range: std::ops::Range<usize>, _, cx| {
                let Mode::Diff(view) = &v.engine.app.mode else {
                    return Vec::new();
                };
                range
                    .map(|i| {
                        let file = v.diff_find.matching_files[i];
                        let path = view.changes.files[file].path.clone();
                        div()
                            .id(("diff-search-file", file))
                            .w_full()
                            .h(px(30.))
                            .px_2()
                            .overflow_hidden()
                            .bg(rgb(if view.current == Some(file) {
                                SELECTED
                            } else {
                                BG
                            }))
                            .hover(|s| s.bg(rgb(HOVER)))
                            .cursor_pointer()
                            .min_w_0()
                            .truncate()
                            .child(path)
                            .on_click(cx.listener(move |v, _, _, cx| {
                                v.select_diff_search_file(file, cx);
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
            .when(self.diff_find.matching_files.is_empty(), |s| {
                s.child(
                    caption(if self.diff_find.busy {
                        "Searching filenames and both file versions…"
                    } else {
                        "No matching files"
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
    fn reset_diff_file_scroll(&mut self) {
        self.diff_scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.diff_horizontal.set_x(px(0.));
        *self.selection.borrow_mut() = Default::default();
        self.wrap_anchor = None;
    }
    fn select_diff_search_file(&mut self, file: usize, cx: &mut Context<Self>) {
        self.diff_find.jump = false;
        self.diff_find.selected = None;
        if let Mode::Diff(view) = &mut self.engine.app.mode {
            if file >= view.changes.files.len() {
                return;
            }
            view.show(Some(file));
        }
        self.reset_diff_file_scroll();
        self.changed(cx);
    }
    fn reveal_current_diff_file(&mut self) {
        if let Mode::Diff(view) = &mut self.engine.app.mode
            && let Some(row) = reveal_current_file(view)
        {
            self.diff_files_scroll
                .scroll_to_item(row, ScrollStrategy::Center);
        }
    }
    fn select_diff_search_hit(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(hit) = self.diff_find.hits.get(index).cloned() else {
            return;
        };
        if let Mode::Diff(view) = &mut self.engine.app.mode {
            let switching = view.current != Some(hit.file);
            view.show(Some(hit.file));
            if switching {
                self.reset_diff_file_scroll();
            }
        }
        self.diff_find.selected = Some(hit);
        self.diff_find.jump = true;
        self.diff_find.selection_visible = false;
        self.changed(cx);
    }
    fn navigate_diff_search(&mut self, forward: bool, cx: &mut Context<Self>) {
        let count = self.diff_find.hits.len();
        if count == 0 || self.diff_find.busy {
            return;
        }
        let current = self
            .diff_find
            .selected
            .as_ref()
            .and_then(|selected| self.diff_find.hits.iter().position(|hit| hit == selected));
        let index = current.map_or(if forward { 0 } else { count - 1 }, |i| {
            if forward {
                (i + 1) % count
            } else {
                (i + count - 1) % count
            }
        });
        self.select_diff_search_hit(index, cx);
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

/// Restore the exact indexed file under a filtered sidebar, expanding only its
/// ancestors. File indices and tree node indices are intentionally different.
fn reveal_current_file(view: &mut crate::diffview::DiffView) -> Option<usize> {
    let file = view.current?;
    let node = view
        .changes
        .tree
        .nodes
        .iter()
        .position(|node| node.file == Some(file))?;
    let mut parent = view.changes.tree.nodes[node].parent;
    while let Some(index) = parent {
        view.changes.tree.nodes[index].expanded = true;
        parent = view.changes.tree.nodes[index].parent;
    }
    view.changes.tree.rebuild_visible();
    let row = view
        .changes
        .tree
        .visible
        .iter()
        .position(|entry| entry.node == node)?;
    view.cursor.select(&view.changes.tree, row);
    Some(row)
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
                && matches!(self.diff_find.scenario_phase, 2 | 3 | 5 | 7 | 11 | 13))
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
                assert_eq!(
                    self.diff_find.matching_files.len(),
                    self.diff_find.files.as_ref().unwrap().len()
                );
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
                let selected = self.diff_find.selected.clone();
                self.navigate_diff_search(true, cx);
                assert_ne!(
                    self.diff_find.selected, selected,
                    "next arrow changes the actual content selection"
                );
                self.navigate_diff_search(false, cx);
                assert_eq!(
                    self.diff_find.selected, selected,
                    "previous arrow returns to the match"
                );
                self.ui_feature_query("source line 420 ordinary", window, cx);
                self.ui_feature_advance(5);
            }
            5 if self.diff_find.query == "source line 420 ordinary"
                && !self.diff_find.busy
                && !self.diff_find.hits.is_empty() =>
            {
                assert!(
                    !self.diff_find.jump,
                    "the replacement query cancels the old jump before its results can navigate"
                );
                self.navigate_diff_search(true, cx);
                self.ui_feature_advance(6);
            }
            6 if !self.diff_find.jump && self.diff_find.selection_visible => {
                let paints = self.diff_find.scenario_paints.as_ref().unwrap().borrow();
                if !paints.contains_key(&2) || !paints.contains_key(&4) || !paints.contains_key(&5)
                {
                    return;
                }
                assert!(
                    paints[&5].size.width >= px(250.) && paints[&5].size.height == px(24.),
                    "actual content rows painted"
                );
                let hit = self.diff_find.selected.as_ref().unwrap();
                let Mode::Diff(view) = &self.engine.app.mode else {
                    panic!("diff remains open")
                };
                assert_eq!(view.current, Some(hit.file));
                assert!(view.diff.as_ref().unwrap().rows.iter().any(|row| matches!(row, diff::Row::Line {left, right} if (if hit.right {right} else {left}).as_ref().is_some_and(|line| line.no == 420 && !diff::search::highlight_ranges(&line.text, &self.diff_find.query).is_empty()))));
                drop(paints);
                self.ui_feature_query("LONG_SEGMENT_NEEDLE", window, cx);
                self.ui_feature_advance(7);
            }
            7 if self.diff_find.query == "LONG_SEGMENT_NEEDLE"
                && !self.diff_find.busy
                && !self.diff_find.hits.is_empty() =>
            {
                assert!(!self.word_wrap);
                self.navigate_diff_search(true, cx);
                self.diff_find.scenario_first_long_hit = self.diff_find.selected.clone();
                self.ui_feature_advance(8);
            }
            8 if !self.diff_find.jump && self.diff_find.selection_visible => {
                if !self
                    .diff_find
                    .scenario_paints
                    .as_ref()
                    .unwrap()
                    .borrow()
                    .contains_key(&5)
                {
                    return;
                }
                assert!(
                    self.diff_horizontal.x.get() < px(-500.),
                    "long match scrolls the actual unwrapped text horizontally"
                );
                self.action(Action::Wrap, window, cx);
                self.ui_feature_advance(9);
            }
            9 if self.word_wrap
                && self
                    .diff_find
                    .scenario_paints
                    .as_ref()
                    .unwrap()
                    .borrow()
                    .contains_key(&5) =>
            {
                self.navigate_diff_search(true, cx);
                self.ui_feature_advance(10);
            }
            10 if !self.diff_find.jump && self.diff_find.selection_visible => {
                if !self
                    .diff_find
                    .scenario_paints
                    .as_ref()
                    .unwrap()
                    .borrow()
                    .contains_key(&5)
                {
                    return;
                }
                let hit = self.diff_find.selected.as_ref().unwrap();
                let Mode::Diff(view) = &self.engine.app.mode else {
                    panic!("diff stays open")
                };
                let diff = view.diff.as_ref().unwrap();
                let source = diff.rows.iter().position(|row| matches!(row, diff::Row::Line {left, right} if (if hit.right {right} else {left}).as_ref().is_some_and(|line| Some(line.no) == hit.line))).unwrap();
                let offset = hit.offset.unwrap();
                let first = self.diff_find.scenario_first_long_hit.as_ref().unwrap();
                assert_eq!(
                    (hit.file, hit.right, hit.line),
                    (first.file, first.right, first.line),
                    "next occurrence remains in the same file, side and long line"
                );
                assert!(
                    hit.offset > first.offset,
                    "next occurrence advances within the same line"
                );
                let visual = self
                    .wrap_layout
                    .rows
                    .iter()
                    .position(|row| {
                        row.source == source
                            && row.sides[usize::from(hit.right)]
                                .as_ref()
                                .is_some_and(|range| range.contains(&offset))
                    })
                    .unwrap();
                assert!(
                    visual > self.wrap_layout.visual(source),
                    "target is a later wrapped segment"
                );
                let top = (-f32::from(self.diff_scroll.0.borrow().base_handle.offset().y) / 24.)
                    .max(0.) as usize;
                let visible = (f32::from(self.diff_horizontal.bounds.get().size.height) / 24.)
                    .ceil() as usize;
                assert!(
                    visual >= top && visual < top + visible,
                    "wrapped match segment scrolls inside actual content viewport"
                );
                assert!(
                    diff.rows
                        .iter()
                        .any(|row| matches!(row, diff::Row::Separator { skipped: 1 })),
                    "fixture includes one collapsed unchanged line between changed hunks"
                );
                self.diff_find.scenario_rows_before_reveal = diff.rows.len();
                self.ui_feature_query("source line 254 ordinary", window, cx);
                self.ui_feature_advance(11);
            }
            11 if self.diff_find.query == "source line 254 ordinary"
                && !self.diff_find.busy
                && !self.diff_find.hits.is_empty() =>
            {
                self.navigate_diff_search(true, cx);
                self.ui_feature_advance(12);
            }
            12 if !self.diff_find.jump && self.diff_find.selection_visible => {
                if !self
                    .diff_find
                    .scenario_paints
                    .as_ref()
                    .unwrap()
                    .borrow()
                    .contains_key(&5)
                {
                    return;
                }
                let Mode::Diff(view) = &self.engine.app.mode else {
                    panic!("diff stays open")
                };
                let diff = view.diff.as_ref().unwrap();
                assert_eq!(
                    diff.rows.len(),
                    self.diff_find.scenario_rows_before_reveal,
                    "revealing a one-line gap keeps the wrap cache row-count key unchanged"
                );
                let source = diff.rows.iter().position(|row| matches!(row, diff::Row::Line {left: Some(line), ..} if line.no == 254)).unwrap();
                assert!(
                    self.wrap_layout.rows.iter().any(|row| row.source == source
                        && row.sides[0].is_some()
                        && row.sides[1].is_some()),
                    "one-line separator replacement rebuilds both wrapped text spans"
                );
                self.ui_feature_query("source line 421 ordinary content in file 7", window, cx);
                self.ui_feature_advance(13);
            }
            13 if self.diff_find.query == "source line 421 ordinary content in file 7"
                && !self.diff_find.busy
                && !self.diff_find.hits.is_empty() =>
            {
                assert_eq!(self.diff_find.matching_files.len(), 1);
                let Mode::Diff(view) = &self.engine.app.mode else {
                    panic!("diff stays open")
                };
                assert_eq!(
                    view.current_change().unwrap().path,
                    "needle-file-7.txt",
                    "filter completion selects the matching file without an arrow click"
                );
                assert!(self.diff_find.selected.is_none());
                self.navigate_diff_search(true, cx);
                self.ui_feature_advance(14);
            }
            14 if !self.diff_find.jump && self.diff_find.selection_visible => {
                if !self
                    .diff_find
                    .scenario_paints
                    .as_ref()
                    .unwrap()
                    .borrow()
                    .contains_key(&5)
                {
                    return;
                }
                self.ui_feature_query("needle-file-1", window, cx);
                self.ui_feature_advance(15);
            }
            15 if !self.diff_find.busy
                && self.diff_find.query == "needle-file-1"
                && self.diff_find.matching_files.len() > 1 =>
            {
                let Mode::Diff(view) = &mut self.engine.app.mode else {
                    unreachable!()
                };
                let file = view
                    .changes
                    .files
                    .iter()
                    .position(|file| file.path == "nested/needle-file-17.txt")
                    .unwrap();
                assert!(self.diff_find.matching_files.contains(&file));
                view.changes.tree.set_all_expanded(false);
                self.diff_horizontal.set_x(px(-800.));
                self.select_diff_search_file(file, cx);
                assert_eq!(
                    self.diff_horizontal.x.get(),
                    px(0.),
                    "search file click resets actual horizontal scroll"
                );
                self.ui_feature_advance(16);
            }
            16 if matches!(&self.engine.app.mode, Mode::Diff(view) if view.diff_for == view.current && view.diff.is_some()) =>
            {
                assert_eq!(
                    self.diff_scroll.0.borrow().base_handle.offset().y,
                    px(0.),
                    "search file click resets actual vertical scroll"
                );
                self.clear_diff_search(window, cx);
                self.ui_feature_advance(17);
            }
            17 if !self.diff_search_active() => {
                let Mode::Diff(view) = &self.engine.app.mode else {
                    unreachable!()
                };
                assert_eq!(
                    view.current_change().unwrap().path,
                    "nested/needle-file-17.txt"
                );
                assert_eq!(
                    view.cursor.file(&view.changes.tree),
                    view.current,
                    "clear search restores exact file in expanded tree"
                );
                let node = view
                    .changes
                    .tree
                    .nodes
                    .iter()
                    .position(|node| {
                        node.file.is_some_and(|file| {
                            view.changes.files[file].path == "nested/needle-file-16.txt"
                        })
                    })
                    .unwrap();
                self.action(Action::File(node), window, cx);
                self.ui_feature_advance(18);
            }
            18 if matches!(&self.engine.app.mode, Mode::Diff(view) if view.diff_for == view.current && view.current_change().is_some_and(|file| file.path == "nested/needle-file-16.txt")) =>
            {
                self.ui_feature_query("MAGIC_NEEDLE", window, cx);
                self.ui_feature_advance(19);
            }
            19 if !self.diff_find.busy && !self.diff_find.hits.is_empty() => {
                self.ui_feature_finish()
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::reveal_current_file;
    use crate::{
        diff::{ChangeKind, FileChange, TreeDiff, changes::ChangeSet},
        diffview::DiffView,
    };
    #[test]
    fn clearing_filter_reveals_correct_file_through_collapsed_compacted_folders() {
        let id = gix::ObjectId::from_bytes_or_panic(&[1; 20]);
        let mut changes = ChangeSet::new(1, Some(id), id);
        let files = ["root.txt", "src/nested/one.txt", "src/nested/two.txt"]
            .into_iter()
            .map(|path| FileChange {
                path: path.into(),
                old_path: None,
                kind: ChangeKind::Modified,
                old: None,
                new: None,
                remote: None,
                worktree: false,
            })
            .collect();
        changes.set_tree(Ok(TreeDiff { files }));
        let mut view = DiffView::new(1, changes, None, 30);
        view.changes.tree.set_all_expanded(false);
        view.show(Some(2));
        let row = reveal_current_file(&mut view).unwrap();
        assert_eq!(
            view.changes.tree.nodes[view.changes.tree.visible[row].node].file,
            Some(2)
        );
        assert_eq!(view.cursor.file(&view.changes.tree), Some(2));
        assert_ne!(
            view.changes.tree.visible[row].node, 2,
            "file list and tree node indices differ"
        );
    }
}
