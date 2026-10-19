//! Opt-in native action measurements. No scenario sends provider writes or opens URLs.
use super::*;

#[derive(Default)]
pub(super) struct State {
    phase: u8,
    expected: Option<gix::ObjectId>,
    selected: Vec<gix::ObjectId>,
    verified: bool,
    dense_rows: usize,
}

impl Desktop {
    fn improvement_focus_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Dispatch outside the entity update: keyboard listeners update this same
        // Desktop entity and otherwise would reenter its mutable borrow.
        self.select_focus.focus(window);
        self.improvement_bench.phase = 3;
        let weak = cx.entity().downgrade();
        window.defer(cx, move |window, cx| {
            if weak
                .update(cx, |v, cx| {
                    v.engine.mark("desktop improvement requested");
                    v.improvement_bench.phase = 2;
                    cx.notify();
                })
                .is_ok()
            {
                assert!(window.dispatch_keystroke(Keystroke::parse("ctrl-f").unwrap(), cx));
            }
        });
    }

    pub(super) fn improvement_scenario_start(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.scenario.as_deref() {
            Some("search-focus") => {
                self.improvement_focus_key(window, cx);
            }
            Some("branch-search-focus") => {
                self.action(Action::Branches, window, cx);
                self.improvement_bench.phase = 1;
            }
            Some("search-clear" | "search-clear-multi") => {
                self.improvement_bench.phase = 1;
                self.search_field = "";
                self.search.update(cx, |s, cx| {
                    s.set_value("m: merge", window, cx);
                });
            }
            Some("provider-scroll" | "provider-menu") => {
                // Discovery rebuilds graph labels. Seed only after its final event
                // so an in-flight Local/Branches message cannot erase the fixture.
                self.improvement_bench.phase = 1;
            }
            _ => return,
        }
        cx.notify();
    }

    pub(super) fn improvement_scenario_tick(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.improvement_bench.phase != 1 {
            return;
        }
        match self.scenario.as_deref() {
            Some("provider-scroll" | "provider-menu") => {
                let branches = &self.engine.app.branches;
                if branches.last_check.is_none()
                    || branches.loading
                    || branches.want_refresh.is_some()
                {
                    return;
                }
                self.seed_provider_scenario();
                self.engine.mark("desktop improvement requested");
                if self.scenario.as_deref() == Some("provider-scroll") {
                    self.scroll_step = 1;
                    self.frame_start = Some(Instant::now());
                    self.graph_scroll.scroll_to_item(0, ScrollStrategy::Top);
                } else {
                    self.action(Action::Select(0, false), window, cx);
                    self.context_menu = Some(point(px(220.), px(210.)));
                }
                self.improvement_bench.phase = 2;
                cx.notify();
            }
            Some("branch-search-focus") => {
                if self.engine.app.branches.busy
                    || self.engine.app.branches.activity_cache.is_empty()
                {
                    return;
                }
                self.improvement_focus_key(window, cx);
                cx.notify();
            }
            Some("search-clear" | "search-clear-multi") => {
                let Some(result) = &self.engine.app.search.result else {
                    return;
                };
                if !result.done {
                    return;
                }
                assert!(
                    result.rows.len() >= 2,
                    "search-clear fixture needs two merge matches"
                );
                let last = (result.rows.len() * 3 / 4)
                    .max(1)
                    .min(result.rows.len() - 1);
                if self.scenario.as_deref() == Some("search-clear-multi") {
                    assert!(
                        last >= 2,
                        "multi-selection fixture needs three merge matches"
                    );
                    self.action(Action::Select(last - 2, false), window, cx);
                    self.action(Action::Select(last - 1, true), window, cx);
                    self.action(Action::Select(last, true), window, cx);
                    // The focused row is now deselected: clearing must return to
                    // the most recently selected row that remains in the set.
                    self.action(Action::Select(last, true), window, cx);
                } else {
                    self.action(Action::Select(last, false), window, cx);
                }
                self.improvement_bench.selected =
                    self.selected_commits.ids(self.engine.app.selected_id());
                self.improvement_bench.expected = self.improvement_bench.selected.last().copied();
                // Render another viewport first: clearing must restore visibility itself.
                self.graph_scroll.scroll_to_item(0, ScrollStrategy::Top);
                self.improvement_bench.phase = 3;
                let weak = cx.entity().downgrade();
                window.on_next_frame(move |window, cx| {
                    let _ = weak.update(cx, |v, cx| {
                        v.engine.mark("desktop improvement requested");
                        v.improvement_bench.phase = 2;
                        v.search.update(cx, |s, cx| s.set_value("", window, cx));
                        cx.notify();
                    });
                });
                cx.notify();
            }
            _ => {}
        }
    }

    pub(super) fn improvement_scenario_ready(&self) -> Option<bool> {
        match self.scenario.as_deref() {
            Some("provider-scroll") => Some(self.scroll_step > 121),
            Some("search-clear" | "search-clear-multi") => {
                Some(self.improvement_bench.phase == 2 && !self.engine.app.search.active())
            }
            Some("search-focus" | "branch-search-focus" | "provider-menu") => {
                Some(self.improvement_bench.phase == 2)
            }
            _ => None,
        }
    }

    pub(super) fn improvement_scenario_assert(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(
            self.scenario.as_deref(),
            Some("provider-scroll" | "provider-menu")
        ) {
            let app = &self.engine.app;
            let end = (app.scroll + app.list_rows).min(app.graph_len());
            let mut commits = 0;
            let mut dense = 0;
            for row in app.scroll..end {
                let Some(index) = app.history_index(row) else {
                    continue;
                };
                commits += 1;
                let id = app.history.commits[index].id;
                if app.labels.get(&id).is_some_and(|labels| {
                    labels.iter().any(|label| {
                        label.kind == crate::git::RefKind::Branch
                            && label.name.starts_with("benchmark/")
                    })
                }) {
                    dense += 1;
                }
            }
            assert!(
                commits > 0 && dense == commits,
                "dense provider fixture lost labels: visible_commits={commits} synthetic_labels={dense} scroll={} list_rows={}",
                app.scroll,
                app.list_rows,
            );
            self.improvement_bench.dense_rows = dense;
        }
        match self.scenario.as_deref() {
            Some("search-focus") => {
                assert!(self.search.read(cx).focus_handle(cx).is_focused(window));
            }
            Some("branch-search-focus") => {
                assert!(
                    self.branch_filter
                        .read(cx)
                        .focus_handle(cx)
                        .is_focused(window)
                );
            }
            Some("search-clear" | "search-clear-multi") => {
                assert_eq!(
                    self.engine.app.selected_id(),
                    self.improvement_bench.expected
                );
                assert_eq!(
                    self.selected_commits.ids(self.engine.app.selected_id()),
                    self.improvement_bench.selected,
                );
                let row = self.engine.app.graph_selected();
                let scroll = self.graph_scroll.0.borrow();
                let item = scroll.last_item_size.expect("graph has a rendered row");
                // GPUI's `item` is the viewport, while `contents` spans all rows.
                let row_height = item.contents.height / self.engine.app.graph_len() as f32;
                let offset = scroll.base_handle.offset().y;
                let y = offset + row_height * row as f32;
                let viewport = scroll.base_handle.bounds().size.height;
                assert!(
                    y + row_height > px(0.)
                        && y < viewport
                        && row >= self.engine.app.scroll
                        && row < self.engine.app.scroll + self.engine.app.list_rows,
                    "selection not visible: row={row} row_height={row_height:?} y={y:?} viewport={viewport:?} offset={offset:?} app.scroll={} list_rows={} deferred={:?}",
                    self.engine.app.scroll,
                    self.engine.app.list_rows,
                    scroll.deferred_scroll_to_item,
                );
            }
            Some("provider-scroll") => {
                assert!(self.provider_scenario_ready(window, cx));
                assert_eq!(self.frame_samples.len(), 121);
                assert!(self.viewport_rows[0] > 0);
                assert!(
                    self.provider_scenario_summary()
                        .contains("provider_seeded=true")
                );
                assert!(
                    self.provider_scenario_summary()
                        .contains("provider_images=1")
                );
            }
            Some("provider-menu") => {
                assert!(self.provider_scenario_ready(window, cx));
                assert!(self.context_menu.is_some());
                assert!(
                    self.provider_scenario_summary()
                        .contains("provider_seeded=true")
                );
                assert!(
                    self.provider_scenario_summary()
                        .contains("provider_images=1")
                );
            }
            _ => return,
        }
        self.improvement_bench.verified = true;
    }

    pub(super) fn improvement_scenario_summary(&self) -> String {
        format!(
            "improvement_verified={} improvement_selected={} improvement_row={} improvement_scroll={} improvement_dense_rows={}\n{}",
            self.improvement_bench.verified,
            self.selected_commits
                .ids(self.engine.app.selected_id())
                .len(),
            self.engine.app.graph_selected(),
            self.engine.app.scroll,
            self.improvement_bench.dense_rows,
            self.provider_scenario_summary(),
        )
    }
}
