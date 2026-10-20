//! Opt-in native TODO scenarios. Normal startup allocates no fixture/probe state.
use super::*;
use crate::branches::{SortColumn, Target};
use std::{cell::RefCell, rc::Rc};

const DRAFT: &str = "TODO benchmark draft preserved";
const FRAMES: usize = 120;

#[derive(Clone, Copy)]
struct Paint {
    epoch: u64,
    bounds: Bounds<Pixels>,
}

#[derive(serde::Serialize)]
struct Observation {
    frame: usize,
    slot: usize,
    bounds: [f32; 4],
}

struct Run {
    name: &'static str,
    phase: u8,
    epoch: u64,
    scheduled: bool,
    started: Instant,
    verified: bool,
    checks: usize,
    head: Option<gix::ObjectId>,
    target: Option<Target>,
    query: String,
    stash_ids: Vec<String>,
    operation_epoch: u64,
    expected_hover: Option<usize>,
    hover_rows_max: usize,
    hover_cells_max: usize,
    frame_started: Option<Instant>,
    samples: Vec<f64>,
    handlers: Vec<f64>,
    observations: Vec<Observation>,
    tool_bounds: [Option<[f32; 4]>; 4],
    tool_scrolls: usize,
    tool_list_fixture: bool,
    tool_ids: Vec<String>,
    tool_custom_id: Option<String>,
    settings_scroll: Option<ScrollHandle>,
    paints: Rc<RefCell<[Option<Paint>; 14]>>,
}

#[derive(Default)]
pub(super) struct State {
    run: Option<Box<Run>>,
}

