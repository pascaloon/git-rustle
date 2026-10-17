//! Native desktop presentation. Lists construct only visible rows.
use crate::desktop_icons::icon;
use crate::desktop_text::{Matches, TextBlock};
use crate::desktop_theme::*;
use crate::{app::Mode, diff, engine::Engine, graph::layout::CellKind};
use gpui::{prelude::*, *};
use gpui_component::{
    Root,
    input::{Input, InputEvent, InputState},
    scroll::{Scrollbar, ScrollbarShow},
};
use std::time::{Duration, Instant};

pub fn run(mut engine: Engine) {
    engine.mark("GPUI initialization requested");
    // Drive local startup while DirectX/DirectWrite initialize on the main thread.
    // No network work or status polling starts until the first native frame.
    let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop = ready.clone();
    let bootstrap = std::thread::Builder::new()
        .name("desktop-bootstrap".into())
        .spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Acquire) {
                engine.tick(None);
                std::thread::sleep(Duration::from_millis(1));
            }
            engine
        })
        .expect("start desktop bootstrap");
    let application = Application::new().with_assets(crate::desktop_icons::Assets);
    ready.store(true, std::sync::atomic::Ordering::Release);
    let mut engine = bootstrap.join().expect("desktop bootstrap");
    engine.mark("GPUI platform ready");
    application.run(move |cx: &mut gpui::App| {
        gpui_component::init(cx);
        crate::desktop_text::init(cx);
        engine.mark("GPUI components ready");
        crate::desktop_theme::apply(cx);
        let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Git Rustle".into()),
                    ..Default::default()
                }),
                window_min_size: Some(size(px(900.), px(600.))),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| Desktop::new(engine, window, cx));
                let weak = view.downgrade();
                window.on_window_should_close(cx, move |_, cx| {
                    weak.update(cx, |v, cx| {
                        if v.engine.app.pending.busy
                            || v.engine.app.branches.busy
                            || v.engine.app.settings.saving
                        {
                            v.notice = "Waiting for the active operation to finish…".into();
                            v.close_requested = true;
                            cx.notify();
                            false
                        } else {
                            true
                        }
                    })
                    .unwrap_or(true)
                });
                cx.new(|cx| Root::new(view, window, cx))
            },
        )
        .expect("open GPUI window");
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Graph,
    Branches,
    Diff,
    Settings,
}

#[derive(Clone)]
enum Action {
    Tab(Tab),
    Wrap,
    Explorer,
    Terminal,
    Refresh,
    Head,
    Branches,
    Settings,
    Diff,
    Checkout,
    Commit,
    CopyDetails,
    CheckAll(bool),
    Select(usize, bool),
    SearchField(&'static str),
    Branch(usize),
    JumpBranch,
    File(usize),
    Check(usize),
    Expand(bool),
    Confirm(usize),
    CancelCheckout,
    SettingsTab(usize),
    SettingsRemote(usize),
    TestSettings,
    SaveSettings,
    ForgetToken,
}

struct Desktop {
    engine: Engine,
    search: Entity<InputState>,
    search_help: bool,
    matches: Matches,
    branch_matches: Matches,
    selected_commits: crate::desktop_selection::Commits,
    branch_filter: Entity<InputState>,
    description: Entity<InputState>,
    new_branch: Entity<InputState>,
    api: Entity<InputState>,
    project: Entity<InputState>,
    token: Entity<InputState>,
    graph_scroll: UniformListScrollHandle,
    diff_files_scroll: UniformListScrollHandle,
    context_menu: Option<Point<Pixels>>,
    settings_loaded: bool,
    branches_loaded: bool,
    graph_width: f32,
    resizing_graph: bool,
    word_wrap: bool,
    wrap_anchor: Option<usize>,
    wrap_layout: crate::desktop_wrap::Layout,
    files_scroll: UniformListScrollHandle,
    branches_scroll: UniformListScrollHandle,
    diff_scroll: UniformListScrollHandle,
    diff_horizontal: crate::desktop_scroll::DiffScroll,
    hovered_scroll: [bool; 4],
    notice: String,
    close_requested: bool,
    settings_identity: Option<(u32, usize, usize)>,
    draft_text: String,
    _subscriptions: Vec<Subscription>,
    _task: Option<Task<()>>,
    detail_id: Option<gix::ObjectId>,
    detail_text: Entity<TextBlock>,
    diff_path: Entity<TextBlock>,
    pane_width: f32,
    resizing: bool,
    diff_sidebar: f32,
    resizing_diff: bool,
    viewport_rows: [usize; 4],
    scenario: Option<String>,
    scenario_started: bool,
    scenario_presented: bool,
    scroll_step: usize,
    frame_samples: Vec<f64>,
    frame_start: Option<Instant>,
    selection: std::rc::Rc<std::cell::RefCell<crate::selectable::Selection>>,
    select_focus: FocusHandle,
}

fn column() -> Div {
    div().flex().flex_col().min_h_0().min_w_0()
}
fn row() -> Div {
    div().flex().items_center().min_w_0().gap_2()
}
fn caption(s: impl Into<SharedString>) -> Div {
    div()
        .text_color(rgb(MUTED))
        .text_size(px(12.))
        .child(s.into())
}

impl Desktop {
    fn tabs(&self, cx: &Context<Self>) -> AnyElement {
        let app = &self.engine.app;
        let current = if app.settings.open {
            Tab::Settings
        } else if app.branches.open {
            Tab::Branches
        } else if matches!(app.mode, Mode::Diff(_)) {
            Tab::Diff
        } else {
            Tab::Graph
        };
        let has_diff = app.parked_diff.is_some() || matches!(app.mode, Mode::Diff(_));
        row()
            .h(px(38.))
            .flex_shrink_0()
            .gap_0()
            .px_3()
            .border_b_1()
            .border_color(rgb(LINE))
            .children(
                [
                    (Tab::Graph, "graph", "Git Graph"),
                    (Tab::Branches, "branch", "Branches"),
                    (Tab::Diff, "diff", "Diff"),
                    (Tab::Settings, "settings", "Settings"),
                ]
                .into_iter()
                .filter(|(t, _, _)| *t != Tab::Diff || has_diff)
                .enumerate()
                .map(|(i, (tab, name, label))| {
                    row()
                        .id(("tab", i))
                        .h_full()
                        .px_4()
                        .cursor_pointer()
                        .border_b_2()
                        .border_color(rgb(if current == tab { ACCENT } else { BG }))
                        .bg(rgb(if current == tab { PANEL } else { BG }))
                        .hover(|s| s.bg(rgb(HOVER)))
                        .child(icon(name))
                        .child(label)
                        .on_click(cx.listener(move |v, _, w, cx| v.action(Action::Tab(tab), w, cx)))
                }),
            )
            .into_any_element()
    }
    fn commit_menu(
        &self,
        position: Point<Pixels>,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let app = &self.engine.app;
        let count = self.selected_commits.ids(app.selected_id()).len();
        let enabled_diff = app.pending.selected || matches!(count, 1 | 2);
        let enabled_checkout = !app.pending.selected
            && self
                .selected_commits
                .checkout_target(app.selected_id())
                .is_some();
        let p = point(
            position
                .x
                .min(window.viewport_size().width - px(224.))
                .max(px(0.)),
            position
                .y
                .min(window.viewport_size().height - px(88.))
                .max(px(0.)),
        );
        deferred(
            column()
                .id("commit-menu")
                .absolute()
                .left(p.x)
                .top(p.y)
                .w(px(224.))
                .p_1()
                .bg(rgb(PANEL))
                .border_1()
                .border_color(rgb(LINE))
                .rounded_md()
                .shadow_lg()
                .occlude()
                .on_mouse_down_out(cx.listener(|v, _, _, cx| {
                    v.context_menu = None;
                    cx.notify();
                }))
                .children(
                    [
                        (
                            Action::Diff,
                            "diff",
                            if app.pending.selected {
                                "Diff pending changes"
                            } else if count > 1 {
                                "Compare selected commits"
                            } else {
                                "Diff commit"
                            },
                            enabled_diff,
                        ),
                        (Action::Checkout, "checkout", "Checkout…", enabled_checkout),
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(i, (action, name, label, enabled))| {
                        row()
                            .id(("commit-menu-item", i))
                            .h(px(36.))
                            .px_2()
                            .rounded_sm()
                            .text_color(rgb(if enabled { TEXT } else { MUTED }))
                            .child(icon(name).text_color(rgb(if enabled { TEXT } else { MUTED })))
                            .child(label)
                            .when(enabled, |s| {
                                s.cursor_pointer()
                                    .hover(|s| s.bg(rgb(SELECTED_HOVER)))
                                    .on_click(cx.listener(move |v, _, w, cx| {
                                        cx.stop_propagation();
                                        v.action(action.clone(), w, cx);
                                    }))
                            })
                    }),
                ),
        )
        .with_priority(3)
        .into_any_element()
    }

