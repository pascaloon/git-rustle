//! Native desktop presentation. Lists construct only visible rows.
use crate::desktop_icons::icon;
use crate::desktop_text::{Matches, TextBlock};
use crate::desktop_theme::*;
use crate::{
    app::Mode,
    diff,
    engine::Engine,
    graph::layout::{CellKind, GraphCell},
};
use gpui::{prelude::*, *};
use gpui_component::{
    Disableable, Root, Selectable, Sizable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu, PopupMenuItem},
    scroll::{Scrollbar, ScrollbarShow},
};
use std::time::{Duration, Instant};
mod landing;
mod operations;
mod rebase;
mod requests;
mod status;

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
                        if v.composer.as_ref().is_some_and(|c| c.busy())
                            || v.engine.operation_busy
                            || v.engine.recents_loading
                            || v.engine.app.pending.busy
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
    Request,
    Rebase,
    Conflicts,
    Graph,
    Branches,
    Diff,
    Settings,
}

#[derive(Clone)]
enum Action {
    NewRequest(String),
    Operation(Box<operations::Action>),
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
    CopySha1,
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
    SettingsProvider(Option<&'static str>),
    SaveSettings,
    ForgetToken,
}

struct Desktop {
    composer: Option<requests::Composer>,
    avatars: requests::Avatars,
    picking_directory: bool,
    opening_directory: bool,
    engine: Engine,
    operations: operations::Panel,
    search: Entity<InputState>,
    search_field: &'static str,
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
    status_panel: Option<status::Panel>,
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

// Checkout and Git operation dialogs share one shell and action alignment.
fn dialog_body(title: &'static str) -> Div {
    column()
        .gap_4()
        .child(div().flex_shrink_0().text_size(px(22.)).child(title))
}
fn dialog_actions() -> Div {
    row().flex_shrink_0().justify_end()
}
fn dialog_overlay(body: Div) -> Div {
    column()
        .absolute()
        .inset_0()
        .p_6()
        .items_center()
        .justify_center()
        .bg(rgba(0x00000099))
        .occlude()
        .child(
            body.w(px(560.))
                .max_w_full()
                .max_h_full()
                .p_6()
                .bg(rgb(PANEL))
                .rounded_lg()
                .border_1()
                .border_color(rgb(LINE))
                .shadow_lg(),
        )
}

// Keep typed field prefixes working while the inset selector supplies the default.
// An empty input must clear the filter even when a field remains selected.
fn search_query(field: &str, text: &str) -> String {
    if text.trim().is_empty() {
        String::new()
    } else if matches!(text.trim_start().get(..2), Some("m:" | "a:" | "s:" | "f:")) {
        text.to_string()
    } else {
        format!("{field}{text}")
    }
}

fn toolbar_filter(input: &Entity<InputState>, cx: &App) -> Input {
    Input::new(input)
        .small()
        .min_h(px(30.))
        .py_0()
        .cleanable(true)
        .when(input.read(cx).value().is_empty(), |s| {
            s.prefix(icon("search").text_color(rgb(MUTED)))
        })
}

fn tab_bar() -> Div {
    row()
        .h(px(38.))
        .flex_shrink_0()
        .gap_0()
        .px_3()
        .border_b_1()
        .border_color(rgb(LINE))
}

fn tab_item(id: impl Into<ElementId>, active: bool, enabled: bool) -> Stateful<Div> {
    row()
        .id(id)
        .h_full()
        .px_4()
        .border_b_2()
        .border_color(rgb(if active { ACCENT } else { BG }))
        .bg(rgb(if active { PANEL } else { BG }))
        .text_color(rgb(if enabled { TEXT } else { MUTED }))
        .when(enabled, |s| s.cursor_pointer().hover(|s| s.bg(rgb(HOVER))))
}

impl Desktop {
    fn tabs(&self, cx: &Context<Self>) -> AnyElement {
        let app = &self.engine.app;
        let current = if self.composer.as_ref().is_some_and(|c| c.active) {
            Tab::Request
        } else if let Some(tab) = self.operations.tab {
            tab
        } else if app.settings.open {
            Tab::Settings
        } else if app.branches.open {
            Tab::Branches
        } else if matches!(app.mode, Mode::Diff(_)) {
            Tab::Diff
        } else {
            Tab::Graph
        };
        let has_diff = app.parked_diff.is_some() || matches!(app.mode, Mode::Diff(_));
        tab_bar()
            .children(
                [
                    (Tab::Graph, "graph", "Git Graph"),
                    (Tab::Branches, "branch", "Branches"),
                    (Tab::Request, "branch", "New PR / MR"),
                    (Tab::Rebase, "graph", "Interactive rebase"),
                    (Tab::Conflicts, "diff", "Conflicts"),
                    (Tab::Diff, "diff", "Diff"),
                    (Tab::Settings, "settings", "Settings"),
                ]
                .into_iter()
                .filter(|(t, _, _)| *t != Tab::Diff || has_diff)
                .filter(|(t, _, _)| *t != Tab::Request || self.composer.is_some())
                .filter(|(t, _, _)| {
                    *t != Tab::Rebase
                        || self.operations.rebase_open
                        || self.engine.operations.interactive_rebase
                })
                .filter(|(t, _, _)| {
                    *t != Tab::Conflicts || self.engine.operations.conflict.is_some()
                })
                .enumerate()
                .map(|(i, (tab, name, label))| {
                    tab_item(("tab", i), current == tab, true)
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
        let mut items = vec![
            (
                Action::Diff,
                "diff",
                if app.pending.selected {
                    "Diff pending changes"
                } else if count > 1 {
                    "Compare selected commits"
                } else {
                    "Diff commit"
                }
                .to_string(),
                enabled_diff,
            ),
            (
                Action::Checkout,
                "checkout",
                "Checkout…".into(),
                enabled_checkout,
            ),
        ];
        if enabled_checkout && let Some(id) = app.selected_id() {
            let target = id.to_string();
            let labels = app
                .refs
                .as_ref()
                .map(|r| {
                    r.refs
                        .iter()
                        .filter(|r| r.id == id && r.kind != crate::git::RefKind::Head)
                        .map(|r| r.name.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let label = if labels.is_empty() {
                id.to_hex_with_len(12).to_string()
            } else {
                format!("{} · {}", labels.join(", "), id.to_hex_with_len(12))
            };
            let idle = !self.engine.operation_busy
                && !app.pending.busy
                && !app.branches.busy
                && !app.settings.saving
                && self.engine.operations.conflict.is_none()
                && !app.ghosts.contains_key(&id);
            let attached = app.refs.as_ref().is_some_and(|r| r.head_branch.is_some());
            if app.head_id() != Some(id) {
                for (dialog, title) in [
                    (operations::Dialog::Merge, "Merge into current branch…"),
                    (
                        operations::Dialog::Rebase,
                        "Rebase current branch onto this…",
                    ),
                ] {
                    items.push((
                        Action::Operation(Box::new(operations::Action::Open(
                            dialog,
                            target.clone(),
                            label.clone(),
                        ))),
                        "graph",
                        title.into(),
                        idle && attached,
                    ));
                }
            }
            items.push((
                Action::Operation(Box::new(operations::Action::Open(
                    operations::Dialog::Plan,
                    target,
                    label,
                ))),
                "graph",
                "Interactive rebase from here…".into(),
                idle && attached && !self.operations.rebase_open,
            ));
            if let Some(refs) = &app.refs {
                for branch in refs
                    .refs
                    .iter()
                    .filter(|r| r.id == id && r.kind == crate::git::RefKind::Branch)
                {
                    items.push((
                        Action::NewRequest(branch.name.clone()),
                        "branch",
                        format!("Create PR / MR from {}…", branch.name),
                        true,
                    ));
                    for (dialog, verb) in [
                        (operations::Dialog::Rename, "Rename"),
                        (operations::Dialog::Delete, "Delete"),
                    ] {
                        let enabled = idle
                            && (dialog != operations::Dialog::Delete
                                || refs.head_branch.as_ref() != Some(&branch.name));
                        items.push((
                            Action::Operation(Box::new(operations::Action::Open(
                                dialog,
                                branch.name.clone(),
                                branch.name.clone(),
                            ))),
                            "branch",
                            format!("{verb} {}…", branch.name),
                            enabled,
                        ));
                    }
                }
            }
        }
        let menu_height =
            ((items.len() * 36 + 10) as f32).min(f32::from(window.viewport_size().height) - 16.);
        let p = point(
            position
                .x
                .min(window.viewport_size().width - px(320.))
                .max(px(0.)),
            position
                .y
                .min(window.viewport_size().height - px(menu_height))
                .max(px(0.)),
        );
        deferred(
            column()
                .id("commit-menu")
                .absolute()
                .left(p.x)
                .top(p.y)
                .w(px(320.))
                .max_h(px(menu_height))
                .overflow_y_scroll()
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
                .children(items.into_iter().enumerate().map(
                    |(i, (action, name, label, enabled))| {
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
                    },
                )),
        )
        .with_priority(3)
        .into_any_element()
    }

    fn new(mut engine: Engine, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let operations = operations::Panel::new(window, cx);
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
            cx.subscribe_in(&token, window, |v, state, event, window, cx| {
                if matches!(event, InputEvent::Focus)
                    && v.engine.app.settings.page > 0
                    && state.read(cx).value() == crate::provider_settings::MASK
                {
                    window.defer(cx, |window, cx| {
                        window.dispatch_action(Box::new(gpui_component::input::SelectAll), cx);
                    });
                }
            }),
            cx.subscribe(&search, |v, state, event, cx| {
                if matches!(event, InputEvent::Change) {
                    v.engine.app.search.text.value =
                        search_query(v.search_field, &state.read(cx).value());
                    v.matches = Matches::new(&v.engine.app.search.text.value);
                    v.selected_commits.clear();
                    v.engine.app.search.changed();
                    v.changed(cx);
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
        let task = cx.spawn_in(window, async move |view, cx| {
            loop {
                let Ok(messages) = view.update(cx, |v, _| v.engine.messages()) else { break; };
                let message =
                    futures_lite::future::or(async { messages.recv().await.ok() }, async {
                        Timer::after(Duration::from_millis(250)).await;
                        None
                    })
                    .await;
                if view
                    .update_in(cx, |v, window, cx| {
                        // A folder may have opened while this receiver was awaiting a message.
                        if !messages.same_channel(&v.engine.messages()) { return; }
                        let old = v.engine.app.graph_selected();
                        let saving = v.engine.app.settings.saving;
                        v.engine.tick(message);
                        v.sync_operations(window, cx);
                        v.sync_avatars(cx);
                        if saving && !v.engine.app.settings.saving {
                            v.settings_identity = None;
                            v.avatars = Default::default();
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
                                Some("landing-open") if v.engine.landing() => {
                                    if let Some(path) = std::env::var_os("GIT_RUSTLE_LANDING_REPO") {
                                        v.open_repository(path.into(), cx);
                                    }
                                }
                                Some("landing-forget") if v.engine.landing() => {
                                    if let Some(path) = v.engine.recents.first().cloned() {
                                        v.engine.mark("forget repository requested");
                                        v.engine.forget_repository(path);
                                    }
                                }
                                Some("operations" | "operations-create") => {
                                    v.operation_action(
                                        operations::Action::Open(
                                            operations::Dialog::Create,
                                            "HEAD".into(),
                                            "HEAD".into(),
                                        ),
                                        window,
                                        cx,
                                    );
                                    v.engine.mark("desktop operation requested");
                                    if v.scenario.as_deref() == Some("operations-create") {
                                        v.engine.request_operation(
                                            crate::git::operations::Request::Create {
                                                name: "bench-topic".into(),
                                                target: "HEAD".into(),
                                            },
                                        );
                                        v.engine.tick(None);
                                    }
                                }
                                Some("push-review" | "push-review-orange" | "push-review-red") => {
                                    v.engine.mark("desktop operation requested");
                                    v.operation_action(
                                        operations::Action::Open(
                                            operations::Dialog::Push,
                                            String::new(),
                                            String::new(),
                                        ),
                                        window,
                                        cx,
                                    );
                                }
                                Some("integrate-review" | "graph-menu") => {
                                    v.engine.mark("desktop operation requested");
                                    if let Some(id) = v.engine.app.selected_id() {
                                        if v.scenario.as_deref() == Some("graph-menu") {
                                            v.context_menu = Some(point(px(220.), px(210.)));
                                        } else {
                                            v.operation_action(
                                                operations::Action::Open(
                                                    operations::Dialog::Merge,
                                                    id.to_string(),
                                                    id.to_hex_with_len(12).to_string(),
                                                ),
                                                window,
                                                cx,
                                            );
                                        }
                                    }
                                }
                                Some("rebase-plan" | "rebase-scroll" | "rebase-edit") => {
                                    v.engine.mark("desktop operation requested");
                                    v.operation_action(
                                        operations::Action::Open(
                                            operations::Dialog::Plan,
                                            std::env::var("GIT_RUSTLE_REBASE_BASE").unwrap_or_else(|_| "HEAD~3".into()),
                                            "HEAD~3".into(),
                                        ),
                                        window,
                                        cx,
                                    );
                                    v.operation_action(operations::Action::Submit, window, cx);
                                }
                                Some("history" | "history-scroll" | "history-clear") => {
                                    v.engine.mark("desktop operation requested");
                                    v.operation_action(operations::Action::History, window, cx);
                                }
                                Some("status-panels") => {
                                    v.toggle_status_panel(status::Panel::Details);
                                    v.scroll_step = 1;
                                    v.frame_start = Some(Instant::now());
                                    cx.notify();
                                }
                                Some("checkout-dialog") => {
                                    v.action(Action::Select(1, false), window, cx);
                                    v.action(Action::Checkout, window, cx);
                                    assert!(v.engine.app.checkout_confirmation.is_some());
                                    v.scroll_step = 1;
                                    v.frame_start = Some(Instant::now());
                                    cx.notify();
                                }
                                Some("scroll" | "graph-resize" | "avatar-scroll" | "avatar-reuse-scroll") => {
                                    if matches!(v.scenario.as_deref(), Some("avatar-scroll" | "avatar-reuse-scroll")) {
                                        let path = std::env::var_os("GIT_RUSTLE_AVATAR_FIXTURE").expect("avatar image fixture required");
                                        let bytes = std::fs::read(path).expect("read avatar fixture");
                                        let format = crate::remote::requests::image_format(&bytes).expect("PNG, JPEG or WebP avatar fixture");
                                        let image = std::sync::Arc::new(Image::from_bytes(format, bytes));
                                        if v.scenario.as_deref() == Some("avatar-reuse-scroll") {
                                            v.avatars.reuse_scenario_image = Some(image);
                                            v.avatars.seed_reuse_scenario();
                                        } else { v.avatars.scenario_image = Some(image); }
                                    }
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
                                Some("settings" | "settings-tabs") => {
                                    v.action(Action::Settings, window, cx)
                                }
                                Some("compare") => {
                                    v.action(Action::Select(0, false), window, cx);
                                    v.action(Action::Select(1, true), window, cx);
                                    v.action(Action::Diff, window, cx);
                                }
                                Some("request-open") => {
                                    let source = v.engine.app.refs.as_ref()
                                        .and_then(|r| r.head_branch.clone()).unwrap_or_default();
                                    v.action(Action::NewRequest(source), window, cx);
                                }
                                Some("request-compose" | "request-edit" | "request-tabs" | "request-clear") => {
                                    v.request_scenario(window, cx);
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
                        if v.scenario_started && v.scenario.as_deref() == Some("history-clear")
                            && v.history_scenario_ready() && !v.engine.operation_error
                        {
                            v.engine.mark("desktop clear requested");
                            v.operation_action(operations::Action::ReviewClear, window, cx);
                            v.operation_action(operations::Action::ClearHistory, window, cx);
                        }
                        if v.scenario_started && v.scroll_step == 0 {
                            let ready = match v.scenario.as_deref() {
                                Some("history-scroll") => v.history_scenario_ready(),
                                Some("request-edit" | "request-tabs" | "request-clear") => v.composer.is_some(),
                                Some("rebase-scroll" | "rebase-edit") => v.rebase_scenario_ready(),
                                Some("settings-tabs") => {
                                    v.engine.app.settings.open && !v.engine.app.settings.busy
                                }
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
                                    Mode::Diff(d) if d.diff.as_ref().is_some_and(|d| d.syntax.is_some()))
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
                            && !v.composer.as_ref().is_some_and(|c| c.busy())
                            && !v.engine.operation_busy
                            && !v.engine.recents_loading
                            && !v.engine.app.pending.busy
                            && !v.engine.app.branches.busy
                            && !v.engine.app.settings.saving)
                        {
                            if let Some(path) = std::env::var_os("GIT_RUSTLE_DUMP") {
                                let _ = std::fs::write(path, v.snapshot(cx));
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
            composer: None,
            avatars: Default::default(),
            picking_directory: false,
            opening_directory: false,
            engine,
            operations,
            search,
            search_field: "",
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
            status_panel: None,
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
        self.sync_request_repository();
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
                app.settings.page,
                app.settings.project_selected,
            );
            if self.settings_identity != Some(key) {
                self.settings_identity = Some(key);
                if app.settings.page == 0 {
                    let project = app
                        .settings
                        .projects
                        .get(app.settings.project_selected)
                        .and_then(|d| {
                            d.project
                                .clone()
                                .or_else(|| d.remote().ok().map(|r| r.project))
                        })
                        .unwrap_or_default();
                    self.project
                        .update(cx, |s, cx| s.set_value(project, window, cx));
                } else {
                    let d = &app.settings.providers[app.settings.page - 1];
                    self.api
                        .update(cx, |s, cx| s.set_value(d.url.clone(), window, cx));
                    self.token
                        .update(cx, |s, cx| s.set_value(d.token_text(), window, cx));
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
        if self.status_panel == Some(status::Panel::Details) {
            self.status_panel = None;
        }
        if matches!(
            action,
            Action::SettingsTab(_)
                | Action::SettingsRemote(_)
                | Action::SaveSettings
                | Action::ForgetToken
        ) && self.settings_identity.is_some()
            && !self.engine.app.settings.busy
        {
            let s = &mut self.engine.app.settings;
            if s.page == 0 {
                if let Some(d) = s.projects.get_mut(s.project_selected) {
                    d.project = Some(self.project.read(cx).value().to_string());
                }
            } else {
                let d = &mut s.providers[s.page - 1];
                d.url = self.api.read(cx).value().to_string();
                d.set_token_text(self.token.read(cx).value().to_string());
            }
        }
        if matches!(action, Action::File(_) | Action::Diff) {
            *self.selection.borrow_mut() = Default::default();
            self.diff_horizontal.set_x(px(0.));
        }
        self.context_menu = None;
        if let Action::NewRequest(source) = action {
            self.open_request(source, window, cx);
            return;
        }
        if let Some(c) = &mut self.composer {
            if let Action::Tab(tab) = action {
                c.active = tab == Tab::Request;
            } else if matches!(
                action,
                Action::Head
                    | Action::Branches
                    | Action::Settings
                    | Action::Diff
                    | Action::JumpBranch
            ) {
                c.active = false;
            }
        }
        if let Action::Operation(action) = action {
            self.operation_action(*action, window, cx);
            return;
        }
        if matches!(
            action,
            Action::Tab(_)
                | Action::Head
                | Action::Branches
                | Action::Settings
                | Action::Diff
                | Action::JumpBranch
        ) {
            self.operations.tab = match action {
                Action::Tab(tab @ (Tab::Rebase | Tab::Conflicts)) => Some(tab),
                _ => None,
            };
        }
        if self.engine.operation_busy
            && matches!(
                action,
                Action::Commit | Action::Checkout | Action::Confirm(_) | Action::SaveSettings
            )
        {
            return;
        }
        let app = &mut self.engine.app;
        if matches!(
            action,
            Action::Select(_, _) | Action::Head | Action::JumpBranch
        ) {
            app.begin_navigation();
        }
        match action {
            Action::NewRequest(_) => unreachable!(),
            Action::Operation(_) => unreachable!(),
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
                self.search_field = prefix;
                let text = self.search.read(cx).value().to_string();
                let text = text.trim();
                let value = if matches!(text.get(..2), Some("m:" | "a:" | "s:" | "f:")) {
                    text[2..].trim_start()
                } else {
                    text
                };
                self.search.update(cx, |s, cx| {
                    s.set_value(value.to_string(), window, cx);
                    s.focus(window, cx);
                });
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
            Action::CopySha1 => {
                if let Some(id) = app.selected_id() {
                    cx.write_to_clipboard(ClipboardItem::new_string(id.to_string()));
                }
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
                    if app.checkout_request.is_some() && !self.engine.operation_error {
                        self.engine.operation_status.clear();
                    }
                }
            }
            Action::SettingsTab(i) => {
                if !app.settings.busy {
                    app.settings.page = i;
                }
            }
            Action::SettingsRemote(i) => {
                if !app.settings.busy {
                    app.settings.project_selected = i;
                }
            }
            Action::SettingsProvider(provider) => {
                if !app.settings.busy
                    && let Some(d) = app.settings.projects.get_mut(app.settings.project_selected)
                {
                    d.provider = provider.map(str::to_string);
                }
            }
            Action::SaveSettings => app.settings.save_page(),
            Action::ForgetToken => {
                if !app.settings.busy && app.settings.page > 0 {
                    let d = &mut app.settings.providers[app.settings.page - 1];
                    d.forget = !d.forget;
                    d.token.clear();
                    self.settings_identity = None;
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
    fn checkout_choice(
        &self,
        index: usize,
        choice: &crate::checkout::Request,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        use crate::checkout::Request;
        let (symbol, action, target, description, branch) = match choice {
            Request::Branch(t) => (
                "branch",
                "SWITCH BRANCH",
                t.label(),
                if t.remote.is_some() {
                    "Use a local branch that tracks this remote."
                } else {
                    "Continue working on this branch."
                },
                true,
            ),
            Request::Commit { id, remote } => (
                "commit",
                "DETACHED HEAD",
                id.to_hex_with_len(12).to_string(),
                if remote.is_some() {
                    "Inspect this commit; download missing objects if needed."
                } else {
                    "Inspect this commit in detached HEAD mode."
                },
                false,
            ),
        };
        let full_target = match choice {
            Request::Commit { id, .. } => id.to_string(),
            _ => target.clone(),
        };
        row()
            .id(("choice", index))
            .w_full()
            .flex_shrink_0()
            .p_3()
            .gap_3()
            .rounded_md()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(CONTROL))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(HOVER)).border_color(rgb(ACCENT)))
            .tooltip(move |w, cx| {
                gpui_component::tooltip::Tooltip::new(full_target.clone()).build(w, cx)
            })
            .child(
                row()
                    .size(px(36.))
                    .flex_shrink_0()
                    .justify_center()
                    .rounded_md()
                    .bg(rgb(if branch { SELECTED } else { PANEL }))
                    .child(icon(symbol).size(px(20.)).text_color(rgb(if branch {
                        ACCENT
                    } else {
                        MUTED
                    }))),
            )
            .child(
                column()
                    .flex_1()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(10.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(if branch { ACCENT } else { MUTED }))
                            .child(action),
                    )
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_ellipsis()
                            .child(target),
                    )
                    .child(caption(description)),
            )
            .child(icon("next").text_color(rgb(MUTED)))
            .on_click(
                cx.listener(move |v, _, window, cx| v.action(Action::Confirm(index), window, cx)),
            )
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
            Action::Operation(ref a) => match a.as_ref() {
                operations::Action::Pull => Some("pull"),
                operations::Action::Open(operations::Dialog::Push, ..) => Some("push"),
                operations::Action::Open(operations::Dialog::Create, ..) => Some("branch"),
                _ => None,
            },
            Action::Settings => Some("settings"),
            Action::Wrap => Some("wrap"),
            Action::Explorer => Some("folder"),
            Action::Terminal => Some("terminal"),
            Action::Head => Some("head"),
            Action::Branches => Some("branch"),
            Action::Diff => Some("diff"),
            Action::Checkout => Some("checkout"),
            Action::CopySha1 => Some("copy"),
            Action::Expand(true) => Some("expand"),
            Action::Expand(false) => Some("collapse"),

            _ => None,
        };
        let text: SharedString = text.into();
        let enabled = (!matches!(action, Action::Diff)
            || self.engine.app.pending.selected
            || matches!(
                self.selected_commits
                    .ids(self.engine.app.selected_id())
                    .len(),
                1 | 2
            ))
            && (!matches!(
                action,
                Action::SettingsTab(_)
                    | Action::SettingsRemote(_)
                    | Action::SettingsProvider(_)
                    | Action::SaveSettings
                    | Action::ForgetToken
            ) || !self.engine.app.settings.busy)
            && (!matches!(action, Action::Operation(_))
                || (!self.engine.operation_busy
                    && !self.engine.app.pending.busy
                    && !self.engine.app.branches.busy
                    && !self.engine.app.settings.saving)
                || matches!(&action, Action::Operation(a) if matches!(a.as_ref(), operations::Action::Cancel)))
            && (!matches!(action, Action::SaveSettings)
                || self.engine.app.settings.page > 0
                || !self.engine.app.settings.projects.is_empty());
        let active = match action {
            Action::SettingsTab(i) => self.engine.app.settings.page == i,
            Action::SettingsProvider(provider) => self
                .engine
                .app
                .settings
                .projects
                .get(self.engine.app.settings.project_selected)
                .is_some_and(|d| d.provider.as_deref() == provider),
            Action::Wrap => self.word_wrap,
            _ => false,
        };
        let save = matches!(action, Action::SaveSettings);
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
            .bg(rgb(if save && enabled {
                ACCENT
            } else if active {
                SELECTED
            } else {
                CONTROL
            }))
            .text_color(rgb(if save && enabled {
                BG
            } else if active {
                ACCENT
            } else if enabled {
                TEXT
            } else {
                MUTED
            }))
            .when(enabled, |s| {
                s.cursor_pointer()
                    .hover(move |s| s.bg(rgb(if save { ACCENT_HOVER } else { SELECTED_HOVER })))
            })
            .when_some(icon_name, |s, n| s.child(icon(n)))
            .when_some(icon_name, |s, n| {
                s.tooltip(move |w, cx| {
                    gpui_component::tooltip::Tooltip::new(match n {
                        "settings" => "Settings",
                        "refresh" => "Refresh",
                        "pull" => "Pull current upstream",
                        "push" => "Review push for the current branch",
                        "wrap" => "Toggle word wrap",
                        "folder" => "Open in Explorer",
                        "terminal" => "Open in Windows Terminal",
                        "expand" => "Expand folders",
                        "collapse" => "Collapse folders",
                        "diff" => "Diff selected commits (one or two)",
                        "head" => "Go to HEAD",
                        "checkout" => "Checkout",
                        "copy" => "Copy SHA1",
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
    fn graph_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        row()
            .h(px(48.))
            .flex_shrink_0()
            .px_3()
            .border_b_1()
            .border_color(rgb(LINE))
            .child(self.button("head", "HEAD", Action::Head, cx))
            .child(self.button("refresh", "", Action::Refresh, cx))
            .child(self.button(
                "pull",
                "Pull",
                Action::Operation(Box::new(operations::Action::Pull)),
                cx,
            ))
            .child(self.button(
                "push",
                "Push",
                Action::Operation(Box::new(operations::Action::Open(
                    operations::Dialog::Push,
                    String::new(),
                    String::new(),
                ))),
                cx,
            ))
            .child(
                self.button(
                    "create-branch",
                    "New branch",
                    Action::Operation(Box::new(operations::Action::Open(
                        operations::Dialog::Create,
                        self.engine
                            .app
                            .head_id()
                            .map(|id| id.to_string())
                            .unwrap_or_else(|| "HEAD".into()),
                        format!("{} (HEAD)", self.current_branch()),
                    ))),
                    cx,
                ),
            )
            .child(caption(format!(
                "{} commits",
                self.engine
                    .app
                    .search
                    .result
                    .as_ref()
                    .map_or(self.engine.app.history.len(), |r| r.rows.len())
            )))
            .child(
                self.button(
                    "new-request",
                    "PR / MR",
                    Action::NewRequest(
                        self.engine
                            .app
                            .refs
                            .as_ref()
                            .and_then(|r| r.head_branch.clone())
                            .unwrap_or_default(),
                    ),
                    cx,
                ),
            )
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
            .child(div().flex_1())
            .child(self.search_box(cx))
            .into_any_element()
    }
    fn graph(&self, cx: &mut Context<Self>) -> AnyElement {
        let n = self.engine.app.graph_len();
        let list = uniform_list(
            "history",
            n,
            cx.processor(|v, range: std::ops::Range<usize>, _, cx| {
                v.viewport_rows[0] = v.viewport_rows[0].max(range.len());
                v.reset_avatar_repository();
                let app = &v.engine.app;
                let changed = v.avatars.observe(range.clone().filter_map(|row| {
                    app.history_index(row)
                        .map(|i| v.engine.app.history.commits[i].id)
                }));
                if changed {
                    v.resolve_avatar_authors(cx);
                }
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
                            // Keep dates clear of the 16px overlay scrollbar, even on hover.
                            .pr(px(24.))
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
                                    row()
                                        .w(px(150.))
                                        .flex_shrink_0()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_color(rgb(MUTED))
                                        .child(requests::avatar(
                                            v.avatars
                                                .image(&rec.id)
                                                .or(v.avatars.scenario_image.as_ref()),
                                            app.history.author(i),
                                            22.,
                                        ))
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
                            let head = app
                                .pending
                                .head_index
                                .and_then(|i| app.history.commits.get(i).map(|rec| (i, rec)));
                            let lane = head.map_or(0, |(_, rec)| rec.node_lane as usize);
                            let cells = pending_graph_cells(
                                head.map_or(&[], |(i, _)| app.history.row_cells(i)),
                                lane,
                            );
                            let flags = head.map_or_else(Vec::new, |(_, rec)| {
                                app.history
                                    .remote_cells
                                    .get(rec.row.range())
                                    .unwrap_or_default()
                                    .to_vec()
                            });
                            r = r
                                .child(pending_graph_canvas(graph_width, lane, cells, flags))
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
            ("", "All fields"),
            ("m: ", "Message"),
            ("a: ", "Author"),
            ("s: ", "Commit SHA"),
            ("f: ", "Changed file"),
        ];
        let query = self.search.read(cx).value();
        let selected = choices
            .iter()
            .find(|(prefix, _)| !prefix.is_empty() && query.trim_start().starts_with(prefix.trim()))
            .or_else(|| {
                choices
                    .iter()
                    .find(|(prefix, _)| *prefix == self.search_field)
            })
            .copied()
            .unwrap_or(choices[0]);
        let view = cx.entity().downgrade();
        let selector = Button::new("search-field")
            .xsmall()
            .ghost()
            .label(match selected.0 {
                "s: " => "SHA",
                "f: " => "File",
                _ => selected.1,
            })
            .child(div().text_color(rgb(MUTED)).child("▾"))
            .tooltip("Search field")
            .dropdown_menu(move |mut menu, _, _| {
                for (prefix, title) in choices {
                    let view = view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(title)
                            .checked(prefix == selected.0)
                            .on_click(move |_, window, cx| {
                                _ = view.update(cx, |v, cx| {
                                    v.action(Action::SearchField(prefix), window, cx);
                                });
                            }),
                    );
                }
                menu
            });
        div().flex_1().min_w(px(180.)).max_w(px(420.)).child(
            toolbar_filter(&self.search, cx).prefix(
                row()
                    .gap_2()
                    .child(
                        div()
                            .pr_1()
                            .border_r_1()
                            .border_color(rgb(LINE))
                            .child(selector),
                    )
                    .when(query.is_empty(), |s| {
                        s.child(icon("search").text_color(rgb(MUTED)))
                    }),
            ),
        )
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
                            .bg(rgb(if selected { SELECTED } else { BG }))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(CONTROL)));
                        if let Some(check) = cs.check_state(node) {
                            r = r.child(v.file_checkbox(node, check, cx));
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
                        .when(!item.is_folder() || !item.expanded, |s| {
                            s.when_some(cs.node_total(node), |s, (a, r)| s.child(diff_stats(a, r)))
                        })
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
    fn file_checkbox(
        &self,
        node: usize,
        state: diff::changes::CheckState,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        use diff::changes::CheckState;
        let disabled = self.engine.app.pending.busy;
        div()
            .id(("file-check-hitbox", node))
            .relative()
            .size(px(16.))
            .flex_shrink_0()
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                Checkbox::new(("file-check", node))
                    .checked(state != CheckState::Unchecked)
                    .disabled(disabled)
                    .on_click(cx.listener(move |v, _, window, cx| {
                        cx.stop_propagation();
                        v.action(Action::Check(node), window, cx);
                    })),
            )
            .when(state == CheckState::Mixed, |s| {
                // The component supplies focus, input and the box; paint a dash
                // over its checkmark for a partially selected folder.
                s.child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| {
                            let opacity = if disabled { 0.5 } else { 1. };
                            let color: Hsla = rgb(ACCENT).into();
                            if disabled {
                                window.paint_quad(quad(
                                    Bounds::new(
                                        bounds.origin + point(px(1.), px(1.)),
                                        size(px(14.), px(14.)),
                                    ),
                                    px(3.),
                                    rgb(BG),
                                    px(0.),
                                    rgb(BG),
                                    BorderStyle::Solid,
                                ));
                            }
                            window.paint_quad(quad(
                                Bounds::new(
                                    bounds.origin + point(px(1.), px(1.)),
                                    size(px(14.), px(14.)),
                                ),
                                px(3.),
                                color.opacity(opacity),
                                px(0.),
                                color,
                                BorderStyle::Solid,
                            ));
                            window.paint_quad(fill(
                                Bounds::new(
                                    bounds.origin + point(px(4.), px(7.)),
                                    size(px(8.), px(2.)),
                                ),
                                rgb(BG),
                            ));
                        },
                    )
                    .absolute()
                    .inset_0(),
                )
            })
    }
    fn details(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let app = &self.engine.app;
        let mut panel = column()
            .w(px(self.pane_width))
            .flex_shrink_0()
            .h_full()
            .bg(rgb(BG))
            .border_l_1()
            .border_color(rgb(INSPECTOR_LINE));
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
            panel =
                panel.child(
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
                                .border_color(rgb(INSPECTOR_LINE))
                                .child(div().flex_1().child(
                                    caption("COMMIT DETAILS").text_color(rgb(INSPECTOR_MUTED)),
                                ))
                                .when_some(self.detail_id, |s, id| {
                                    s.child(requests::avatar(
                                        self.avatars.image(&id),
                                        app.details
                                            .as_ref()
                                            .map(|d| d.author_name.as_str())
                                            .unwrap_or("?"),
                                        28.,
                                    ))
                                    .child(
                                        div()
                                            .font_family("Cascadia Mono")
                                            .text_size(px(11.))
                                            .text_color(rgb(INSPECTOR_MUTED))
                                            .child(id.to_hex_with_len(8).to_string()),
                                    )
                                })
                                .when(self.detail_id.is_some(), |s| {
                                    s.child(self.button("copy-sha1", "", Action::CopySha1, cx))
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
                                        diff.syntax_line(false, left.as_ref()),
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
                                        diff.syntax_line(true, right.as_ref()),
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
                    .h(px(48.))
                    .flex_shrink_0()
                    .px_3()
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(caption(format!("{} branches", browser.visible.len())))
                    .child(
                        self.button(
                            "branch-request",
                            "Create PR / MR",
                            Action::NewRequest(
                                browser
                                    .chosen()
                                    .map(|e| e.target.name.clone())
                                    .unwrap_or_default(),
                            ),
                            cx,
                        ),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(180.))
                            .max_w(px(420.))
                            .child(toolbar_filter(&self.branch_filter, cx)),
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
    fn settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let settings = &self.engine.app.settings;
        let mut form = column().max_w(px(700.)).gap_3();
        if settings.page == 0 {
            form = form
                .child(caption("Settings for this project"))
                .child(caption("Repository path"))
                .child(
                    div()
                        .text_color(rgb(TEXT))
                        .child(self.engine.repository_path()),
                )
                .child(caption("Remote"))
                .child(
                    row()
                        .flex_wrap()
                        .children(settings.projects.iter().enumerate().map(|(i, d)| {
                            div()
                                .id(("remote", i))
                                .px_3()
                                .py_2()
                                .rounded_md()
                                .bg(rgb(if settings.project_selected == i {
                                    SELECTED
                                } else {
                                    CONTROL
                                }))
                                .text_color(rgb(if settings.project_selected == i {
                                    ACCENT
                                } else {
                                    TEXT
                                }))
                                .child(d.name.clone())
                                .when(!settings.busy, |s| {
                                    s.cursor_pointer().on_click(cx.listener(
                                        move |v, _, window, cx| {
                                            v.action(Action::SettingsRemote(i), window, cx)
                                        },
                                    ))
                                })
                        })),
                );
            if let Some(d) = settings.projects.get(settings.project_selected) {
                form = form
                    .child(div().text_color(rgb(TEXT)).child(d.url.clone()))
                    .child(caption("Provider"))
                    .child(
                        row()
                            .child(self.button(
                                "provider-auto",
                                "Auto",
                                Action::SettingsProvider(None),
                                cx,
                            ))
                            .child(self.button(
                                "provider-gitlab",
                                "GitLab",
                                Action::SettingsProvider(Some("gitlab")),
                                cx,
                            ))
                            .child(self.button(
                                "provider-github",
                                "GitHub",
                                Action::SettingsProvider(Some("github")),
                                cx,
                            )),
                    )
                    .child(caption("Project path · group/project or owner/repository"))
                    .child(Input::new(&self.project).disabled(settings.busy));
            } else if !settings.busy {
                form = form.child(caption(
                    "No remotes configured. Add one with git remote add, then reopen the app.",
                ));
            }
        } else {
            let draft = &settings.providers[settings.page - 1];
            form = form.child(caption("Global settings · shared by all your projects"))
                .child(caption("Custom server or API URL"))
                .child(Input::new(&self.api).disabled(settings.busy))
                .child(caption("Access token · stored in Windows Credential Manager"))
                .child(Input::new(&self.token).disabled(settings.busy))
                .child(caption(if draft.forget { "The saved token will be removed when you save." }
                    else if draft.has_token { "A custom token is saved. Leave it masked to keep it, or replace it." }
                    else { "No custom token saved. Environment tokens and Git credentials can still be used." }))
                .child(self.button("forget", if draft.forget { "✓ Forget saved token" } else { "Forget saved token" }, Action::ForgetToken, cx));
        }
        column()
            .flex_1()
            .p_6()
            .gap_4()
            .child(div().text_size(px(20.)).child("Settings"))
            .child(
                tab_bar().children(["Project", "GitLab", "GitHub"].into_iter().enumerate().map(
                    |(page, label)| {
                        tab_item(
                            ("settings-tab", page),
                            settings.page == page,
                            !settings.busy,
                        )
                        .child(label)
                        .when(!settings.busy, |s| {
                            s.on_click(cx.listener(move |v, _, window, cx| {
                                v.action(Action::SettingsTab(page), window, cx)
                            }))
                        })
                    },
                )),
            )
            .child(form)
            .child(row().child(self.button(
                "save",
                if settings.saving { "Saving…" } else { "Save" },
                Action::SaveSettings,
                cx,
            )))
            .child(
                div()
                    .text_color(rgb(if settings.error { REMOVED } else { MUTED }))
                    .child(settings.status.clone()),
            )
            .into_any_element()
    }
    fn scenario_ready(&self) -> bool {
        let app = &self.engine.app;
        match self.scenario.as_deref() {
            Some("history-clear") => {
                !self.engine.operation_busy
                    && self.engine.operations.labels.is_empty()
                    && self.engine.operation_status == "Undo history cleared."
            }
            Some(
                "operations" | "history" | "push-review" | "push-review-orange" | "push-review-red"
                | "integrate-review" | "graph-menu",
            ) => !self.engine.operation_busy,
            Some("operations-create") => {
                !self.engine.operation_busy
                    && self
                        .engine
                        .operations
                        .labels
                        .iter()
                        .any(|l| l == "Create branch bench-topic")
            }
            Some("rebase-plan") => {
                self.engine.operations.plan.is_some()
                    && self.operations.rebase_open
                    && !self.engine.operation_busy
            }
            Some("request-compose" | "request-open") => self.composer.is_some(),
            Some(
                "history-scroll"
                | "rebase-scroll"
                | "rebase-edit"
                | "scroll"
                | "graph-resize"
                | "files-scroll"
                | "diff-scroll"
                | "diff-pan"
                | "search-scroll"
                | "wrap-scroll"
                | "tabs"
                | "settings-tabs"
                | "checkout-dialog"
                | "status-panels"
                | "request-edit"
                | "request-tabs"
                | "request-clear"
                | "avatar-scroll"
                | "avatar-reuse-scroll",
            ) => self.scroll_step > 121,
            Some("branches") => app.branches.open && !app.branches.activity_cache.is_empty(),
            Some("diff" | "pending-diff" | "compare") => {
                matches!(&app.mode,Mode::Diff(v) if v.diff.as_ref().is_some_and(|d| d.syntax.is_some()))
            }
            Some("search") => app.search.result.as_ref().is_some_and(|r| r.done),
            Some("settings") => app.settings.open && !app.settings.busy,
            Some("pending") => app.pending.known && app.pending.selected,
            _ => true,
        }
    }
    fn snapshot(&self, cx: &gpui::App) -> String {
        if self.engine.landing() {
            return format!(
                "Git Rustle\nlanding page\nOpen Directory\nrecent_count={}\n{}\n{}\n",
                self.engine.recents.len(),
                self.engine
                    .recents
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
                self.engine.recents_error
            );
        }
        let app = &self.engine.app;
        format!(
            "Git Rustle\n{}\n{} commits\n{}\n{}\n{}\nviewport_rows={:?}\nsyntax_spans={}\ncheckout_choices={}\noperations={} cursor={} conflicts={}\n{}\n",
            app.repo_name,
            app.history.len(),
            app.details
                .as_ref()
                .map(|d| d.message.as_str())
                .unwrap_or_default(),
            app.ghost_status,
            app.pending.status,
            self.viewport_rows,
            match &app.mode {
                Mode::Diff(v) => v
                    .diff
                    .as_ref()
                    .and_then(|d| d.syntax.as_ref())
                    .map_or(0, |s| s.span_count()),
                _ => 0,
            },
            app.checkout_confirmation
                .as_ref()
                .map_or(0, |d| d.choices.len()),
            self.engine.operations.labels.len(),
            self.engine.operations.cursor,
            self.engine
                .operations
                .conflict
                .as_ref()
                .map_or(0, |c| c.files.len()),
            format_args!(
                "footer_panel={:?}\n{}\n{}\n{}\n{}",
                self.status_panel,
                self.engine.operation_status,
                self.operations.summary(),
                self.request_summary(cx),
                self.avatars.summary()
            ),
        )
    }
}
impl Render for Desktop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.engine.landing()
            || (self.opening_directory && self.engine.repository_path().is_empty())
        {
            return self.landing_page(window, cx).into_any_element();
        }
        self.sync_inputs(window, cx);
        let content = if self.composer.as_ref().is_some_and(|c| c.active) {
            self.request_content(window, cx)
        } else if self.operations.tab.is_some() {
            self.operations_content(cx)
        } else if self.engine.app.settings.open {
            self.settings(cx)
        } else if self.engine.app.branches.open {
            self.branches(cx)
        } else if matches!(self.engine.app.mode, Mode::Diff(_)) {
            self.diff(window, cx)
        } else {
            column()
                .flex_1()
                .child(self.graph_toolbar(cx))
                .child(
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
                                .bg(rgb(BG))
                                .hover(|s| s.bg(rgb(CONTROL)))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|v, _, _, _| v.resizing = true),
                                ),
                        )
                        .child(self.details(window, cx)),
                )
                .into_any_element()
        };
        let app = &self.engine.app;
        let dialog = app.checkout_confirmation.as_ref().map(|d| {
            dialog_overlay(
                dialog_body("Confirm checkout")
                    .child(caption("Choose how to open this revision."))
                    .child(
                        column()
                            .id("checkout-options")
                            .gap_2()
                            .max_h(px(360.))
                            .overflow_y_scroll()
                            .children(
                                d.choices
                                    .iter()
                                    .enumerate()
                                    .map(|(i, c)| self.checkout_choice(i, c, cx)),
                            ),
                    )
                    .child(dialog_actions().child(self.button(
                        "cancel-checkout",
                        "Cancel",
                        Action::CancelCheckout,
                        cx,
                    ))),
            )
        });
        if self.scroll_step > 0 && self.scroll_step <= 121 {
            let weak = cx.entity().downgrade();
            window.on_next_frame(move |window, cx| {
                let _ = weak.update(cx, |v, cx| {
                    if let Some(start) = v.frame_start.take() {
                        v.frame_samples.push(start.elapsed().as_secs_f64() * 1000.);
                    }
                    v.scroll_step += 1;
                    if v.scroll_step <= 121 {
                        let handler_start = Instant::now();
                        match v.scenario.as_deref() {
                            Some("history-scroll") => v.history_scenario_scroll(v.scroll_step),
                            Some("request-edit" | "request-tabs" | "request-clear") => {
                                v.request_scenario_step(window, cx)
                            }
                            Some("rebase-scroll" | "rebase-edit") => {
                                v.rebase_scenario_step(v.scroll_step, window, cx);
                            }
                            Some("status-panels") => {
                                if matches!(v.scroll_step % 4, 1 | 2) {
                                    v.toggle_status_panel(status::Panel::Details);
                                } else {
                                    v.operation_action(operations::Action::History, window, cx);
                                }
                            }
                            Some("settings-tabs") => {
                                v.action(Action::SettingsTab(v.scroll_step % 3), window, cx)
                            }
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
                        v.frame_start = Some(if v.scenario.as_deref() == Some("history-scroll") {
                            handler_start
                        } else {
                            Instant::now()
                        });
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
                if e.keystroke.key == "escape" && v.status_panel.take().is_some() {
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                if e.keystroke.key == "escape" && v.operations.dismiss() {
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
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
            .child(self.status_bar(cx))
            .children(self.status_details(cx))
            .children(dialog)
            .children(self.operations_overlay(cx))
            .children(self.context_menu.map(|p| self.commit_menu(p, window, cx)))
            .into_any_element()
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
    syntax: &[diff::syntax::Span],
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
            let decorations = crate::selectable::Decorations {
                changes: hl,
                syntax: diff::syntax::clipped(syntax, offset..offset + text.len()),
            };
            s.child(crate::selectable::line(
                id,
                text,
                decorations,
                selection,
                focus,
                horizontal,
                offset,
            ))
        })
}
fn graph_node_center(bounds: Bounds<Pixels>, lane: usize) -> Point<Pixels> {
    point(
        bounds.left() + px(lane as f32 * 14. + 7.),
        bounds.top() + bounds.size.height / 2.,
    )
}

const GRAPH_NODE_RADIUS: f32 = 5.;

fn paint_graph_node(center: Point<Pixels>, color: Hsla, hollow: bool, window: &mut Window) {
    let radius = px(GRAPH_NODE_RADIUS);
    window.paint_quad(quad(
        Bounds::new(
            center - point(radius, radius),
            size(radius * 2., radius * 2.),
        ),
        radius,
        if hollow { rgba(0).into() } else { color },
        px(if hollow { 2. } else { 1. }),
        color,
        BorderStyle::Solid,
    ));
}

fn graph_connects_top(kind: CellKind) -> bool {
    matches!(
        kind,
        CellKind::Vertical
            | CellKind::Node
            | CellKind::MergeNode
            | CellKind::Cross
            | CellKind::EndFromRight
            | CellKind::EndFromLeft
            | CellKind::JoinFromLeft
            | CellKind::JoinFromRight
    )
}

fn graph_color(cell: GraphCell, flag: u8) -> Hsla {
    let color: Hsla = rgb(COLORS[cell.color() as usize % COLORS.len()]).into();
    if flag & 1 != 0 {
        color.opacity(0.42)
    } else {
        color
    }
}

/// The synthetic row sits immediately above HEAD: only its incoming lanes
/// cross this row. Merge-parent starts and horizontal connectors belong below it.
fn pending_graph_cells(head: &[GraphCell], lane: usize) -> Vec<GraphCell> {
    let mut cells: Vec<_> = head
        .iter()
        .enumerate()
        .map(|(i, cell)| {
            if i == lane {
                GraphCell::new(CellKind::Node, cell.color())
            } else if graph_connects_top(cell.kind()) {
                GraphCell::new(CellKind::Vertical, cell.color())
            } else {
                GraphCell::EMPTY
            }
        })
        .collect();
    // Unborn repositories and history still loading have no HEAD row yet.
    if cells.len() <= lane {
        cells.resize(lane + 1, GraphCell::EMPTY);
        cells[lane] = GraphCell::new(CellKind::Node, 0);
    }
    cells
}

fn pending_graph_canvas(
    width: f32,
    lane: usize,
    cells: Vec<GraphCell>,
    flags: Vec<u8>,
) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            for (i, cell) in cells.iter().enumerate().take((width / 14.) as usize) {
                let center = graph_node_center(bounds, i);
                let color = graph_color(*cell, flags.get(i).copied().unwrap_or(0));
                if i == lane {
                    // Stop at the rim so the pending marker stays hollow on every background.
                    let start = center.y + px(GRAPH_NODE_RADIUS);
                    window.paint_quad(fill(
                        Bounds::new(
                            point(center.x - px(1.), start),
                            size(px(2.), bounds.bottom() - start),
                        ),
                        color,
                    ));
                    paint_graph_node(center, color, true, window);
                } else if cell.kind() == CellKind::Vertical {
                    window.paint_quad(fill(
                        Bounds::new(
                            point(center.x - px(1.), bounds.top()),
                            size(px(2.), bounds.size.height),
                        ),
                        color,
                    ));
                }
            }
        },
    )
    .w(px(width))
    .h_full()
    .flex_shrink_0()
}

fn graph_canvas(cells: Vec<GraphCell>, flags: Vec<u8>, width: f32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            for (i, c) in cells.iter().enumerate().take((width / 14.) as usize) {
                let center = graph_node_center(bounds, i);
                let x = center.x;
                let middle = center.y;
                let color = graph_color(*c, flags.get(i).copied().unwrap_or(0));
                let k = c.kind();
                let top = graph_connects_top(k);
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
                        Bounds::new(
                            point(x - px(1.), bounds.top()),
                            size(px(2.), middle - bounds.top()),
                        ),
                        color,
                    ));
                }
                if bottom {
                    window.paint_quad(fill(
                        Bounds::new(
                            point(x - px(1.), middle),
                            size(px(2.), bounds.bottom() - middle),
                        ),
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
                    paint_graph_node(center, color, false, window);
                }
            }
        },
    )
    .w(px(width))
    // Fill the commit row so adjacent canvases meet without a vertical gap.
    .h_full()
    .flex_shrink_0()
}

#[cfg(test)]
mod graph_tests {
    use super::{CellKind, GraphCell, pending_graph_cells};
    use crate::graph::layout::Layout;

    fn id(n: u8) -> gix::ObjectId {
        gix::ObjectId::from_bytes_or_panic(&[n; 20])
    }

    #[test]
    fn pending_inherits_head_color_and_preserves_parallel_branch() {
        let mut layout = Layout::new();
        let other = layout.push(&id(3), &[id(1)]);
        let head = layout.push(&id(2), &[id(1)]);
        let lane = head.node_lane as usize;
        assert_ne!(lane, other.node_lane as usize);
        let pending = pending_graph_cells(&head.cells, lane);
        assert_eq!(pending[lane].color(), head.cells[lane].color());
        assert_ne!(pending[lane].color(), other.cells[0].color());
        assert_eq!(
            pending[0],
            GraphCell::new(CellKind::Vertical, other.cells[0].color())
        );
    }

    #[test]
    fn pending_preserves_incoming_merge_lanes_but_does_not_start_merge_parents() {
        let mut layout = Layout::new();
        layout.push(&id(4), &[id(2)]);
        layout.push(&id(3), &[id(2)]);
        let head = layout.push(&id(2), &[id(1)]);
        assert_eq!(head.cells[1].kind(), CellKind::EndFromRight);
        let pending = pending_graph_cells(&head.cells, head.node_lane as usize);
        assert_eq!(
            pending[1],
            GraphCell::new(CellKind::Vertical, head.cells[1].color())
        );

        let merge = Layout::new().push(&id(4), &[id(3), id(2)]);
        assert_eq!(merge.cells[1].kind(), CellKind::StartToRight);
        let pending = pending_graph_cells(&merge.cells, merge.node_lane as usize);
        assert_eq!(pending[1], GraphCell::EMPTY);
    }

    #[test]
    fn pending_without_head_has_a_fallback_node() {
        assert_eq!(
            pending_graph_cells(&[], 0),
            vec![GraphCell::new(CellKind::Node, 0)]
        );
    }
}