impl Desktop {
    pub(super) fn todo_scenario_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = match self.scenario.as_deref() {
            Some("todo-graph") => "todo-graph",
            Some("branch-sort") => "branch-sort",
            Some("graph-hover") => "graph-hover",
            Some("stash-panel") => "stash-panel",
            Some("stash-apply") => "stash-apply",
            Some("stash-delete") => "stash-delete",
            Some("external-tools") => "external-tools",
            Some("ref-storage") => "ref-storage",
            _ => return,
        };
        assert!(self.todo_bench.run.is_none());
        if matches!(
            name,
            "todo-graph" | "stash-panel" | "stash-apply" | "stash-delete" | "ref-storage"
        ) {
            let root = self
                .engine
                .repository_root()
                .expect("TODO fixture repository");
            assert_eq!(
                std::fs::read_to_string(root.join(".git/git-rustle-todos-fixture"))
                    .expect("disposable TODO fixture marker"),
                "disposable todos benchmark fixture"
            );
        }
        self.todo_bench.run = Some(Box::new(Run {
            name,
            phase: 0,
            epoch: 1,
            scheduled: false,
            started: Instant::now(),
            verified: false,
            checks: 0,
            head: self.engine.app.head_id(),
            target: None,
            query: String::new(),
            stash_ids: Vec::new(),
            operation_epoch: self.engine.operation_epoch,
            expected_hover: None,
            hover_rows_max: 0,
            hover_cells_max: 0,
            frame_started: None,
            samples: Vec::new(),
            handlers: Vec::new(),
            observations: Vec::new(),
            tool_bounds: [None; 4],
            tool_scrolls: 0,
            tool_list_fixture: false,
            tool_ids: Vec::new(),
            tool_custom_id: None,
            settings_scroll: (name == "external-tools").then(ScrollHandle::new),
            paints: Rc::new(RefCell::new([None; 14])),
        }));
        match name {
            "todo-graph" => {
                assert!(self.engine.app.pending.exists(), "dirty TODO graph fixture");
                assert!(self.engine.app.history.len() >= 3);
                assert!(
                    self.engine.app.pending.row() > 0,
                    "fixture must put newer branch above HEAD/pending"
                );
                self.description
                    .update(cx, |s, cx| s.set_value(DRAFT, window, cx));
                let id = self.engine.app.history.commits.last().unwrap().id;
                self.search_field = "";
                self.search
                    .update(cx, |s, cx| s.set_value(format!("s: {id}"), window, cx));
                self.todo_advance(1);
            }
            "branch-sort" => {
                self.engine.mark("desktop scenario requested");
                self.action(Action::Branches, window, cx);
                self.todo_advance(1);
            }
            "graph-hover" => {
                self.engine.mark("desktop scenario requested");
                if let Some(row) = self.graph_hover.anchor {
                    self.hover_commit(row, false, cx);
                }
                self.todo_advance(1);
            }
            "stash-panel" | "stash-apply" | "stash-delete" => {
                assert!(
                    self.stash_entries().is_empty() && !self.stashes.open,
                    "stash listing must remain lazy"
                );
                self.engine.mark("desktop scenario requested");
                self.action(
                    Action::Stashes(Box::new(stashes::Action::Toggle)),
                    window,
                    cx,
                );
                self.todo_advance(1);
            }
            _ => {
                self.engine.mark("desktop scenario requested");
                self.action(Action::Settings, window, cx);
                self.todo_advance(1);
            }
        }
        cx.notify();
    }

    fn todo_advance(&mut self, phase: u8) {
        let run = self.todo_bench.run.as_mut().unwrap();
        run.phase = phase;
        run.epoch += 1;
    }

    /// Attach to the actual rendered container: toolbar, pending row, stashes,
    /// tools, reference storage, Diff, and branch header respectively.
    pub(super) fn todo_probe(&self, slot: usize) -> Option<AnyElement> {
        let run = self.todo_bench.run.as_ref()?;
        let paints = run.paints.clone();
        let epoch = run.epoch;
        Some(
            canvas(
                move |bounds, _, _| bounds,
                move |bounds, _, _, _| {
                    paints.borrow_mut()[slot] = Some(Paint { epoch, bounds });
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .into_any_element(),
        )
    }

    fn todo_painted(&self, slot: usize, window: &Window) -> bool {
        let run = self.todo_bench.run.as_ref().unwrap();
        run.paints.borrow()[slot].is_some_and(|paint| {
            paint.epoch == run.epoch
                && paint.bounds.size.width > px(0.)
                && paint.bounds.size.height > px(0.)
                && paint.bounds.right() > px(0.)
                && paint.bounds.left() < window.viewport_size().width
                && paint.bounds.bottom() > px(0.)
                && paint.bounds.top() < window.viewport_size().height
        })
    }

    pub(super) fn todo_scenario_schedule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(run) = self
            .todo_bench
            .run
            .as_mut()
            .filter(|r| !r.verified && !r.scheduled)
        else {
            return;
        };
        run.scheduled = true;
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = weak.update(cx, |v, cx| {
                if let Some(run) = &mut v.todo_bench.run {
                    run.scheduled = false;
                }
                v.todo_scenario_tick(window, cx);
            });
        });
    }

    pub(super) fn todo_scenario_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(run) = self.todo_bench.run.as_ref().filter(|r| !r.verified) else {
            return;
        };
        assert!(
            run.started.elapsed() < Duration::from_secs(45),
            "TODO scenario {} stalled phase {} epoch {} checks {}",
            run.name,
            run.phase,
            run.epoch,
            run.checks
        );
        let (name, phase) = (run.name, run.phase);
        match name {
            "todo-graph" => self.todo_graph_tick(phase, window, cx),
            "branch-sort" => self.todo_sort_tick(phase, window, cx),
            "graph-hover" => self.todo_hover_tick(phase, window, cx),
            "stash-panel" | "stash-apply" | "stash-delete" => {
                self.todo_stash_tick(name, phase, window, cx)
            }
            "external-tools" => self.todo_tools_tick(phase, window, cx),
            "ref-storage" => self.todo_storage_tick(phase, window, cx),
            _ => unreachable!(),
        }
        // Completion needs one final render to schedule scenario presentation;
        // idle fixtures may have no worker messages left to wake the window.
        cx.notify();
    }

    fn todo_graph_tick(&mut self, phase: u8, window: &mut Window, cx: &mut Context<Self>) {
        match phase {
            1 if self
                .engine
                .app
                .search
                .result
                .as_ref()
                .is_some_and(|r| r.done && r.rows.len() == 1) =>
            {
                self.action(Action::Select(0, false), window, cx);
                assert!(!self.engine.app.pending.selected);
                assert_eq!(self.engine.app.pending.description.value, DRAFT);
                self.engine.mark("desktop scenario requested");
                self.action(Action::Head, window, cx);
                self.todo_advance(2);
            }
            2 if self.todo_painted(0, window) && self.todo_painted(1, window) => {
                let app = &self.engine.app;
                assert!(app.pending.selected && !app.search.active());
                assert_eq!(app.graph_selected(), app.pending.row());
                assert!(app.pending.row() > 0);
                assert_eq!(app.pending.description.value, DRAFT);
                let head_row = app.pending.row() + 1;
                self.todo_bench.run.as_mut().unwrap().checks += 5;
                self.hover_commit(head_row, true, cx);
                self.todo_advance(3);
            }
            3 if self.todo_painted(1, window) => {
                let app = &self.engine.app;
                let pending = app.pending.row();
                let head = app.pending.head_index.expect("HEAD geometry available");
                let lane = app.history.commits[head].node_lane as usize;
                assert!(
                    self.graph_hover
                        .row(pending)
                        .get(lane)
                        .is_some_and(|flag| flag & crate::graph::hover::GLYPH != 0),
                    "pending={pending} lane={lane} anchor={:?} cache={:?} scroll={} rows={}",
                    self.graph_hover.anchor,
                    self.graph_hover.diagnostics(),
                    app.scroll,
                    app.list_rows,
                );
                assert!(self.graph_hover.diagnostics().1 > 0);
                self.hover_commit(pending + 1, false, cx);
                self.todo_bench.run.as_mut().unwrap().checks += 2;
                self.action(Action::Diff, window, cx);
                self.todo_advance(4);
            }
            4 if self.todo_painted(5, window)
                && matches!(&self.engine.app.mode, Mode::Diff(d) if d.diff.is_some()) =>
            {
                assert!(self.engine.app.pending.selected);
                self.todo_bench.run.as_mut().unwrap().checks += 2;
                self.select_focus.focus(window);
                self.todo_advance(5);
                // The actual root capture handler must see Escape, outside this
                // entity's mutable borrow, including with a child focus handle.
                window.defer(cx, |window, cx| {
                    assert!(window.dispatch_keystroke(Keystroke::parse("escape").unwrap(), cx));
                });
            }
            5 if matches!(self.engine.app.mode, Mode::Graph)
                && self.todo_painted(0, window)
                && self.todo_painted(1, window) =>
            {
                let app = &self.engine.app;
                assert!(app.pending.selected && app.parked_diff.is_some());
                assert_eq!(app.graph_selected(), app.pending.row());
                assert_eq!(app.pending.description.value, DRAFT);
                self.todo_bench.run.as_mut().unwrap().checks += 4;
                self.todo_finish();
            }
            _ => {}
        }
    }

    fn todo_sort_tick(&mut self, phase: u8, window: &mut Window, cx: &mut Context<Self>) {
        if phase == 1 {
            if self.engine.app.branches.loading
                || self.engine.app.branches.entries.len() < 2
                || self.engine.app.branches.activity_cache.is_empty()
            {
                return;
            }
            let query = self.engine.app.branches.entries[0]
                .target
                .name
                .chars()
                .next()
                .unwrap()
                .to_lowercase()
                .to_string();
            self.todo_bench.run.as_mut().unwrap().query = query.clone();
            self.branch_filter
                .update(cx, |s, cx| s.set_value(query, window, cx));
            self.todo_advance(2);
            return;
        }
        if phase == 2 {
            let run = self.todo_bench.run.as_ref().unwrap();
            if self.engine.app.branches.query != run.query || !self.todo_painted(6, window) {
                return;
            }
            let selected = self.engine.app.branches.visible.len() / 2;
            self.action(Action::Branch(selected), window, cx);
            self.todo_bench.run.as_mut().unwrap().target =
                self.engine.app.branches.chosen().map(|e| e.target.clone());
            self.todo_frame_action(window, cx);
            return;
        }
        if phase != 3 || !self.todo_painted(6, window) {
            return;
        }
        self.todo_record_frame(6);
        let browser = &self.engine.app.branches;
        let run = self.todo_bench.run.as_ref().unwrap();
        assert_eq!(browser.chosen().map(|e| &e.target), run.target.as_ref());
        assert_eq!(browser.query, run.query);
        for pair in browser.entries.windows(2) {
            let order = match browser.sort.column {
                SortColumn::Name => pair[0].target.name.cmp(&pair[1].target.name),
                SortColumn::Activity => pair[0]
                    .activity
                    .as_ref()
                    .map(|a| (a.recent, a.commit_time))
                    .cmp(&pair[1].activity.as_ref().map(|a| (a.recent, a.commit_time))),
                SortColumn::LastCommit => pair[0]
                    .activity
                    .as_ref()
                    .map(|a| a.commit_time)
                    .cmp(&pair[1].activity.as_ref().map(|a| a.commit_time)),
            };
            assert!(if browser.sort.descending {
                !order.is_lt()
            } else {
                !order.is_gt()
            });
        }
        self.todo_bench.run.as_mut().unwrap().checks += 3;
        if self.todo_bench.run.as_ref().unwrap().samples.len() == FRAMES {
            self.todo_finish();
        } else {
            self.todo_frame_action(window, cx);
        }
    }

    fn todo_hover_tick(&mut self, phase: u8, window: &mut Window, cx: &mut Context<Self>) {
        if phase == 1 {
            if !self.todo_painted(0, window) {
                return;
            }
            assert_eq!(self.graph_hover.diagnostics(), (0, 0));
            self.todo_frame_action(window, cx);
            return;
        }
        if phase != 3 || !self.todo_painted(0, window) {
            return;
        }
        self.todo_record_frame(0);
        let (rows, cells) = self.graph_hover.diagnostics();
        let expected = self.todo_bench.run.as_ref().unwrap().expected_hover;
        assert_eq!(
            self.graph_hover.anchor,
            expected,
            "hover cache={:?} scroll={} rows={} frame={}",
            self.graph_hover.diagnostics(),
            self.engine.app.scroll,
            self.engine.app.list_rows,
            self.todo_bench.run.as_ref().unwrap().samples.len()
        );
        if expected.is_some() {
            assert_eq!(rows, self.engine.app.list_rows);
            assert!(
                cells > 0 && rows < self.engine.app.history.len(),
                "highlight only visible geometry"
            );
        } else {
            assert_eq!((rows, cells), (0, 0));
        }
        let run = self.todo_bench.run.as_mut().unwrap();
        run.hover_rows_max = run.hover_rows_max.max(rows);
        run.hover_cells_max = run.hover_cells_max.max(cells);
        run.checks += 3;
        if run.samples.len() == FRAMES {
            self.todo_finish();
        } else {
            self.todo_frame_action(window, cx);
        }
    }

    fn todo_frame_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let run = self.todo_bench.run.as_ref().unwrap();
        let (name, frame, previous_hover) = (run.name, run.samples.len(), run.expected_hover);
        let start = Instant::now();
        let expected_hover = if name == "branch-sort" {
            let column = [
                SortColumn::Name,
                SortColumn::Name,
                SortColumn::Activity,
                SortColumn::Activity,
                SortColumn::LastCommit,
                SortColumn::LastCommit,
            ][frame % 6];
            self.action(Action::BranchSort(column), window, cx);
            None
        } else if frame.is_multiple_of(2) {
            let app = &self.engine.app;
            let row = (0..app.list_rows)
                .map(|offset| app.scroll + (offset + frame * 7) % app.list_rows.max(1))
                .find(|&row| row < app.graph_len() && app.history_index(row).is_some())
                .expect("visible commit for hover");
            self.hover_commit(row, true, cx);
            Some(row)
        } else {
            self.hover_commit(previous_hover.unwrap(), false, cx);
            None
        };
        let handler_ms = start.elapsed().as_secs_f64() * 1000.;
        self.todo_advance(3);
        let run = self.todo_bench.run.as_mut().unwrap();
        run.handlers.push(handler_ms);
        run.frame_started = Some(start);
        run.expected_hover = expected_hover;
        cx.notify();
    }

    fn todo_record_frame(&mut self, slot: usize) {
        let run = self.todo_bench.run.as_mut().unwrap();
        // Stop the action-to-paint clock before assertions or auditing work.
        run.samples
            .push(run.frame_started.take().unwrap().elapsed().as_secs_f64() * 1000.);
        let paint = run.paints.borrow()[slot].unwrap();
        run.observations.push(Observation {
            frame: run.samples.len() - 1,
            slot,
            bounds: [
                paint.bounds.origin.x.into(),
                paint.bounds.origin.y.into(),
                paint.bounds.size.width.into(),
                paint.bounds.size.height.into(),
            ],
        });
    }

    fn todo_stash_tick(
        &mut self,
        name: &str,
        phase: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if phase == 1 && self.stashes_ready() && self.todo_painted(2, window) {
            assert_eq!(self.stash_entries().len(), 3);
            let ids = self.stash_entries().iter().map(|s| s.oid.clone()).collect();
            self.todo_bench.run.as_mut().unwrap().stash_ids = ids;
            self.todo_bench.run.as_mut().unwrap().checks += 2;
            if name == "stash-panel" {
                self.todo_finish();
                return;
            }
            let frozen = self.stash_entries()[0].clone();
            let action = if name == "stash-apply" {
                stashes::Action::Apply(frozen)
            } else {
                stashes::Action::Delete(frozen)
            };
            self.action(Action::Stashes(Box::new(action)), window, cx);
            self.todo_advance(2);
        } else if phase == 2 && self.operations.modal_open() && self.todo_painted(7, window) {
            self.todo_bench.run.as_mut().unwrap().operation_epoch = self.engine.operation_epoch;
            self.action(
                Action::Operation(Box::new(operations::Action::Confirm)),
                window,
                cx,
            );
            self.todo_advance(3);
        } else if phase == 3
            && self.stashes_ready()
            && !self.engine.app.pending.loading
            && self.engine.operation_epoch != self.todo_bench.run.as_ref().unwrap().operation_epoch
            && self.todo_painted(2, window)
        {
            assert!(
                !self.engine.operation_error,
                "{}",
                self.engine.operation_status
            );
            assert!(self.engine.operations.conflict.is_none());
            let run = self.todo_bench.run.as_ref().unwrap();
            let ids: Vec<_> = self.stash_entries().iter().map(|s| s.oid.clone()).collect();
            if name == "stash-apply" {
                if !self.engine.app.pending.exists() {
                    return;
                }
                assert_eq!(ids, run.stash_ids);
            } else {
                assert!(!self.engine.app.pending.exists());
                assert_eq!(ids, run.stash_ids[1..]);
            }
            self.todo_bench.run.as_mut().unwrap().checks += 4;
            self.todo_finish();
        }
    }

    fn todo_tools_tick(&mut self, phase: u8, window: &mut Window, cx: &mut Context<Self>) {
        if phase >= 5 {
            self.todo_tools_list_tick(phase, window, cx);
            return;
        }
        if phase == 1 && !self.engine.app.settings.busy {
            self.action(
                Action::SettingsTab(crate::settings::GENERAL_PAGE),
                window,
                cx,
            );
            self.todo_advance(2);
            return;
        }
        if phase == 2 && self.todo_painted(3, window) {
            if !self.tools_scenario_loaded()
                || !self.tools_scenario_scan_complete()
                || self.tools_config_busy()
            {
                return;
            }
            assert!(
                self.tools_uninitialized(),
                "startup configuration must not create editor inputs or expand General tools"
            );
            assert!(self.tools_scenario_scan_complete());
            let fixture = std::env::var_os("LOCALAPPDATA")
                .map(std::path::PathBuf::from)
                .and_then(|path| {
                    std::fs::read_to_string(path.join(".git-rustle-tools-list-fixture")).ok()
                })
                .is_some_and(|text| text == "disposable external tools list fixture");
            let entries = self.tools_scenario_entries();
            if fixture {
                assert_eq!(entries.len(), 2, "both controlled installations discovered");
                assert!(entries.iter().any(|(_, name)| name.contains("Code")));
                assert!(entries.iter().any(|(_, name)| name.contains("Araxis")));
            }
            let run = self.todo_bench.run.as_mut().unwrap();
            run.tool_list_fixture = fixture;
            run.tool_ids = entries.into_iter().map(|(id, _)| id).collect();
            self.todo_bench.run.as_mut().unwrap().checks += 1;
            self.engine.mark("tools list configure requested");
            self.action(
                Action::Tools(Box::new(external_tools::Action::Toggle)),
                window,
                cx,
            );
            if self.tools_scenario_entries().is_empty() {
                // Legacy geometry-only scenarios may have no installation.
                // Exercise the actual Add action, without ever saving a draft.
                self.action(
                    Action::Tools(Box::new(external_tools::Action::Add)),
                    window,
                    cx,
                );
            }
            self.todo_advance(3);
            return;
        }
        if !matches!(phase, 3 | 4) || !self.tools_ready() {
            return;
        }
        // on_next_frame runs before the new frame paints. Never advance the
        // epoch again while still waiting for the preceding scroll's paint.
        let panel_current = {
            let run = self.todo_bench.run.as_ref().unwrap();
            run.paints.borrow()[3].is_some_and(|paint| paint.epoch == run.epoch)
        };
        if !panel_current {
            return;
        }
        let slots = if phase == 3 { [9, 10] } else { [11, 12] };
        if !self.todo_painted(3, window)
            || slots.iter().any(|&slot| !self.todo_painted(slot, window))
        {
            self.todo_scroll_tools(phase, window, cx);
            return;
        }
        let run = self.todo_bench.run.as_ref().unwrap();
        let paints = run.paints.borrow();
        let panel = paints[3].unwrap().bounds;
        if phase == 4 && panel.bottom() > window.viewport_size().height {
            drop(paints);
            self.todo_scroll_tools(phase, window, cx);
            return;
        }
        let mut observed = Vec::new();
        for slot in slots {
            let bounds = paints[slot].unwrap().bounds;
            if bounds.size.width < px(300.) {
                let observed: Vec<_> = paints
                    .iter()
                    .enumerate()
                    .filter_map(|(slot, paint)| paint.map(|p| (slot, p.epoch, p.bounds)))
                    .collect();
                eprintln!(
                    "TODO external-tools layout phase={phase} viewport={:?} epoch={} scrolls={} panel={panel:?} painted={observed:?}",
                    window.viewport_size(),
                    run.epoch,
                    run.tool_scrolls
                );
            }
            assert!(
                bounds.size.width >= px(300.),
                "external-tool input slot{slot} width {:?}",
                bounds.size.width
            );
            let height = if slot >= 11 { 90. } else { 30. };
            assert!(
                bounds.size.height >= px(height),
                "external-tool input slot{slot} vertically compressed"
            );
            observed.push((
                slot - 9,
                [
                    f32::from(bounds.left() - panel.left()),
                    f32::from(bounds.top() - panel.top()),
                    bounds.size.width.into(),
                    bounds.size.height.into(),
                ],
            ));
        }
        drop(paints);
        let run = self.todo_bench.run.as_mut().unwrap();
        for (slot, bounds) in observed {
            run.tool_bounds[slot] = Some(bounds);
        }
        run.checks += 4;
        if phase == 3 {
            // The upper inputs are already verified. Bring the lower controls
            // and final Save row into view in one actual viewport update.
            self.todo_scroll_tools(4, window, cx);
            return;
        }
        let bounds = run
            .tool_bounds
            .map(|value| value.expect("all four actual tool inputs painted"));
        for pair in bounds.windows(2) {
            assert!(
                pair[0][1] + pair[0][3] <= pair[1][1],
                "external tool inputs overlap vertically: {bounds:?}"
            );
        }
        assert_eq!(self.engine.app.settings.page, crate::settings::GENERAL_PAGE);
        assert!(!self.external_merge_busy());
        // Save is reached by real scrolling; geometry-only callers never write.
        self.todo_bench.run.as_mut().unwrap().checks += 4;
        self.engine.mark("tools list configure painted");
        if self.todo_bench.run.as_ref().unwrap().tool_list_fixture {
            self.action(
                Action::Tools(Box::new(external_tools::Action::Add)),
                window,
                cx,
            );
            self.todo_advance(5);
        } else {
            self.todo_finish();
        }
    }

    fn todo_tools_list_tick(&mut self, phase: u8, window: &mut Window, cx: &mut Context<Self>) {
        assert!(self.todo_bench.run.as_ref().unwrap().tool_list_fixture);
        if self.tools_config_busy() {
            return;
        }
        match phase {
            5 if self.tools_ready() && self.todo_painted(3, window) => {
                let id = self.tools_scenario_selected().expect("new draft selected");
                assert!(!self.todo_bench.run.as_ref().unwrap().tool_ids.contains(&id));
                self.todo_bench.run.as_mut().unwrap().tool_custom_id = Some(id);
                let root = std::env::split_paths(
                    &std::env::var_os("GIT_RUSTLE_TOOL_DISCOVERY_ROOTS")
                        .expect("controlled first-scan roots"),
                )
                .next()
                .expect("controlled VS Code root");
                let executable = root.join("Code.exe");
                assert!(executable.is_file());
                for (index, value) in [
                    "Benchmark saved tool".to_string(),
                    executable.display().to_string(),
                    "--wait\n--diff\n{left}\n{right}".into(),
                    "--wait\n--merge\n{left}\n{right}\n{base}\n{output}".into(),
                ]
                .into_iter()
                .enumerate()
                {
                    self.tools_scenario_edit_input(index, &value, window, cx);
                }
                self.engine.mark("tools list save requested");
                self.action(
                    Action::Tools(Box::new(external_tools::Action::Save)),
                    window,
                    cx,
                );
                self.todo_advance(6);
            }
            6 if self.tools_ready() && self.todo_painted(3, window) => {
                let entries = self.tools_scenario_entries();
                let run = self.todo_bench.run.as_ref().unwrap();
                assert_eq!(entries.len(), run.tool_ids.len() + 1);
                assert!(entries.iter().any(|(id, name)| {
                    Some(id) == run.tool_custom_id.as_ref() && name == "Benchmark saved tool"
                }));
                let first = run.tool_ids[0].clone();
                self.todo_bench.run.as_mut().unwrap().checks += 3;
                self.engine.mark("tools list saved frame");
                self.engine.mark("tools list selection requested");
                self.action(
                    Action::Tools(Box::new(external_tools::Action::Select(first))),
                    window,
                    cx,
                );
                self.todo_advance(7);
            }
            7 if self.tools_ready() && self.todo_painted(3, window) => {
                let run = self.todo_bench.run.as_ref().unwrap();
                assert_eq!(
                    self.tools_scenario_selected().as_ref(),
                    run.tool_ids.first()
                );
                let custom = run.tool_custom_id.clone().unwrap();
                let selected_name = self
                    .tools_scenario_entries()
                    .into_iter()
                    .find(|(id, _)| Some(id) == run.tool_ids.first())
                    .unwrap()
                    .1;
                assert_eq!(self.tools_scenario_input(0, cx), selected_name);
                self.todo_bench.run.as_mut().unwrap().checks += 1;
                self.engine.mark("tools list selected frame");
                self.engine.mark("tools list remove requested");
                self.action(
                    Action::Tools(Box::new(external_tools::Action::Select(custom))),
                    window,
                    cx,
                );
                self.action(
                    Action::Tools(Box::new(external_tools::Action::Remove)),
                    window,
                    cx,
                );
                self.action(
                    Action::Tools(Box::new(external_tools::Action::Save)),
                    window,
                    cx,
                );
                self.todo_advance(8);
            }
            8 if self.tools_ready() && self.todo_painted(3, window) => {
                let ids: Vec<_> = self
                    .tools_scenario_entries()
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect();
                assert_eq!(ids, self.todo_bench.run.as_ref().unwrap().tool_ids);
                self.todo_bench.run.as_mut().unwrap().checks += 2;
                self.engine.mark("tools list removed frame");
                // The fixture started in Settings without a parked Diff. Open
                // an actual commit diff through the normal production actions.
                self.action(Action::Tab(Tab::Graph), window, cx);
                self.action(Action::Select(0, false), window, cx);
                self.action(Action::Diff, window, cx);
                self.todo_advance(9);
            }
            9 if matches!(&self.engine.app.mode, Mode::Diff(d) if d.current_change().is_some()) => {
                let change = match &self.engine.app.mode {
                    Mode::Diff(d) => d.current_change().unwrap().clone(),
                    _ => unreachable!(),
                };
                self.engine.mark("tools diff picker requested");
                self.action(
                    Action::Tools(Box::new(external_tools::Action::OpenDiff(change))),
                    window,
                    cx,
                );
                self.todo_advance(10);
            }
            10 if self.todo_painted(13, window) => {
                assert_eq!(
                    self.tools_scenario_picker_ids(),
                    self.todo_bench.run.as_ref().unwrap().tool_ids
                );
                assert!(!self.external_merge_busy());
                self.todo_bench.run.as_mut().unwrap().checks += 2;
                self.engine.mark("tools diff picker painted");
                self.select_focus.focus(window);
                self.todo_advance(11);
                // Dispatch outside this entity update so the production root
                // handler closes the picker before considering Diff navigation.
                window.defer(cx, |window, cx| {
                    assert!(window.dispatch_keystroke(Keystroke::parse("escape").unwrap(), cx));
                });
            }
            11 if self.todo_painted(5, window) => {
                assert!(self.tools_scenario_picker_ids().is_empty());
                assert!(matches!(self.engine.app.mode, Mode::Diff(_)));
                self.todo_bench.run.as_mut().unwrap().checks += 2;
                self.todo_finish();
            }
            _ => {}
        }
    }

    pub(super) fn todo_settings_scroll(&self) -> Option<ScrollHandle> {
        self.todo_bench.run.as_ref()?.settings_scroll.clone()
    }

    fn todo_scroll_tools(&mut self, phase: u8, window: &mut Window, cx: &mut Context<Self>) {
        let run = self.todo_bench.run.as_mut().unwrap();
        run.tool_scrolls += 1;
        let scroll = run
            .settings_scroll
            .as_ref()
            .expect("opt-in settings scroll handle")
            .clone();
        if run.tool_scrolls > 12 {
            let observed: Vec<_> = run
                .paints
                .borrow()
                .iter()
                .enumerate()
                .filter_map(|(slot, paint)| paint.map(|p| (slot, p.epoch, p.bounds)))
                .collect();
            panic!(
                "external-tool controls unreachable phase={phase} epoch={} offset={:?} max_offset={:?} viewport={:?} scroll_bounds={:?} painted={observed:?}",
                run.epoch,
                scroll.offset(),
                scroll.max_offset(),
                window.viewport_size(),
                scroll.bounds()
            );
        }
        let offset = scroll.offset();
        if phase == 4 {
            scroll.scroll_to_bottom();
        } else {
            scroll.set_offset(point(
                offset.x,
                (offset.y - px(240.)).max(-scroll.max_offset().height),
            ));
        }
        self.todo_advance(phase);
        cx.notify();
    }

    fn todo_storage_tick(&mut self, phase: u8, window: &mut Window, cx: &mut Context<Self>) {
        match phase {
            1 if !self.engine.app.settings.busy => {
                self.action(Action::SettingsTab(0), window, cx);
                self.todo_advance(2);
            }
            2 if self.todo_painted(4, window) => {
                assert!(
                    !self.ref_storage_ready(),
                    "conversion metadata must remain lazy before explicit Review"
                );
                self.action(Action::RefStorage(ref_storage::Action::Review), window, cx);
                self.todo_advance(3);
            }
            3 if self.ref_storage_ready()
                && self.ref_storage.confirming
                && self.todo_painted(8, window) =>
            {
                self.todo_bench.run.as_mut().unwrap().checks += 2;
                self.action(Action::RefStorage(ref_storage::Action::Confirm), window, cx);
                self.todo_advance(4);
            }
            4 if self.ref_storage_enabled()
                && self.engine.reference_storage_reftable()
                && !self.engine.operation_busy
                && !self.engine.app.want_refresh
                && !self.engine.app.pending.loading
                && self.todo_painted(4, window) =>
            {
                assert!(self.engine.operations.labels.is_empty());
                assert!(
                    self.engine.app.pending.exists(),
                    "conversion preserves dirty contents"
                );
                self.todo_bench.run.as_mut().unwrap().checks += 4;
                self.todo_finish();
            }
            _ => {}
        }
    }

    fn todo_finish(&mut self) {
        let run = self.todo_bench.run.as_mut().unwrap();
        assert_eq!(
            self.engine.app.head_id(),
            run.head,
            "TODO UI actions preserve HEAD"
        );
        run.checks += 1;
        if matches!(run.name, "branch-sort" | "graph-hover") {
            assert_eq!(run.samples.len(), FRAMES);
            assert_eq!(run.handlers.len(), FRAMES);
            assert_eq!(run.observations.len(), FRAMES);
            if let Some(path) = std::env::var_os("GIT_RUSTLE_FRAME_SAMPLES") {
                let path = std::path::PathBuf::from(path);
                std::fs::write(&path, serde_json::to_vec_pretty(&run.samples).unwrap()).unwrap();
                std::fs::write(
                    path.with_extension("todos-bounds.json"),
                    serde_json::to_vec_pretty(&run.observations).unwrap(),
                )
                .unwrap();
            }
            self.engine
                .mark("desktop scroll done (120 measured frames)");
        }
        run.verified = true;
        run.phase = 250;
    }

    pub(super) fn todo_scenario_ready(&self) -> Option<bool> {
        self.todo_bench.run.as_ref().map(|r| r.verified)
    }

    pub(super) fn todo_scenario_assert(&self) {
        let Some(run) = &self.todo_bench.run else {
            return;
        };
        assert!(run.verified && run.checks > 0);
        assert_eq!(self.engine.app.head_id(), run.head);
    }

    pub(super) fn todo_scenario_summary(&self) -> String {
        let Some(run) = &self.todo_bench.run else {
            return String::new();
        };
        format!(
            "todos_verified={} todos_checks={} todos_frames={} todos_phase={} todos_hover_rows_max={} todos_hover_cells_max={} todos_tool_scrolls={} todos_tool_list_verified={}\ntodo_handler_ms={}\ntodos_tool_input_bounds={}",
            run.verified,
            run.checks,
            run.samples.len(),
            run.phase,
            run.hover_rows_max,
            run.hover_cells_max,
            run.tool_scrolls,
            run.tool_list_fixture && run.verified,
            serde_json::to_string(&run.handlers).unwrap(),
            serde_json::to_string(&run.tool_bounds).unwrap()
        )
    }
}