    fn new(mut engine: Engine, window: &mut Window, cx: &mut Context<Self>) -> Self {
        engine.mark("GPUI window ready");
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search commits…"));
        let branch_filter =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter branches…"));
        let description = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .placeholder("Commit description")
        });
        let detail_text = cx.new(TextBlock::new);
        let diff_path = cx.new(TextBlock::new);
        let new_branch =
            cx.new(|cx| InputState::new(window, cx).placeholder("New branch (optional)"));
        let api = cx.new(|cx| InputState::new(window, cx).placeholder("API URL"));
        let project = cx.new(|cx| InputState::new(window, cx).placeholder("Project path"));
        let token = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("Access token")
        });
        let subscriptions = vec![
            cx.subscribe(&search, |v, state, event, cx| {
                if matches!(event, InputEvent::Change) {
                    v.engine.app.search.text.value = state.read(cx).value().to_string();
                    v.matches = Matches::new(&v.engine.app.search.text.value);
                    v.selected_commits.clear();
                    v.engine.app.search.changed();
                    v.changed(cx);
                }
                match event {
                    InputEvent::Focus => {
                        v.search_help = true;
                        cx.notify();
                    }
                    InputEvent::Blur | InputEvent::PressEnter { .. } => {
                        v.search_help = false;
                        cx.notify();
                    }
                    _ => {}
                }
            }),
            cx.subscribe(&branch_filter, |v, state, event, cx| {
                if matches!(event, InputEvent::Change) {
                    v.engine.app.branches.query = state.read(cx).value().to_string();
                    v.branch_matches = Matches::new(&v.engine.app.branches.query);
                    v.engine.app.branches.filter();
                    v.engine.app.branches.select(0);
                    v.changed(cx);
                }
            }),
            cx.subscribe(&description, |v, state, event, cx| {
                if matches!(event, InputEvent::Change) {
                    v.engine.app.pending.description.value = state.read(cx).value().to_string();
                    v.draft_text = v.engine.app.pending.description.value.clone();
                }
            }),
            cx.subscribe(&new_branch, |v, state, event, cx| {
                if matches!(event, InputEvent::Change) {
                    v.engine.app.pending.branch.value = state.read(cx).value().to_string();
                }
            }),
        ];
        let messages = engine.messages();
        let task = cx.spawn_in(window, async move |view, cx| {
            loop {
                let message =
                    futures_lite::future::or(async { messages.recv().await.ok() }, async {
                        Timer::after(Duration::from_millis(250)).await;
                        None
                    })
                    .await;
                if view
                    .update_in(cx, |v, window, cx| {
                        let old = v.engine.app.graph_selected();
                        let saving = v.engine.app.settings.saving;
                        v.engine.tick(message);
                        if saving && !v.engine.app.settings.saving {
                            v.settings_identity = None;
                        }
                        if old != v.engine.app.graph_selected() {
                            v.graph_scroll.scroll_to_item(
                                v.engine.app.graph_selected(),
                                ScrollStrategy::Center,
                            );
                        }
                        v.sync_inputs(window, cx);
                        let dragging = v.selection.borrow().dragging;
                        if dragging {
                            v.scroll_selection(window, cx);
                        }
                        if !v.scenario_started && v.engine.autoquit_ready(Duration::from_millis(20))
                        {
                            v.scenario_started = true;
                            match v.scenario.as_deref() {
                                Some("scroll" | "graph-resize") => {
                                    v.scroll_step = 1;
                                    v.frame_start = Some(Instant::now());
                                    v.graph_scroll.scroll_to_item(0, ScrollStrategy::Top);
                                    cx.notify();
                                }
                                Some("pending-diff") => v.action(Action::Diff, window, cx),
                                Some("branches") => v.action(Action::Branches, window, cx),
                                Some(
                                    "diff" | "diff-scroll" | "diff-pan" | "wrap-scroll" | "tabs",
                                ) => {
                                    v.action(Action::Head, window, cx);
                                    v.action(Action::Diff, window, cx);
                                }
                                Some("search" | "search-scroll") => {
                                    v.search
                                        .update(cx, |s, cx| s.set_value("m: merge", window, cx));
                                }
                                Some("settings") => v.action(Action::Settings, window, cx),
                                Some("compare") => {
                                    v.action(Action::Select(0, false), window, cx);
                                    v.action(Action::Select(1, true), window, cx);
                                    v.action(Action::Diff, window, cx);
                                }
                                Some("compare-menu") => {
                                    v.action(Action::Select(0, false), window, cx);
                                    v.action(Action::Select(1, true), window, cx);
                                    v.action(Action::Checkout, window, cx);
                                    assert!(v.engine.app.checkout_confirmation.is_none());
                                    v.context_menu = Some(point(px(220.), px(210.)));
                                    cx.notify();
                                }
                                _ => {}
                            }
                        }
                        if v.scenario_started && v.scroll_step == 0 {
                            let ready = match v.scenario.as_deref() {
                                Some("search-scroll") => v
                                    .engine
                                    .app
                                    .search
                                    .result
                                    .as_ref()
                                    .is_some_and(|r| r.done && !r.rows.is_empty()),
                                Some("files-scroll") => v
                                    .engine
                                    .app
                                    .details_changes
                                    .as_ref()
                                    .is_some_and(|c| c.loaded && !c.tree.visible.is_empty()),
                                Some("diff-scroll" | "diff-pan" | "wrap-scroll" | "tabs") => {
                                    matches!(&v.engine.app.mode,
                                    Mode::Diff(d) if d.diff.is_some())
                                }
                                _ => false,
                            };
                            if ready {
                                if v.scenario.as_deref() == Some("wrap-scroll") {
                                    v.action(Action::Wrap, window, cx);
                                }
                                v.scroll_step = 1;
                                v.frame_start = Some(Instant::now());
                                cx.notify();
                            }
                        }
                        if v.engine.app.needs_redraw {
                            v.engine.app.needs_redraw = false;
                            cx.notify();
                        }
                        let autoquit = std::env::var("GIT_RUSTLE_AUTOQUIT")
                            .ok()
                            .and_then(|s| s.parse().ok());
                        if autoquit.is_some_and(|ms| {
                            v.engine.autoquit_ready(Duration::from_millis(ms))
                                && (v.scenario.is_none() || v.scenario_presented)
                        }) || (v.close_requested
                            && !v.engine.app.pending.busy
                            && !v.engine.app.branches.busy
                            && !v.engine.app.settings.saving)
                        {
                            if let Some(path) = std::env::var_os("GIT_RUSTLE_DUMP") {
                                let _ = std::fs::write(path, v.snapshot());
                            }
                            cx.quit();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let diff_scroll = UniformListScrollHandle::new();
        Self {
            engine,
            search,
            search_help: false,
            matches: Matches::default(),
            branch_matches: Matches::default(),
            selected_commits: Default::default(),
            branch_filter,
            description,
            new_branch,
            api,
            project,
            token,
            graph_scroll: UniformListScrollHandle::new(),
            diff_files_scroll: UniformListScrollHandle::new(),
            context_menu: None,
            settings_loaded: false,
            branches_loaded: false,
            graph_width: 84.,
            resizing_graph: false,
            word_wrap: false,
            wrap_anchor: None,
            wrap_layout: Default::default(),
            files_scroll: UniformListScrollHandle::new(),
            branches_scroll: UniformListScrollHandle::new(),
            diff_horizontal: crate::desktop_scroll::DiffScroll::new(diff_scroll.clone()),
            diff_scroll,
            hovered_scroll: [false; 4],
            notice: String::new(),
            close_requested: false,
            settings_identity: None,
            draft_text: String::new(),
            _subscriptions: subscriptions,
            _task: Some(task),
            detail_id: None,
            detail_text,
            diff_path,
            pane_width: 470.,
            resizing: false,
            diff_sidebar: 320.,
            resizing_diff: false,
            viewport_rows: [0; 4],
            scenario: std::env::var("GIT_RUSTLE_SCENARIO").ok(),
            scenario_started: false,
            scenario_presented: false,
            scroll_step: 0,
            frame_samples: Vec::new(),
            frame_start: None,
            selection: Default::default(),
            select_focus: cx.focus_handle(),
        }
    }
    fn sync_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let app = &self.engine.app;
        if app.pending.description.value != self.draft_text {
            self.draft_text = app.pending.description.value.clone();
            self.description
                .update(cx, |s, cx| s.set_value(self.draft_text.clone(), window, cx));
            self.new_branch.update(cx, |s, cx| {
                s.set_value(app.pending.branch.value.clone(), window, cx)
            });
        }
        if app.settings.open && !app.settings.busy {
            let key = (
                app.settings.session,
                app.settings.tab,
                app.settings.selected[app.settings.tab],
            );
            if self.settings_identity != Some(key) {
                self.settings_identity = Some(key);
                if let Some(d) = app.settings.current() {
                    self.api.update(cx, |s, cx| {
                        s.set_value(d.config.api_url.clone().unwrap_or_default(), window, cx)
                    });
                    self.project.update(cx, |s, cx| {
                        s.set_value(d.config.project.clone().unwrap_or_default(), window, cx)
                    });
                    self.token
                        .update(cx, |s, cx| s.set_value(d.token.clone(), window, cx));
                }
            }
        }
        let details = app
            .details
            .as_ref()
            .filter(|d| Some(d.id) == app.selected_id());
        let id = details.map(|d| d.id);
        if id != self.detail_id {
            self.detail_id = id;
            let document = details
                .map(crate::desktop_details::document)
                .unwrap_or_default();
            self.detail_text.update(cx, |s, cx| {
                s.set_document(document, &app.search.text.value, cx)
            });
        }
        self.detail_text
            .update(cx, |s, cx| s.set_query(&app.search.text.value, cx));
        let path = match &app.mode {
            Mode::Diff(d) => d
                .current_change()
                .map(|c| c.path.clone())
                .unwrap_or_default(),
            _ => String::new(),
        };
        self.diff_path.update(cx, |s, cx| s.set(path, "", cx));
    }
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.engine.app.needs_redraw = true;
        self.engine.tick(None);
        cx.notify();
    }
    fn scroll_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let bounds = self.diff_horizontal.bounds.get();
        let pointer = window.mouse_position();
        if pointer.x < bounds.left() || pointer.x > bounds.right() {
            return;
        }
        let down = pointer.y >= bounds.bottom();
        if !down && pointer.y >= bounds.top() {
            return;
        }
        let Mode::Diff(view) = &self.engine.app.mode else {
            return;
        };
        let Some(diff) = &view.diff else { return };
        let mut s = self.selection.borrow_mut();
        let Some((id, right)) = s.id else { return };
        if id != view.id || diff.rows.is_empty() {
            return;
        }
        let total = if self.word_wrap {
            self.wrap_layout.rows.len()
        } else {
            diff.rows.len()
        };
        let top =
            (-f32::from(self.diff_scroll.0.borrow().base_handle.offset().y) / 24.).max(0.) as usize;
        let top = if down {
            top.saturating_add(3)
        } else {
            top.saturating_sub(3)
        }
        .min(total.saturating_sub(1));
        self.diff_scroll.scroll_to_item(top, ScrollStrategy::Top);
        let end = if down {
            top + (f32::from(bounds.size.height) / 24.).floor().max(1.) as usize - 1
        } else {
            top
        }
        .min(total.saturating_sub(1));
        let wrapped = self.word_wrap.then(|| &self.wrap_layout.rows[end]);
        let source = wrapped.map_or(end, |r| r.source);
        if let diff::Row::Line { left, right: r } = &diff.rows[source]
            && let Some(line) = if right { r.as_ref() } else { left.as_ref() }
        {
            let span = wrapped.and_then(|r| r.sides[usize::from(right)].as_ref());
            s.end = (
                source,
                if down {
                    span.map_or(line.text.len(), |r| r.end)
                } else {
                    span.map_or(0, |r| r.start)
                },
            );
        }
        cx.notify();
    }
    fn action(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(action, Action::SettingsTab(_) | Action::SettingsRemote(_))
            && self.settings_identity.is_some()
        {
            let s = &mut self.engine.app.settings;
            if let Some(d) = s.drafts[s.tab].get_mut(s.selected[s.tab]) {
                d.config.api_url = Some(self.api.read(cx).value().to_string());
                d.config.project = Some(self.project.read(cx).value().to_string());
                d.token = self.token.read(cx).value().to_string();
            }
        }
        if matches!(action, Action::File(_) | Action::Diff) {
            *self.selection.borrow_mut() = Default::default();
            self.diff_horizontal.set_x(px(0.));
        }
        self.context_menu = None;
        let app = &mut self.engine.app;
        if matches!(
            action,
            Action::Select(_, _) | Action::Head | Action::JumpBranch
        ) {
            app.begin_navigation();
        }
        match action {
            Action::Refresh => {
                app.want_refresh = true;
                app.pending.want_refresh = true;
                app.branches.want_refresh = Some(true);
            }
            Action::Head => {
                self.selected_commits.clear();
                if app.settings.saving {
                    return;
                }
                app.settings.open = false;
                app.branches.open = false;
                app.park_diff();
                app.clear_search();
                if let Some(id) = app.head_id()
                    && let Some(i) = app.history.index_of(&id)
                {
                    app.select(i);
                }
                self.search.update(cx, |s, cx| s.set_value("", window, cx));
            }
            Action::Branches | Action::Tab(Tab::Branches) => {
                app.park_diff();
                app.settings.open = false;
                if !self.branches_loaded {
                    app.branches.show(app.refs.as_ref());
                    self.branches_loaded = true;
                } else {
                    app.branches.open = true;
                }
            }
            Action::Settings | Action::Tab(Tab::Settings) => {
                app.park_diff();
                app.branches.open = false;
                if !self.settings_loaded {
                    app.settings.show();
                    self.settings_loaded = true;
                } else {
                    app.settings.open = true;
                }
            }
            Action::Tab(tab) => {
                app.settings.open = false;
                app.branches.open = false;
                app.branches.pending_jump = None;
                if tab == Tab::Diff {
                    app.resume_diff();
                } else {
                    app.park_diff();
                }
            }
            Action::Wrap => {
                let top = (-f32::from(self.diff_scroll.0.borrow().base_handle.offset().y) / 24.)
                    .max(0.) as usize;
                self.wrap_anchor = Some(if self.word_wrap {
                    self.wrap_layout.source(top)
                } else {
                    top
                });
                self.word_wrap = !self.word_wrap;
                self.diff_horizontal.set_x(px(0.));
            }
            Action::Explorer | Action::Terminal => {
                let terminal = matches!(action, Action::Terminal);
                let path = self.engine.repository_path();
                let weak = cx.entity().downgrade();
                cx.spawn(async move |_, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move { crate::desktop_shell::open(&path, terminal) })
                        .await;
                    if let Err(error) = result {
                        let _ = weak.update(cx, |v, cx| {
                            v.notice = error;
                            cx.notify();
                        });
                    }
                })
                .detach();
                return;
            }
            Action::Select(i, control) => {
                let id = app.history_index(i).map(|i| app.history.commits[i].id);
                self.selected_commits.click(id, app.selected_id(), control);
                app.select_graph(i);
            }
            Action::SearchField(prefix) => {
                let text = self.search.read(cx).value().to_string();
                let text = text.trim();
                let value = if matches!(text.get(..2), Some("m:" | "a:" | "s:" | "f:")) {
                    text[2..].trim_start()
                } else {
                    text
                };
                self.search.update(cx, |s, cx| {
                    s.set_value(format!("{prefix}{value}"), window, cx);
                    s.focus(window, cx);
                });
                self.search_help = false;
            }
            Action::Diff => {
                if app.pending.selected {
                    app.open_diff();
                } else {
                    app.open_selected_diff(&self.selected_commits.ids(app.selected_id()));
                }
                self.diff_scroll.scroll_to_item(0, ScrollStrategy::Top);
            }
            Action::Checkout => {
                if !app.pending.selected
                    && let Some(id) = self.selected_commits.checkout_target(app.selected_id())
                {
                    app.confirm_commit_checkout(id);
                }
            }
            Action::Commit => app.request_commit(),
            Action::CopyDetails => {
                cx.write_to_clipboard(ClipboardItem::new_string(
                    self.detail_text.read(cx).text().to_owned(),
                ));
            }
            Action::CheckAll(value) => {
                if !app.pending.busy
                    && let Some(cs) = app.active_changes_mut()
                {
                    cs.set_all_checked(value);
                }
            }
            Action::Check(node) => {
                if !app.pending.busy
                    && let Some(cs) = app.active_changes_mut()
                {
                    cs.toggle_checked(node);
                }
            }
            Action::Expand(value) => {
                if let Some(cs) = app.active_changes_mut() {
                    cs.tree.set_all_expanded(value);
                }
            }
            Action::File(node) => {
                if let Some(cs) = app.active_changes_mut() {
                    if cs.tree.nodes[node].is_folder() {
                        cs.tree.toggle(node);
                    } else if let Some(file) = cs.tree.nodes[node].file {
                        if matches!(app.mode, Mode::Graph) {
                            app.open_diff();
                        }
                        if let Some(v) = app.diff_view_mut() {
                            v.show(Some(file));
                        }
                        self.diff_scroll.scroll_to_item(0, ScrollStrategy::Top);
                    }
                }
            }
            Action::Branch(i) => app.branches.select(i),
            Action::JumpBranch => {
                self.selected_commits.clear();
                if let Some(target) = app.branches.chosen().map(|e| e.target.clone())
                    && let Ok(id) = gix::ObjectId::from_hex(target.sha.as_bytes())
                {
                    app.clear_search();
                    if let Some(i) = app.history.index_of(&id) {
                        app.select(i);
                        app.branches.open = false;
                    } else if target
                        .remote
                        .as_ref()
                        .is_some_and(|r| app.branches.remotes.contains_key(r))
                    {
                        app.branches
                            .preview_targets
                            .retain(|t| t.remote != target.remote || t.name != target.name);
                        app.branches.preview_targets.push(target);
                        app.branches.pending_jump = Some(id);
                        app.branches.preview_requested = true;
                        app.branches.status = "Loading remote history…".into();
                    }
                    self.search.update(cx, |s, cx| s.set_value("", window, cx));
                }
            }
            Action::CancelCheckout => app.checkout_confirmation = None,
            Action::Confirm(i) => {
                if !app.branches.busy
                    && !app.pending.busy
                    && let Some(dialog) = app.checkout_confirmation.take()
                {
                    app.checkout_request = dialog.choices.get(i).cloned();
                }
            }
            Action::SettingsTab(i) => {
                if !app.settings.busy {
                    app.settings.tab = i;
                }
            }
            Action::SettingsRemote(i) => {
                if !app.settings.busy {
                    app.settings.selected[app.settings.tab] = i;
                }
            }
            Action::TestSettings | Action::SaveSettings => {
                let tab = app.settings.tab;
                if let Some(d) = app.settings.drafts[tab].get_mut(app.settings.selected[tab]) {
                    d.config.api_url = Some(self.api.read(cx).value().to_string());
                    d.config.project = Some(self.project.read(cx).value().to_string());
                    d.token = self.token.read(cx).value().to_string();
                }
                app.settings.action(matches!(action, Action::SaveSettings));
            }
            Action::ForgetToken => {
                let t = app.settings.tab;
                if let Some(d) = app.settings.drafts[t].get_mut(app.settings.selected[t]) {
                    d.forget = !d.forget;
                }
            }
        }
        if matches!(action, Action::Head | Action::JumpBranch) {
            self.graph_scroll
                .scroll_to_item(app.graph_selected(), ScrollStrategy::Center);
        }
        self.changed(cx);
        self.sync_inputs(window, cx);
    }
    fn button(
        &self,
        id: &'static str,
        text: impl Into<SharedString>,
        action: Action,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let icon_name = match action {
            Action::Refresh => Some("refresh"),
            Action::Settings => Some("settings"),
            Action::Wrap => Some("wrap"),
            Action::Explorer => Some("folder"),
            Action::Terminal => Some("terminal"),
            Action::Head => Some("head"),
            Action::Branches => Some("branch"),
            Action::Diff => Some("diff"),
            Action::Checkout => Some("checkout"),
            Action::CopyDetails => Some("copy"),
            Action::Expand(true) => Some("expand"),
            Action::Expand(false) => Some("collapse"),

            _ => None,
        };
        let text: SharedString = text.into();
        let enabled = !matches!(action, Action::Diff)
            || self.engine.app.pending.selected
            || matches!(
                self.selected_commits
                    .ids(self.engine.app.selected_id())
                    .len(),
                1 | 2
            );
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .gap_2()
            .flex_shrink_0()
            .px_2()
            .h(px(30.))
            .rounded_md()
            .bg(rgb(if matches!(action, Action::Wrap) && self.word_wrap {
                SELECTED
            } else {
                CONTROL
            }))
            .text_color(rgb(if enabled { TEXT } else { MUTED }))
            .when(enabled, |s| {
                s.cursor_pointer().hover(|s| s.bg(rgb(SELECTED_HOVER)))
            })
            .when_some(icon_name, |s, n| s.child(icon(n)))
            .when_some(icon_name, |s, n| {
                s.tooltip(move |w, cx| {
                    gpui_component::tooltip::Tooltip::new(match n {
                        "settings" => "Settings",
                        "refresh" => "Refresh",
                        "wrap" => "Toggle word wrap",
                        "folder" => "Open in Explorer",
                        "terminal" => "Open in Windows Terminal",
                        "expand" => "Expand folders",
                        "collapse" => "Collapse folders",
                        "diff" => "Diff selected commits (one or two)",
                        "head" => "Go to HEAD",
                        "checkout" => "Checkout",
                        "copy" => "Copy commit details",
                        "branch" => "Branches",
                        _ => "Next file",
                    })
                    .build(w, cx)
                })
            })
            .when(!text.is_empty(), |s| s.child(text))
            .when(enabled, |s| {
                s.on_click(
                    cx.listener(move |v, _, window, cx| v.action(action.clone(), window, cx)),
                )
            })
    }
    fn graph(&self, cx: &mut Context<Self>) -> AnyElement {
        let n = self.engine.app.graph_len();
        let list = uniform_list(
            "history",
            n,
            cx.processor(|v, range: std::ops::Range<usize>, _, cx| {
                v.viewport_rows[0] = v.viewport_rows[0].max(range.len());
                v.engine.app.list_rows = range.len().max(1);
                v.engine.app.scroll = range.start;
                let app = &v.engine.app;
                let graph_width = v.graph_width;
                range
                    .map(|row_index| {
                        let selected =
                            app.history_index(row_index)
                                .map_or(app.pending.selected, |i| {
                                    v.selected_commits
                                        .contains(app.history.commits[i].id, app.selected_id())
                                });
                        let mut r = row()
                            .id(("commit", row_index))
                            .w_full()
                            .h(px(32.))
                            .px_2()
                            .gap_2()
                            .bg(rgb(if selected { SELECTED } else { BG }))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(HOVER)))
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |v, e: &MouseDownEvent, w, cx| {
                                    let id = v
                                        .engine
                                        .app
                                        .history_index(row_index)
                                        .map(|i| v.engine.app.history.commits[i].id);
                                    let selected = id.map_or(v.engine.app.pending.selected, |id| {
                                        v.selected_commits.contains(id, v.engine.app.selected_id())
                                    });
                                    if !selected {
                                        v.action(Action::Select(row_index, false), w, cx);
                                    }
                                    v.context_menu = Some(e.position);
                                    cx.stop_propagation();
                                    cx.notify();
                                }),
                            )
                            .on_click(cx.listener(move |v, e: &ClickEvent, window, cx| {
                                v.action(
                                    Action::Select(row_index, e.modifiers().control),
                                    window,
                                    cx,
                                );
                                if e.click_count() == 2 && !e.modifiers().control {
                                    v.action(Action::Diff, window, cx);
                                }
                            }));
                        if let Some(i) = app.history_index(row_index) {
                            let rec = app.history.commits[i];
                            let ghost = app.ghosts.contains_key(&rec.id);
                            let cells = if app.search.active() {
                                app.search
                                    .result
                                    .as_ref()
                                    .and_then(|s| s.graph.get(row_index))
                                    .map(|r| r.0.clone())
                                    .unwrap_or_default()
                            } else {
                                app.history.row_cells(i).to_vec()
                            };
                            let flags = if app.search.active() {
                                app.search
                                    .result
                                    .as_ref()
                                    .and_then(|r| r.graph.get(row_index))
                                    .map(|r| r.2.clone())
                                    .unwrap_or_default()
                            } else {
                                app.history
                                    .remote_cells
                                    .get(rec.row.range())
                                    .unwrap_or_default()
                                    .to_vec()
                            };
                            r = r.child(graph_canvas(cells, flags, graph_width)).child(
                                div()
                                    .w(px(76.))
                                    .flex_shrink_0()
                                    .text_color(rgb(if ghost { MUTED } else { ACCENT }))
                                    .child(v.matches.styled(rec.id.to_hex_with_len(8).to_string())),
                            );
                            if app.head_id() == Some(rec.id) {
                                r = r.child(div().text_color(rgb(ACCENT)).child("HEAD"));
                            }
                            if let Some(labels) = app.labels.get(&rec.id) {
                                for label in
                                    labels.iter().filter(|label| label.name != "HEAD").take(3)
                                {
                                    r = r.child(
                                        div()
                                            .rounded_sm()
                                            .px_1()
                                            .bg(rgb(SELECTED))
                                            .text_color(rgb(SEARCH_TEXT))
                                            .max_w(px(170.))
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .child(v.matches.styled(label.name.clone())),
                                    );
                                }
                            }
                            r = r
                                .child(
                                    div()
                                        .flex_1()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_color(rgb(if ghost { MUTED } else { TEXT }))
                                        .child(v.matches.styled(app.history.subject(i).to_owned())),
                                )
                                .child(
                                    div()
                                        .w(px(150.))
                                        .flex_shrink_0()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_color(rgb(MUTED))
                                        .child(v.matches.styled(app.history.author(i).to_owned())),
                                )
                                .child(
                                    div()
                                        .w(px(62.))
                                        .flex_shrink_0()
                                        .text_right()
                                        .text_size(px(12.))
                                        .text_color(rgb(MUTED))
                                        .child(crate::ui::time::format_relative(
                                            rec.commit_time,
                                            crate::ui::time::now_unix(),
                                        )),
                                );
                        } else {
                            r = r
                                .child(div().w(px(graph_width)).text_color(rgb(ACCENT)).child("●"))
                                .child(format!(
                                    "Pending changes  ·  {} files",
                                    app.pending.snapshot.files.len()
                                ));
                        }
                        r
                    })
                    .collect()
            }),
        )
        .track_scroll(self.graph_scroll.clone())
        .size_full();
        column()
            .flex_1()
            .h_full()
            .child(
                row()
                    .h(px(48.))
                    .flex_shrink_0()
                    .px_3()
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(caption(format!(
                        "{} commits",
                        self.engine
                            .app
                            .search
                            .result
                            .as_ref()
                            .map_or(self.engine.app.history.len(), |r| r.rows.len())
                    )))
                    .when(
                        self.selected_commits
                            .ids(self.engine.app.selected_id())
                            .len()
                            > 1,
                        |s| {
                            s.child(caption(format!(
                                "{} selected",
                                self.selected_commits
                                    .ids(self.engine.app.selected_id())
                                    .len()
                            )))
                        },
                    )
                    .child(self.search_box(cx))
                    .child(self.button("head", "HEAD", Action::Head, cx))
                    .child(self.button("refresh", "", Action::Refresh, cx)),
            )
            .child(
                column()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(self.scroll_region(0, list, Scrollbar::vertical(&self.graph_scroll), cx))
                    .child(
                        div()
                            .id("graph-column-divider")
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(px(self.graph_width + 8.))
                            .w(px(5.))
                            .cursor_col_resize()
                            .bg(rgb(LINE))
                            .hover(|s| s.bg(rgb(ACCENT)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|v, _, _, cx| {
                                    v.resizing_graph = true;
                                    cx.stop_propagation();
                                }),
                            )
                            .on_click(|_, _, cx| cx.stop_propagation()),
                    ),
            )
            .into_any_element()
    }
    fn search_box(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let choices = [
            ("", "All fields", "message, author, SHA, branches"),
            ("m: ", "Message", "full commit description"),
            ("a: ", "Author", "name or email"),
            ("s: ", "Commit SHA", "full or partial hash"),
            ("f: ", "Changed file", "path or filename"),
        ];
        div()
            .relative()
            .flex_1()
            .min_w(px(100.))
            .max_w(px(420.))
            .child(
                row()
                    .gap_1()
                    .child(icon("search").text_color(rgb(MUTED)))
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.search).cleanable(true)),
                    ),
            )
            .when(self.search_help, |s| {
                s.child(
                    deferred(
                        column()
                            .absolute()
                            .top(px(36.))
                            .left_0()
                            .w(px(340.))
                            .p_1()
                            .rounded_md()
                            .bg(rgb(PANEL))
                            .border_1()
                            .border_color(rgb(LINE))
                            .shadow_lg()
                            .occlude()
                            .children(choices.into_iter().enumerate().map(
                                |(i, (prefix, title, hint))| {
                                    row()
                                        .id(("search-choice", i))
                                        .p_2()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgb(SELECTED)))
                                        .child(
                                            div()
                                                .w(px(26.))
                                                .text_color(rgb(ACCENT))
                                                .child(prefix.trim().to_string()),
                                        )
                                        .child(column().child(title).child(caption(hint)))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |v, _, w, cx| {
                                                cx.stop_propagation();
                                                v.action(Action::SearchField(prefix), w, cx);
                                            }),
                                        )
                                },
                            )),
                    )
                    .with_priority(2),
                )
            })
    }
    fn scroll_region(
        &self,
        slot: usize,
        content: impl IntoElement,
        scrollbar: Scrollbar,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        div()
            .id(("scroll-region", slot))
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .on_hover(cx.listener(move |v, hovered, _, cx| {
                v.hovered_scroll[slot] = *hovered;
                cx.notify();
            }))
            .child(content)
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .child(scrollbar.id(("scrollbar", slot)).scrollbar_show(
                        if self.hovered_scroll[slot] {
                            ScrollbarShow::Always
                        } else {
                            ScrollbarShow::Scrolling
                        },
                    )),
            )
    }
    fn files(&self, cx: &mut Context<Self>) -> AnyElement {
        let cs = match &self.engine.app.mode {
            Mode::Diff(v) => Some(&v.changes),
            Mode::Graph => self.engine.app.details_changes.as_ref(),
        };
        let count = cs.map_or(0, |c| c.tree.visible.len());
        let error = cs.and_then(|c| c.error.clone());
        let loading = cs.is_none_or(|c| !c.loaded);
        let list = uniform_list(
            "files",
            count,
            cx.processor(|v, range: std::ops::Range<usize>, _, cx| {
                v.viewport_rows[1] = v.viewport_rows[1].max(range.len());
                let cs = match &v.engine.app.mode {
                    Mode::Diff(d) => &d.changes,
                    Mode::Graph => v.engine.app.details_changes.as_ref().unwrap(),
                };
                range
                    .map(|i| {
                        let visible = cs.tree.visible[i];
                        let node = visible.node;
                        let item = &cs.tree.nodes[node];
                        let selected = item.file.is_some()
                            && matches!(&v.engine.app.mode, Mode::Diff(d) if d.current == item.file);
                        let mut r = row()
                            .id(("file", node))
                            .w_full()
                            .pr_3()
                            .h(px(30.))
                            .pl(px(8. + visible.depth as f32 * 14.))
                            .bg(rgb(if selected { SELECTED } else { PANEL }))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(CONTROL)));
                        if let Some(check) = cs.checkbox(node) {
                            r = r.child(div().id(("check", node)).child(check).on_click(
                                cx.listener(move |v, _, window, cx| {
                                    cx.stop_propagation();
                                    v.action(Action::Check(node), window, cx);
                                }),
                            ));
                        }
                        let prefix = if item.is_folder() {
                            if item.expanded { "▾" } else { "▸" }
                        } else {
                            ""
                        };
                        r.child(
                            div()
                                .flex_1()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .child(if v.matches.files {if let Some(file)=item.file {v.matches.styled_filename(&cs.files[file].path,&item.name)}else{v.matches.styled(format!("{prefix} {}",item.name))}} else {StyledText::new(format!("{prefix} {}",item.name))}),
                        )
                        .when_some(cs.node_total(node),|s,(a,r)|s.child(diff_stats(a,r)))
                        .child(caption(
                            item.kind
                                .map(|k| k.letter().to_string())
                                .unwrap_or_default(),
                        ))
                        .on_click(cx.listener(move |v, _, window, cx| {
                            v.action(Action::File(node), window, cx)
                        }))
                    })
                    .collect()
            }),
        )
        .track_scroll(if matches!(self.engine.app.mode,Mode::Diff(_)) {self.diff_files_scroll.clone()} else {self.files_scroll.clone()})
        .size_full();
        column()
            .flex_1()
            .min_h(px(120.))
            .child(
                row()
                    .p_2()
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(div().flex_1().child(caption("FILES")))
                    .child(self.button("expand", "", Action::Expand(true), cx))
                    .child(self.button("collapse", "", Action::Expand(false), cx)),
            )
            .when(loading, |s| s.child(caption("Loading files…")))
            .when_some(error, |s, e| s.child(caption(e)))
            .child(self.scroll_region(
                1,
                list,
                Scrollbar::vertical(if matches!(self.engine.app.mode, Mode::Diff(_)) {
                    &self.diff_files_scroll
                } else {
                    &self.files_scroll
                }),
                cx,
            ))
            .into_any_element()
    }
    fn details(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let app = &self.engine.app;
        let mut panel = column()
            .w(px(self.pane_width))
            .flex_shrink_0()
            .h_full()
            .bg(rgb(PANEL))
            .border_l_1()
            .border_color(rgb(LINE));
        if app.pending.selected {
            panel = panel.child(
                column()
                    .p_4()
                    .gap_3()
                    .child(div().text_size(px(18.)).child("Commit changes"))
                    .child(
                        Input::new(&self.description)
                            .disabled(app.pending.busy)
                            .h(px(150.)),
                    )
                    .child(Input::new(&self.new_branch).disabled(app.pending.busy))
                    .child(
                        row()
                            .child(self.button("commit", "Commit", Action::Commit, cx))
                            .child(self.button(
                                "check-all",
                                "Check all",
                                Action::CheckAll(true),
                                cx,
                            ))
                            .child(self.button(
                                "uncheck-all",
                                "Uncheck all",
                                Action::CheckAll(false),
                                cx,
                            )),
                    )
                    .when(!app.pending.status.is_empty(), |s| {
                        s.child(caption(app.pending.status.clone()))
                    }),
            );
        } else {
            panel = panel.child(
                column()
                    .h(relative(0.55))
                    .min_h(px(240.))
                    .flex_shrink_0()
                    .child(
                        row()
                            .h(px(44.))
                            .flex_shrink_0()
                            .px_4()
                            .border_b_1()
                            .border_color(rgb(LINE))
                            .child(div().flex_1().child(caption("COMMIT DETAILS")))
                            .when_some(self.detail_id, |s, id| {
                                s.child(
                                    div()
                                        .font_family("Cascadia Mono")
                                        .text_size(px(11.))
                                        .text_color(rgb(ACCENT))
                                        .child(id.to_hex_with_len(8).to_string()),
                                )
                            })
                            .when(self.detail_id.is_some(), |s| {
                                s.child(self.button("copy-details", "", Action::CopyDetails, cx))
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .px_4()
                            .py_3()
                            .when(self.detail_id.is_none(), |s| {
                                s.child(caption(if app.selected_id().is_some() {
                                    "Loading commit details…"
                                } else {
                                    "Select a commit to inspect its details"
                                }))
                            })
                            .when(self.detail_id.is_some(), |s| {
                                s.child(self.detail_text.clone())
                            }),
                    ),
            );
        }
        panel.child(self.files(cx)).into_any_element()
    }
    fn diff(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Mode::Diff(view) = &self.engine.app.mode else {
            return div().into_any_element();
        };
        let top =
            (-f32::from(self.diff_scroll.0.borrow().base_handle.offset().y) / 24.).max(0.) as usize;
        let source_top = self.wrap_layout.source(top);
        let mut reflow = false;
        if self.word_wrap
            && let Some(diff) = &view.diff
        {
            let single = view.current_change().is_some_and(|c| {
                matches!(c.kind, diff::ChangeKind::Added | diff::ChangeKind::Deleted)
            });
            let width = (f32::from(window.viewport_size().width) - self.diff_sidebar - 6.)
                / if single { 1. } else { 2. }
                - 68.;
            let started = Instant::now();
            reflow =
                self.wrap_layout
                    .update(view.id, view.current.unwrap_or(0), diff, width, window);
            if reflow && self.scenario.as_deref() == Some("wrap-scroll") {
                let elapsed = started.elapsed().as_secs_f64() * 1000.;
                eprintln!("wrap layout {elapsed:.3} ms");
            }
        }
        if let Some(source) = self
            .wrap_anchor
            .take()
            .or_else(|| reflow.then_some(source_top))
        {
            self.diff_scroll.scroll_to_item(
                if self.word_wrap {
                    self.wrap_layout.visual(source)
                } else {
                    source
                },
                ScrollStrategy::Top,
            );
        }
        let n = if self.word_wrap && view.diff.is_some() {
            self.wrap_layout.rows.len()
        } else {
            view.diff.as_ref().map_or(0, |d| d.rows.len())
        };
        let single_sided = view
            .current_change()
            .is_some_and(|c| matches!(c.kind, diff::ChangeKind::Added | diff::ChangeKind::Deleted));
        let line_width = if self.word_wrap {
            0.
        } else {
            view.diff.as_ref().map_or(0., |d| d.max_width as f32 * 7.4)
        };
        let geometry = self.diff_horizontal.clone();
        let horizontal = self.diff_horizontal.clone();
        let list = uniform_list(
            "diff-lines",
            n,
            cx.processor(|v, range: std::ops::Range<usize>, _, _| {
                v.viewport_rows[2] = v.viewport_rows[2].max(range.len());
                let Mode::Diff(view) = &v.engine.app.mode else {
                    return Vec::new();
                };
                let Some(diff) = &view.diff else {
                    return Vec::new();
                };
                let kind = view.current_change().map(|c| c.kind);
                range
                    .map(|visual| {
                        let wrapped = v.word_wrap.then(|| &v.wrap_layout.rows[visual]);
                        let i = wrapped.map_or(visual, |r| r.source);
                        match &diff.rows[i] {
                            diff::Row::Line { left, right } => row()
                                .w_full()
                                .h(px(24.))
                                .gap_0()
                                .when(kind != Some(diff::ChangeKind::Added), |r| {
                                    r.child(diff_line(
                                        left.as_ref().filter(|_| {
                                            wrapped.is_none_or(|r| r.sides[0].is_some())
                                        }),
                                        (view.id, i, false, visual),
                                        wrapped.and_then(|r| r.sides[0].clone()),
                                        v.selection.clone(),
                                        v.select_focus.clone(),
                                        v.diff_horizontal.x.clone(),
                                    ))
                                })
                                .when(kind != Some(diff::ChangeKind::Deleted), |r| {
                                    r.child(diff_line(
                                        right.as_ref().filter(|_| {
                                            wrapped.is_none_or(|r| r.sides[1].is_some())
                                        }),
                                        (view.id, i, true, visual),
                                        wrapped.and_then(|r| r.sides[1].clone()),
                                        v.selection.clone(),
                                        v.select_focus.clone(),
                                        v.diff_horizontal.x.clone(),
                                    ))
                                }),
                            diff::Row::Separator { skipped } => row()
                                .w_full()
                                .h(px(24.))
                                .child(caption(format!("··· {skipped} unchanged lines ···"))),
                            diff::Row::Message(s) => {
                                row().w_full().h(px(24.)).child(caption(s.clone()))
                            }
                        }
                    })
                    .collect()
            }),
        )
        .track_scroll(self.diff_scroll.clone())
        .size_full();
        row()
            .flex_1()
            .min_h_0()
            .items_start()
            .gap_0()
            .child(
                column()
                    .w(px(self.diff_sidebar))
                    .flex_shrink_0()
                    .h_full()
                    .child(self.files(cx)),
            )
            .child(
                div()
                    .id("diff-divider")
                    .w(px(5.))
                    .h_full()
                    .cursor_col_resize()
                    .bg(rgb(LINE))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|v, _, _, _| v.resizing_diff = true),
                    ),
            )
            .child(
                column()
                    .flex_1()
                    .h_full()
                    .border_l_1()
                    .border_color(rgb(LINE))
                    .child(
                        row()
                            .p_2()
                            .flex_shrink_0()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .h(px(24.))
                                    .child(self.diff_path.clone()),
                            )
                            .when_some(view.diff.as_ref(), |s,d|s.child(diff_stats(d.added,d.removed)))
                            .child(self.button("word-wrap", "", Action::Wrap, cx)),
                    )
                    .when(n == 0, |s| {
                        s.child(caption(if !view.changes.loaded {
                            "Loading files…"
                        } else if view.changes.error.is_some() {
                            "Unable to load files"
                        } else if view.current.is_none() {
                            "No changed files"
                        } else if view.diff.is_none() {
                            "Loading diff…"
                        } else {
                            "No text changes"
                        }))
                    })
                    .child(
                        self.scroll_region(
                            2,
                            div()
                                .size_full()
                                .relative()
                                .child(
                                    canvas(
                                        move |bounds, _, _| {
                                            geometry.bounds.set(bounds);
                                            geometry.configure(
                                                bounds.size.width,
                                                px(line_width),
                                                single_sided,
                                            );
                                        },
                                        move |bounds, _, window, _| {
                                            // Capture before the vertical list can reinterpret an
                                            // X-only (or Shift+wheel) gesture as vertical scrolling.
                                            window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
                                                if phase != DispatchPhase::Capture || !bounds.contains(&event.position) {
                                                    return;
                                                }
                                                let delta = event.delta.pixel_delta(px(24.));
                                                let dx = if event.modifiers.shift && delta.x == px(0.) {
                                                    delta.y
                                                } else {
                                                    delta.x
                                                };
                                                if dx != px(0.) {
                                                    horizontal.set_x(horizontal.x.get() + dx);
                                                    window.refresh();
                                                    cx.stop_propagation();
                                                }
                                            });
                                        },
                                    )
                                    .absolute()
                                    .size_full(),
                                )
                                .child(list),
                            Scrollbar::new(&self.diff_horizontal),
                            cx,
                        ),
                    ),
            )
            .into_any_element()
    }
    fn branches(&self, cx: &mut Context<Self>) -> AnyElement {
        let browser = &self.engine.app.branches;
        let list = uniform_list(
            "branches",
            browser.visible.len(),
            cx.processor(|v, range: std::ops::Range<usize>, _, cx| {
                v.viewport_rows[3] = v.viewport_rows[3].max(range.len());
                v.engine.app.branches.rows = range.len().max(1);
                v.engine.app.branches.scroll = range.start;
                let b = &v.engine.app.branches;
                range
                    .map(|i| {
                        let entry = &b.entries[b.visible[i]];
                        row()
                            .id(("branch", i))
                            .w_full()
                            .h(px(40.))
                            .px_4()
                            .bg(rgb(if b.selected == i { SELECTED } else { BG }))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(HOVER)))
                            .child(div().flex_1().overflow_hidden().whitespace_nowrap().child(
                                v.branch_matches.styled(format!(
                                    "{}{}",
                                    if entry.current { "● " } else { "" },
                                    entry.target.label()
                                )),
                            ))
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
                                div().w(px(62.)).flex_shrink_0().text_right().child(caption(
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
                    })
                    .collect()
            }),
        )
        .track_scroll(self.branches_scroll.clone())
        .size_full();
        column()
            .flex_1()
            .child(
                row()
                    .p_3()
                    .child(icon("search"))
                    .child(
                        div()
                            .w(px(340.))
                            .child(Input::new(&self.branch_filter).cleanable(true)),
                    )
                    .child(caption(format!("{} branches", browser.visible.len()))),
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
    fn settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let settings = &self.engine.app.settings;
        column()
            .flex_1()
            .p_6()
            .gap_4()
            .child(row().child(div().text_size(px(20.)).child("Remote settings")))
            .child(
                row()
                    .child(self.button("gitlab", "GitLab", Action::SettingsTab(0), cx))
                    .child(self.button("github", "GitHub", Action::SettingsTab(1), cx)),
            )
            .children(
                settings.drafts[settings.tab]
                    .iter()
                    .enumerate()
                    .map(|(i, d)| {
                        div()
                            .id(("remote", i))
                            .p_2()
                            .cursor_pointer()
                            .bg(rgb(if settings.selected[settings.tab] == i {
                                SELECTED
                            } else {
                                PANEL
                            }))
                            .child(d.config.name.clone())
                            .on_click(cx.listener(move |v, _, window, cx| {
                                v.action(Action::SettingsRemote(i), window, cx)
                            }))
                    }),
            )
            .child(
                column()
                    .max_w(px(650.))
                    .gap_3()
                    .child(caption("API URL"))
                    .child(Input::new(&self.api))
                    .child(caption("Project path"))
                    .child(Input::new(&self.project))
                    .child(caption("Access token · Windows Credential Manager"))
                    .child(Input::new(&self.token))
                    .child(
                        row()
                            .child(self.button(
                                "forget",
                                if settings.current().is_some_and(|d| d.forget) {
                                    "✓ Forget saved token"
                                } else {
                                    "Forget saved token"
                                },
                                Action::ForgetToken,
                                cx,
                            ))
                            .child(self.button("test", "Test", Action::TestSettings, cx))
                            .child(self.button("save", "Save", Action::SaveSettings, cx)),
                    ),
            )
            .child(caption(settings.status.clone()))
            .into_any_element()
    }
    fn scenario_ready(&self) -> bool {
        let app = &self.engine.app;
        match self.scenario.as_deref() {
            Some(
                "scroll" | "graph-resize" | "files-scroll" | "diff-scroll" | "diff-pan"
                | "search-scroll" | "wrap-scroll" | "tabs",
            ) => self.scroll_step > 121,
            Some("branches") => app.branches.open && !app.branches.activity_cache.is_empty(),
            Some("diff" | "pending-diff" | "compare") => {
                matches!(&app.mode,Mode::Diff(v) if v.diff.is_some())
            }
            Some("search") => app.search.result.as_ref().is_some_and(|r| r.done),
            Some("settings") => app.settings.open && !app.settings.busy,
            Some("pending") => app.pending.known && app.pending.selected,
            _ => true,
        }
    }
    fn snapshot(&self) -> String {
        let app = &self.engine.app;
        format!(
            "Git Rustle\n{}\n{} commits\n{}\n{}\n{}\nviewport_rows={:?}\n",
            app.repo_name,
            app.history.len(),
            app.details
                .as_ref()
                .map(|d| d.message.as_str())
                .unwrap_or_default(),
            app.ghost_status,
            app.pending.status,
            self.viewport_rows
        )
    }
}
impl Render for Desktop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_inputs(window, cx);
        let content = if self.engine.app.settings.open {
            self.settings(cx)
        } else if self.engine.app.branches.open {
            self.branches(cx)
        } else if matches!(self.engine.app.mode, Mode::Diff(_)) {
            self.diff(window, cx)
        } else {
            row()
                .flex_1()
                .min_h_0()
                .items_start()
                .gap_0()
                .child(self.graph(cx))
                .child(
                    div()
                        .id("pane-divider")
                        .w(px(5.))
                        .h_full()
                        .cursor_col_resize()
                        .bg(rgb(LINE))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|v, _, _, _| v.resizing = true),
                        ),
                )
                .child(self.details(window, cx))
                .into_any_element()
        };
        let app = &self.engine.app;
        let status = match &app.load {
            crate::app::LoadState::Empty | crate::app::LoadState::Loading => {
                "Loading history…".to_string()
            }
            crate::app::LoadState::Refreshing => "Refreshing history…".to_string(),
            crate::app::LoadState::Error(e) => e.clone(),
            _ if app.search.busy => app
                .search
                .result
                .as_ref()
                .map(|r| format!("Searching… {}/{}", r.examined, r.total))
                .unwrap_or_else(|| "Searching…".into()),
            _ if app
                .search
                .result
                .as_ref()
                .is_some_and(|r| r.unavailable > 0) =>
            {
                app.search
                    .result
                    .as_ref()
                    .and_then(|r| r.error.clone())
                    .unwrap_or_else(|| "Search incomplete: objects unavailable".into())
            }
            _ if app.branches.loading => "Refreshing remote branches…".into(),
            // Hide the idle count, while retaining remote progress and errors.
            _ if !app.ghost_status.ends_with(" ghost commits (remote only)") => {
                app.ghost_status.clone()
            }
            _ => String::new(),
        };
        let dialog = app.checkout_confirmation.as_ref().map(|d| {
            column()
                .absolute()
                .inset_0()
                .items_center()
                .justify_center()
                .bg(rgba(0x00000099))
                .child(
                    column()
                        .w(px(620.))
                        .p_6()
                        .gap_4()
                        .rounded_lg()
                        .bg(rgb(PANEL))
                        .border_1()
                        .border_color(rgb(LINE))
                        .child(div().text_size(px(20.)).child("Confirm checkout"))
                        .children(d.choices.iter().enumerate().map(|(i, c)| {
                            div()
                                .id(("choice", i))
                                .p_3()
                                .rounded_md()
                                .bg(rgb(CONTROL))
                                .cursor_pointer()
                                .child(c.label())
                                .on_click(cx.listener(move |v, _, window, cx| {
                                    v.action(Action::Confirm(i), window, cx)
                                }))
                        }))
                        .child(self.button(
                            "cancel-checkout",
                            "Cancel",
                            Action::CancelCheckout,
                            cx,
                        )),
                )
        });
        if self.scroll_step > 0 && self.scroll_step <= 121 {
            let weak = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = weak.update(cx, |v, cx| {
                    if let Some(start) = v.frame_start.take() {
                        v.frame_samples.push(start.elapsed().as_secs_f64() * 1000.);
                    }
                    v.scroll_step += 1;
                    if v.scroll_step <= 121 {
                        match v.scenario.as_deref() {
                            Some("files-scroll") => {
                                let n = v
                                    .engine
                                    .app
                                    .details_changes
                                    .as_ref()
                                    .map_or(0, |c| c.tree.visible.len());
                                v.files_scroll.scroll_to_item(
                                    (v.scroll_step * 7) % n.max(1),
                                    ScrollStrategy::Top,
                                );
                            }
                            Some("graph-resize") => {
                                v.graph_width = if v.scroll_step.is_multiple_of(2) {
                                    84.
                                } else {
                                    420.
                                };
                            }
                            Some("tabs") => {
                                if v.scroll_step.is_multiple_of(2) {
                                    v.engine.app.park_diff();
                                } else {
                                    v.engine.app.resume_diff();
                                }
                            }
                            Some("wrap-scroll") => {
                                v.diff_scroll.scroll_to_item(
                                    (v.scroll_step * 37) % v.wrap_layout.rows.len().max(1),
                                    ScrollStrategy::Top,
                                );
                            }
                            Some("diff-scroll") => {
                                let n = match &v.engine.app.mode {
                                    Mode::Diff(d) => d.diff.as_ref().map_or(0, |d| d.rows.len()),
                                    _ => 0,
                                };
                                v.diff_scroll.scroll_to_item(
                                    (v.scroll_step * 37) % n.max(1),
                                    ScrollStrategy::Top,
                                );
                            }
                            Some("diff-pan") => v
                                .diff_horizontal
                                .set_x(-px(((v.scroll_step * 37) % 1600) as f32)),
                            _ => {
                                let row = (v.scroll_step * 37) % v.engine.app.graph_len().max(1);
                                v.graph_scroll.scroll_to_item(row, ScrollStrategy::Top);
                            }
                        }
                        v.frame_start = Some(Instant::now());
                        cx.notify();
                    } else {
                        v.engine.mark("desktop scroll done (120 measured frames)");
                        cx.notify();
                        if let Some(path) = std::env::var_os("GIT_RUSTLE_FRAME_SAMPLES") {
                            let _ = std::fs::write(
                                path,
                                serde_json::to_vec_pretty(&v.frame_samples[1..]).unwrap(),
                            );
                        }
                    }
                });
            });
        }
        if self.scenario_started
            && self.scenario.is_some()
            && !self.scenario_presented
            && self.scenario_ready()
        {
            let weak = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = weak.update(cx, |v, _| {
                    v.scenario_presented = true;
                    v.engine.mark("desktop scenario frame");
                });
            });
        }
        let weak = cx.entity().downgrade();
        if !self.engine.has_presented()
            && (!app.history.is_empty()
                || matches!(
                    app.load,
                    crate::app::LoadState::Complete
                        | crate::app::LoadState::Cached
                        | crate::app::LoadState::Error(_)
                ))
        {
            window.on_next_frame(move |_, cx| {
                let _ = weak.update(cx, |v, cx| {
                    v.engine.presented();
                    v.changed(cx);
                });
            });
        }
        column()
            .relative()
            .size_full()
            .track_focus(&self.select_focus)
            .on_key_down(cx.listener(|v, e: &KeyDownEvent, window, cx| {
                if e.keystroke.key == "escape" && v.context_menu.take().is_some() {
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                if v.select_focus.is_focused(window)
                    && e.keystroke.modifiers.control
                    && e.keystroke.key == "c"
                {
                    let s = v.selection.borrow();
                    if let Mode::Diff(view) = &v.engine.app.mode
                        && let Some(diff) = &view.diff
                    {
                        let text = s.copy(diff);
                        if !text.is_empty() {
                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                        }
                    }
                    cx.stop_propagation();
                }
            }))
            .on_mouse_move(cx.listener(|v, e: &MouseMoveEvent, window, cx| {
                if v.resizing_graph && e.pressed_button == Some(MouseButton::Left) {
                    v.graph_width = (f32::from(e.position.x) - 8.).clamp(
                        28.,
                        (f32::from(window.viewport_size().width) - v.pane_width - 250.).max(28.),
                    );
                    cx.notify();
                }
                if v.resizing_diff && e.pressed_button == Some(MouseButton::Left) {
                    v.diff_sidebar = f32::from(e.position.x).clamp(200., 600.);
                    cx.notify();
                }
                if v.resizing && e.pressed_button == Some(MouseButton::Left) {
                    v.pane_width =
                        (f32::from(window.viewport_size().width - e.position.x)).clamp(320., 800.);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|v, _, _, _| {
                    v.resizing_graph = false;
                    v.resizing = false;
                    v.resizing_diff = false;
                    v.selection.borrow_mut().dragging = false;
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|v, _, _, _| {
                    v.selection.borrow_mut().dragging = false;
                    v.resizing_graph = false;
                    v.resizing = false;
                    v.resizing_diff = false;
                }),
            )
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .font_family("Segoe UI")
            .text_size(px(14.))
            .child(
                row()
                    .h(px(68.))
                    .flex_shrink_0()
                    .px_4()
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(
                        column()
                            .flex_1()
                            .overflow_hidden()
                            .child(div().text_size(px(18.)).child(app.repo_name.clone()))
                            .child(caption(self.engine.repository_path()).whitespace_nowrap()),
                    )
                    .child(self.button("explorer", "", Action::Explorer, cx))
                    .child(self.button("terminal", "", Action::Terminal, cx)),
            )
            .child(self.tabs(cx))
            .child(content)
            .when(!status.is_empty(), |s| {
                s.child(
                    div()
                        .px_3()
                        .py_1()
                        .border_t_1()
                        .border_color(rgb(LINE))
                        .child(caption(status)),
                )
            })
            .when_some(app.checkout_notice.as_ref(), |s, n| {
                s.child(caption(n.as_ref().unwrap_or_else(|e| e).clone()))
            })
            .when(!self.notice.is_empty(), |s| {
                s.child(caption(self.notice.clone()))
            })
            .children(dialog)
            .children(self.context_menu.map(|p| self.commit_menu(p, window, cx)))
    }
}

fn diff_stats(added: u32, removed: u32) -> Div {
    row()
        .flex_shrink_0()
        .text_size(px(12.))
        .child(div().text_color(rgb(ADDED)).child(format!("+{added}")))
        .child(div().text_color(rgb(REMOVED)).child(format!("−{removed}")))
}

fn diff_line(
    line: Option<&diff::Line>,
    id: (u32, usize, bool, usize),
    span: Option<std::ops::Range<usize>>,
    selection: std::rc::Rc<std::cell::RefCell<crate::selectable::Selection>>,
    focus: FocusHandle,
    horizontal: std::rc::Rc<std::cell::Cell<Pixels>>,
) -> Div {
    let added = id.2;
    let changed = line.is_some_and(|l| l.kind != diff::LineKind::Context);
    let offset = span.as_ref().map_or(0, |r| r.start);
    let text = line
        .map(|l| {
            span.as_ref()
                .map_or_else(|| l.text.clone(), |r| l.text[r.clone()].to_string())
        })
        .unwrap_or_default();
    let hl = line
        .map(|l| {
            if span.is_none() {
                return l.hl.clone();
            }
            let start = l.text[..offset].chars().count();
            let end = start + text.chars().count();
            l.hl.iter()
                .filter_map(|r| {
                    let a = r.start.max(start);
                    let b = r.end.min(end);
                    (a < b).then_some(a.saturating_sub(start)..b.saturating_sub(start))
                })
                .collect()
        })
        .unwrap_or_default();
    row()
        .flex_1()
        .min_w_0()
        .h_full()
        .gap_1()
        .overflow_hidden()
        .font_family("Cascadia Mono")
        .text_size(px(12.))
        .bg(rgb(if changed {
            if added { ADDED_BG } else { REMOVED_BG }
        } else {
            BG
        }))
        .child(
            div()
                .w(px(48.))
                .flex_shrink_0()
                .text_color(rgb(MUTED))
                .child(
                    line.filter(|_| offset == 0)
                        .map(|l| format!("{:>5}", l.no))
                        .unwrap_or_default(),
                ),
        )
        .when(line.is_some(), |s| {
            s.child(crate::selectable::line(
                id, text, hl, selection, focus, horizontal, offset,
            ))
        })
}
fn graph_canvas(
    cells: Vec<crate::graph::layout::GraphCell>,
    flags: Vec<u8>,
    width: f32,
) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let middle = bounds.top() + px(15.);
            for (i, c) in cells.iter().enumerate().take((width / 14.) as usize) {
                let x = bounds.left() + px(i as f32 * 14. + 7.);
                let color: Hsla = rgb(COLORS[c.color() as usize % 10]).into();
                let color: Hsla = if flags.get(i).is_some_and(|f| f & 1 != 0) {
                    color.opacity(0.42)
                } else {
                    color
                };
                let k = c.kind();
                let top = matches!(
                    k,
                    CellKind::Vertical
                        | CellKind::Node
                        | CellKind::MergeNode
                        | CellKind::Cross
                        | CellKind::EndFromRight
                        | CellKind::EndFromLeft
                        | CellKind::JoinFromLeft
                        | CellKind::JoinFromRight
                );
                let bottom = matches!(
                    k,
                    CellKind::Vertical
                        | CellKind::Node
                        | CellKind::MergeNode
                        | CellKind::Cross
                        | CellKind::StartToRight
                        | CellKind::StartToLeft
                        | CellKind::JoinFromLeft
                        | CellKind::JoinFromRight
                );
                if top {
                    window.paint_quad(fill(
                        Bounds::new(point(x - px(1.), bounds.top()), size(px(2.), px(15.))),
                        color,
                    ));
                }
                if bottom {
                    window.paint_quad(fill(
                        Bounds::new(point(x - px(1.), middle), size(px(2.), px(15.))),
                        color,
                    ));
                }
                if matches!(
                    k,
                    CellKind::Horizontal
                        | CellKind::Cross
                        | CellKind::EndFromRight
                        | CellKind::StartToRight
                        | CellKind::JoinFromLeft
                ) {
                    window.paint_quad(fill(
                        Bounds::new(point(x - px(14.), middle - px(1.)), size(px(14.), px(2.))),
                        color,
                    ));
                }
                if matches!(
                    k,
                    CellKind::Horizontal
                        | CellKind::Cross
                        | CellKind::EndFromLeft
                        | CellKind::StartToLeft
                        | CellKind::JoinFromRight
                ) {
                    window.paint_quad(fill(
                        Bounds::new(point(x, middle - px(1.)), size(px(14.), px(2.))),
                        color,
                    ));
                }
                if matches!(k, CellKind::Node | CellKind::MergeNode) {
                    window.paint_quad(quad(
                        Bounds::new(point(x - px(4.), middle - px(4.)), size(px(8.), px(8.))),
                        px(4.),
                        color,
                        px(1.),
                        color,
                        BorderStyle::Solid,
                    ));
                }
            }
        },
    )
    .w(px(width))
    .h(px(30.))
    .flex_shrink_0()
}
