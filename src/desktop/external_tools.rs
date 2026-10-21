//! Saved comparison tools are hydrated on a worker; the editor keeps a separate draft.
use super::*;
use crate::external_tools::{self as tools, Config, Entry, Preset};
use gpui_component::popover::Popover;
use std::{path::PathBuf, sync::Arc};

#[derive(Clone)]
pub(super) enum Action {
    Toggle,
    Configure,
    Add,
    Remove,
    Select(String),
    Save,
    OpenDiff(diff::FileChange),
    OpenMerge(String),
    ClosePicker,
    Diff(String, diff::FileChange),
    Merge(String, String),
}
#[derive(Clone)]
enum Picker {
    Diff(diff::FileChange),
    Merge(String),
}
type Choices = Arc<Vec<(String, String)>>;

fn editor_arguments(previous: &Option<Vec<String>>, text: &str) -> Option<Vec<String>> {
    // The line editor cannot distinguish an embedded LF from an argument
    // separator. Keep the original argv verbatim until its display is edited.
    if previous
        .as_ref()
        .is_some_and(|args| args.join("\n") == text)
    {
        return previous.clone();
    }
    if text.is_empty() {
        return None;
    }
    Some(
        text.replace("\r\n", "\n")
            .split('\n')
            .map(str::to_owned)
            .collect(),
    )
}
#[derive(Default)]
pub(super) struct State {
    open: bool,
    loaded: bool,
    loading: bool,
    attempted: bool,
    busy: bool,
    saving: bool,
    merging: bool,
    config: Config,
    draft: Config,
    selected: Option<String>,
    inputs: Option<[Entity<InputState>; 4]>,
    diff_choices: Option<Choices>,
    merge_choices: Option<Choices>,
    picker: Option<Picker>,
    message: String,
    error: bool,
    icons: std::collections::HashMap<PathBuf, Option<Arc<Image>>>,
    icon_paths: Vec<PathBuf>,
    icon_generation: u64,
    icons_pending: bool,
    icon_scenario_started: bool,
    general_requested: bool,
    icon_paints:
        std::rc::Rc<std::cell::RefCell<std::collections::HashMap<PathBuf, Bounds<Pixels>>>>,
}
impl Desktop {
    fn refresh_tool_icons(&mut self, force: bool, cx: &mut Context<Self>) {
        let mut paths: Vec<_> = self
            .tools
            .config
            .tools
            .iter()
            .chain(&self.tools.draft.tools)
            .map(|entry| entry.executable.clone())
            .collect();
        paths.sort();
        paths.dedup();
        if !force && paths == self.tools.icon_paths {
            return;
        }
        self.tools.icon_paths = paths.clone();
        self.tools.icon_generation = self.tools.icon_generation.wrapping_add(1);
        let generation = self.tools.icon_generation;
        self.tools.icons_pending = true;
        cx.spawn(async move |view, cx| {
            let icons: std::collections::HashMap<PathBuf, Option<Arc<Image>>> = cx
                .background_executor()
                .spawn(async move {
                    paths
                        .into_iter()
                        .map(|path| {
                            let image = tools::icons::load(&path);
                            (path, image)
                        })
                        .collect()
                })
                .await;
            let _ = view.update(cx, |v, cx| {
                if v.tools.icon_generation != generation {
                    return;
                }
                for image in v.tools.icons.values().flatten() {
                    if !icons
                        .values()
                        .flatten()
                        .any(|current| current.id() == image.id())
                    {
                        image.clone().remove_asset(cx);
                    }
                }
                v.tools.icons = icons;
                v.tools.icons_pending = false;
                cx.notify();
            });
        })
        .detach();
    }
    fn tool_icon(&self, path: &std::path::Path) -> AnyElement {
        let image = match self.tools.icons.get(path).and_then(Option::as_ref) {
            Some(image) => img(image.clone())
                .size(px(20.))
                .flex_shrink_0()
                .into_any_element(),
            None => icon("tool").size(px(20.)).into_any_element(),
        };
        if !matches!(
            self.scenario.as_deref(),
            Some("tool-icons" | "tool-icons-diff")
        ) {
            return image;
        }
        let paints = self.tools.icon_paints.clone();
        let path = path.to_path_buf();
        div()
            .relative()
            .size(px(20.))
            .flex_shrink_0()
            .child(image)
            .child(
                canvas(
                    move |bounds, _, _| bounds,
                    move |bounds, _, _, _| {
                        paints.borrow_mut().insert(path.clone(), bounds);
                    },
                )
                .absolute()
                .size_full(),
            )
            .into_any_element()
    }

    pub(super) fn tools_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.scenario.as_deref() == Some("tool-icons-diff")
            && self.tools.icon_scenario_started
            && self.tools.picker.is_none()
            && let Mode::Diff(view) = &self.engine.app.mode
            && let Some(change) = view.current_change().cloned()
        {
            // Exercise the split button's real menu handler so every configured
            // icon is painted; quick launch now intentionally shows just one.
            self.tools_action(Action::OpenDiff(change), window, cx);
        }
        if self.tools.general_requested && !self.engine.app.settings.busy {
            self.tools.general_requested = false;
            if self.engine.app.settings.open {
                self.action(
                    super::Action::SettingsTab(crate::settings::GENERAL_PAGE),
                    window,
                    cx,
                );
                cx.notify();
            }
        }
        if !matches!(
            self.scenario.as_deref(),
            Some("tool-icons" | "tool-icons-diff")
        ) || self.tools.icon_scenario_started
            || !self.tools.loaded
            || self.tools_config_busy()
        {
            return;
        }
        self.tools.icon_scenario_started = true;
        self.tools.icon_paints.borrow_mut().clear();
        self.engine.mark("desktop scenario requested");
        if self.scenario.as_deref() == Some("tool-icons-diff") {
            self.action(super::Action::Diff, window, cx);
        } else {
            self.tools_action(Action::Configure, window, cx);
        }
    }

    pub(super) fn tools_icons_scenario_ready(
        &self,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> bool {
        if !self.tools.icon_scenario_started
            || !self.tools.loaded
            || self.tools.icons_pending
            || (self.scenario.as_deref() == Some("tool-icons") && !self.tools.open)
        {
            return false;
        }
        let actual = self.tools.icons.values().flatten().count();
        if actual == 0 || self.tools.icons.len() < 2 || actual == self.tools.icons.len() {
            return false;
        }
        for entry in &self.tools.config.tools {
            let paints = self.tools.icon_paints.borrow();
            let Some(bounds) = paints.get(&entry.executable) else {
                return false;
            };
            assert!(
                bounds.size.width >= px(19.) && bounds.size.height >= px(19.),
                "tool icon must have actual layout space"
            );
            if let Some(image) = self
                .tools
                .icons
                .get(&entry.executable)
                .and_then(Option::as_ref)
                && image.clone().get_render_image(window, cx).is_none()
            {
                return false;
            }
        }
        true
    }

    pub(super) fn tools_icons_scenario_summary(&self) -> String {
        format!(
            "tool_icons_actual={} tool_icons_fallback={} tool_icons_painted={}",
            self.tools.icons.values().flatten().count(),
            self.tools
                .icons
                .values()
                .filter(|image| image.is_none())
                .count(),
            self.tools.icon_paints.borrow().len()
        )
    }
    pub(super) fn tools_ready(&self) -> bool {
        self.tools.open
            && self.tools.loaded
            && self.tools.inputs.is_some()
            && !self.tools.loading
            && !self.tools.error
            && !self.tools.icons_pending
    }
    pub(super) fn tools_uninitialized(&self) -> bool {
        !self.tools.open && self.tools.inputs.is_none()
    }
    pub(super) fn tools_startup_pending(&self) -> bool {
        !self.tools.attempted
    }
    pub(super) fn tools_config_busy(&self) -> bool {
        self.tools.loading || self.tools.saving || self.tools.icons_pending
    }
    pub(super) fn tools_startup_begin(&mut self) -> bool {
        if self.tools.attempted || self.tools.loading || self.tools.saving {
            return false;
        }
        self.tools.attempted = true;
        self.tools.loading = true;
        true
    }
    pub(super) fn tools_notice(&self) -> Option<AnyElement> {
        (!self.tools.message.is_empty()).then(|| {
            div()
                .text_color(rgb(if self.tools.error { REMOVED } else { MUTED }))
                .child(self.tools.message.clone())
                .into_any_element()
        })
    }
    pub(super) fn external_merge_busy(&self) -> bool {
        self.tools.merging
    }
    pub(super) fn tools_picker_open(&self) -> bool {
        self.tools.picker.is_some()
    }
    fn tools_editor_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tools.inputs.is_some() {
            return;
        }
        self.tools.inputs = Some([
            cx.new(|cx| InputState::new(window, cx).placeholder("Tool name")),
            cx.new(|cx| InputState::new(window, cx).placeholder("C:\\Path\\Tool.exe")),
            cx.new(|cx| {
                InputState::new(window, cx)
                    .multi_line(true)
                    .placeholder("One diff argument per line")
            }),
            cx.new(|cx| {
                InputState::new(window, cx)
                    .multi_line(true)
                    .placeholder("One merge argument per line")
            }),
        ]);
    }
    pub(super) fn load_tools(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tools.open {
            return;
        }
        self.tools_editor_inputs(window, cx);
        if self.tools.loaded || self.tools.loading {
            return;
        }
        self.tools.loading = true;
        cx.spawn_in(window, async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async { tools::load() })
                .await;
            let _ = view.update_in(cx, |v, window, cx| v.tools_hydrate(result, window, cx));
        })
        .detach();
    }
    fn capture_tools_editor(&mut self, cx: &mut Context<Self>) {
        let Some(inputs) = &self.tools.inputs else {
            return;
        };
        let Some(entry) = self
            .tools
            .draft
            .tools
            .iter_mut()
            .find(|entry| self.tools.selected.as_ref() == Some(&entry.id))
        else {
            return;
        };
        let executable = PathBuf::from(inputs[1].read(cx).value().trim().to_string());
        if !tools::same_executable(&executable, &entry.executable) {
            entry.preset = Preset::Custom;
        }
        entry.name = inputs[0].read(cx).value().to_string();
        entry.executable = executable;
        entry.diff_arguments = editor_arguments(&entry.diff_arguments, &inputs[2].read(cx).value());
        entry.merge_arguments =
            editor_arguments(&entry.merge_arguments, &inputs[3].read(cx).value());
    }
    fn populate_tools(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(inputs) = &self.tools.inputs else {
            return;
        };
        if self
            .tools
            .selected
            .as_ref()
            .is_none_or(|id| !self.tools.draft.tools.iter().any(|e| &e.id == id))
        {
            self.tools.selected = self.tools.draft.tools.first().map(|e| e.id.clone());
        }
        let entry = self
            .tools
            .draft
            .tools
            .iter()
            .find(|e| self.tools.selected.as_ref() == Some(&e.id));
        let values = [
            entry.map(|e| e.name.clone()).unwrap_or_default(),
            entry
                .map(|e| e.executable.display().to_string())
                .unwrap_or_default(),
            entry
                .and_then(|e| e.diff_arguments.as_ref())
                .map(|a| a.join("\n"))
                .unwrap_or_default(),
            entry
                .and_then(|e| e.merge_arguments.as_ref())
                .map(|a| a.join("\n"))
                .unwrap_or_default(),
        ];
        for (input, value) in inputs.iter().zip(values) {
            input.update(cx, |s, cx| s.set_value(value, window, cx));
        }
    }
    fn tools_publish_choices(&mut self) {
        let choices = |merge: bool| {
            Arc::new(
                self.tools
                    .config
                    .tools
                    .iter()
                    .filter(|e| {
                        if merge {
                            e.merge_arguments.is_some()
                        } else {
                            e.diff_arguments.is_some()
                        }
                    })
                    .map(|e| (e.id.clone(), e.name.clone()))
                    .collect(),
            )
        };
        self.tools.diff_choices = Some(choices(false));
        self.tools.merge_choices = Some(choices(true));
    }
    pub(super) fn tools_hydrate(
        &mut self,
        result: Result<Config, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.capture_tools_editor(cx);
        let dirty = self.tools.draft != self.tools.config;
        self.tools.loading = false;
        self.tools.loaded = true;
        match result {
            Ok(config) => {
                self.tools.attempted |= config.scanned;
                self.tools.config = config.clone();
                if !dirty {
                    self.tools.draft = config;
                    self.populate_tools(window, cx);
                }
                self.tools_publish_choices();
                self.tools.error = false;
            }
            Err(error) => {
                self.tools.attempted = true;
                self.tools.message = error;
                self.tools.error = true;
            }
        }
        self.refresh_tool_icons(true, cx);
        cx.notify();
    }
    pub(super) fn tools_action(
        &mut self,
        action: Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(action, Action::ClosePicker) {
            self.tools.picker = None;
            cx.notify();
            return;
        }
        if self.tools.busy {
            return;
        }
        if matches!(action, Action::Configure) {
            self.capture_tools_editor(cx);
            self.tools.picker = None;
            self.action(super::Action::Settings, window, cx);
            self.tools.general_requested = self.engine.app.settings.busy;
            if !self.tools.general_requested {
                self.action(
                    super::Action::SettingsTab(crate::settings::GENERAL_PAGE),
                    window,
                    cx,
                );
            }
            self.tools.open = true;
            self.load_tools(window, cx);
            if self.tools.loaded {
                self.populate_tools(window, cx);
            }
            cx.notify();
            return;
        }
        if self.tools.loading && !matches!(action, Action::Toggle) {
            return;
        }
        match action {
            Action::Configure => unreachable!(),
            Action::OpenDiff(change) => self.tools.picker = Some(Picker::Diff(change)),
            Action::OpenMerge(path) => self.tools.picker = Some(Picker::Merge(path)),
            Action::ClosePicker => unreachable!(),
            Action::Toggle => {
                self.capture_tools_editor(cx);
                self.tools.open = !self.tools.open;
                if self.tools.open {
                    self.load_tools(window, cx);
                    if self.tools.loaded {
                        self.populate_tools(window, cx);
                    }
                }
            }
            Action::Select(id) => {
                self.capture_tools_editor(cx);
                if self.tools.draft.tools.iter().any(|entry| entry.id == id) {
                    self.tools.selected = Some(id);
                    self.populate_tools(window, cx);
                }
            }
            Action::Add => {
                if !self.tools.open || !self.tools.loaded {
                    return;
                }
                self.capture_tools_editor(cx);
                let id = tools::new_id();
                self.tools.draft.tools.push(Entry {
                    id: id.clone(),
                    name: "New tool".into(),
                    executable: PathBuf::new(),
                    preset: Preset::Custom,
                    diff_arguments: Some(vec!["{left}".into(), "{right}".into()]),
                    merge_arguments: None,
                });
                self.tools.selected = Some(id);
                self.populate_tools(window, cx);
            }
            Action::Remove => {
                self.capture_tools_editor(cx);
                self.tools
                    .draft
                    .tools
                    .retain(|e| self.tools.selected.as_ref() != Some(&e.id));
                self.tools.selected = None;
                self.populate_tools(window, cx);
            }
            Action::Save => {
                if !self.tools.open || !self.tools.loaded {
                    return;
                }
                self.capture_tools_editor(cx);
                let config = self.tools.draft.clone();
                self.tools.busy = true;
                self.tools.saving = true;
                cx.spawn_in(window, async move |view, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move { tools::save(&config) })
                        .await;
                    let _ = view.update_in(cx, |v, window, cx| {
                        v.tools.busy = false;
                        v.tools.saving = false;
                        v.tools.error = result.is_err();
                        match result {
                            Ok(config) => {
                                v.tools.config = config.clone();
                                v.tools.draft = config;
                                v.tools_publish_choices();
                                v.refresh_tool_icons(true, cx);
                                v.populate_tools(window, cx);
                                v.tools.message = "External tools saved.".into();
                            }
                            Err(error) => v.tools.message = error,
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
            Action::Diff(id, change) => self.launch_tool(&id, Some(change), None, cx),
            Action::Merge(id, path) => self.launch_tool(&id, None, Some(path), cx),
        }
        self.refresh_tool_icons(false, cx);
        cx.notify();
    }
    fn launch_tool(
        &mut self,
        id: &str,
        change: Option<diff::FileChange>,
        merge: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.tools.picker = None;
        let merging = merge.is_some();
        let Some(tool) = self.tools.config.entry(id).and_then(|e| {
            if merging {
                e.merge_tool()
            } else {
                e.diff_tool()
            }
        }) else {
            self.tools.message =
                "This saved tool is unavailable. Choose another tool or configure the list.".into();
            self.tools.error = true;
            return;
        };
        let Some(root) = self
            .engine
            .repository_root()
            .map(std::path::Path::to_path_buf)
        else {
            return;
        };
        if merging && !self.engine.begin_repository_edit() {
            return;
        }
        self.tools.busy = true;
        self.tools.merging = merging;
        self.tools.message = if merging {
            "Merge tool open. Save and close it, then mark the file resolved."
        } else {
            "Comparison tool open…"
        }
        .into();
        self.tools.error = false;
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    if let Some(path) = merge {
                        tools::merge(&root, &path, &tool)
                    } else {
                        tools::diff(&root, &change.unwrap(), &tool)
                    }
                })
                .await;
            let _ =
                view.update(cx, |v, cx| {
                    v.tools.busy = false;
                    v.tools.merging = false;
                    if merging {
                        v.engine.end_repository_edit();
                    }
                    v.tools.error = result.is_err();
                    v.tools.message = result.err().unwrap_or_else(|| if merging {
                    "Merge tool closed. Review the result and mark the edited file resolved.".into()
                } else { "Comparison tool closed.".into() });
                    cx.notify();
                });
        })
        .detach();
    }
    pub(super) fn tools_diff_button(
        &self,
        change: &diff::FileChange,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.tools_launch_controls(Some(change.clone()), None, cx)
    }
    pub(super) fn tools_merge_button(&self, path: &str, cx: &mut Context<Self>) -> AnyElement {
        self.tools_launch_controls(None, Some(path.to_owned()), cx)
    }
    fn tools_launch_controls(
        &self,
        change: Option<diff::FileChange>,
        path: Option<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let merging = path.is_some();
        let choices = if merging {
            &self.tools.merge_choices
        } else {
            &self.tools.diff_choices
        };
        let mut controls = row().gap_0();
        if let Some(choices) = choices {
            for (index, (id, name)) in choices.iter().take(1).enumerate() {
                let Some(entry) = self.tools.config.entry(id) else {
                    continue;
                };
                let action = if let Some(path) = &path {
                    Action::Merge(id.clone(), path.clone())
                } else {
                    Action::Diff(id.clone(), change.clone().unwrap())
                };
                let button_id: SharedString =
                    format!("quick-tool-{}-{index}", path.as_deref().unwrap_or("diff")).into();
                controls = controls.child(
                    Button::new(button_id)
                        .xsmall()
                        .ghost()
                        .child(self.tool_icon(&entry.executable))
                        .label(format!(
                            "{} with {name}",
                            if merging { "Merge" } else { "Open" }
                        ))
                        .tooltip(format!(
                            "{} with {}",
                            if merging { "Merge" } else { "Compare" },
                            name
                        ))
                        .disabled(
                            self.tools.busy
                                || self.tools.loading
                                || (merging && self.engine.operation_busy),
                        )
                        .on_click(
                            cx.listener(move |v, _, w, cx| v.tools_action(action.clone(), w, cx)),
                        ),
                );
            }
        }
        controls
            .child(self.tools_picker(change, path, cx))
            .into_any_element()
    }
    fn tools_picker(
        &self,
        change: Option<diff::FileChange>,
        path: Option<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let merging = path.is_some();
        let choices = if merging {
            self.tools.merge_choices.clone()
        } else {
            self.tools.diff_choices.clone()
        };
        let id: SharedString = if let Some(path) = &path {
            format!("external-merge-{path}").into()
        } else {
            "external-diff".into()
        };
        let disabled =
            self.tools.busy || self.tools.loading || (merging && self.engine.operation_busy);
        let button = Button::new(id).xsmall().ghost().disabled(disabled);
        if choices.as_ref().is_none_or(|c| c.is_empty()) {
            return button
                .label("Configure tools…")
                .on_click(
                    cx.listener(|v, _, window, cx| v.tools_action(Action::Configure, window, cx)),
                )
                .into_any_element();
        }
        let context = path
            .map(Picker::Merge)
            .unwrap_or_else(|| Picker::Diff(change.unwrap()));
        let open = match (&self.tools.picker, &context) {
            (Some(Picker::Diff(_)), Picker::Diff(_)) => true,
            (Some(Picker::Merge(a)), Picker::Merge(b)) => a == b,
            _ => false,
        };
        let selected = self
            .tools
            .picker
            .clone()
            .filter(|_| open)
            .unwrap_or_else(|| context.clone());
        let weak = cx.entity().downgrade();
        let changed = weak.clone();
        let popover_id: SharedString = if merging {
            format!(
                "merge-picker-{}",
                match &context {
                    Picker::Merge(path) => path,
                    _ => unreachable!(),
                }
            )
            .into()
        } else {
            "diff-picker".into()
        };
        Popover::new(popover_id)
            .p_1()
            .open(open)
            .trigger(button.child(icon("down")).tooltip("Choose comparison tool"))
            .on_open_change(move |open, window, cx| {
                let _ = changed.update(cx, |v, cx| {
                    let action = if !open {
                        Action::ClosePicker
                    } else {
                        match &context {
                            Picker::Diff(change) => Action::OpenDiff(change.clone()),
                            Picker::Merge(path) => Action::OpenMerge(path.clone()),
                        }
                    };
                    v.tools_action(action, window, cx);
                });
            })
            .content(move |_, _, cx| {
                let probe = weak.upgrade().and_then(|v| v.read(cx).todo_probe(13));
                let mut menu = column()
                    .relative()
                    .w(px(220.))
                    .gap_0()
                    .p_1()
                    .children(probe);
                for (index, (id, name)) in choices.as_ref().unwrap().iter().enumerate() {
                    let tool_icon = weak.upgrade().map(|view| {
                        let view = view.read(cx);
                        view.tools
                            .config
                            .entry(id)
                            .map(|entry| view.tool_icon(&entry.executable))
                            .unwrap_or_else(|| icon("tool").into_any_element())
                    });
                    let (weak, id, selected) = (weak.clone(), id.clone(), selected.clone());
                    menu = menu.child(
                        Button::new(("external-tool-choice", index))
                            .xsmall()
                            .ghost()
                            .w_full()
                            .justify_start()
                            .child(row().gap_2().children(tool_icon).child(name.clone()))
                            .on_click(move |_, window, cx| {
                                let _ = weak.update(cx, |v, cx| {
                                    let action = match &selected {
                                        Picker::Diff(change) => {
                                            Action::Diff(id.clone(), change.clone())
                                        }
                                        Picker::Merge(path) => {
                                            Action::Merge(id.clone(), path.clone())
                                        }
                                    };
                                    v.tools_action(action, window, cx);
                                });
                            }),
                    );
                }
                let weak = weak.clone();
                menu.child(
                    Button::new("picker-configure-tools")
                        .xsmall()
                        .ghost()
                        .w_full()
                        .justify_start()
                        .label("Configure tools…")
                        .on_click(move |_, window, cx| {
                            let _ = weak
                                .update(cx, |v, cx| v.tools_action(Action::Configure, window, cx));
                        }),
                )
            })
            .into_any_element()
    }
    pub(super) fn tools_controls(&self, cx: &mut Context<Self>) -> Div {
        let locked = self.tools.busy || self.tools.loading;
        let mut panel = column()
            .w(px(700.))
            .max_w_full()
            .flex_shrink_0()
            .mt_4()
            .pt_4()
            .border_t_1()
            .border_color(rgb(LINE))
            .gap_2()
            .child(
                row()
                    .w_full()
                    .flex_shrink_0()
                    .child("External comparison tools")
                    .child(div().flex_1())
                    .child(
                        Button::new("configure-tools")
                            .label(if self.tools.open {
                                "Hide configuration"
                            } else {
                                "Configure tools…"
                            })
                            .disabled(self.tools.busy)
                            .on_click(
                                cx.listener(|v, _, w, cx| v.tools_action(Action::Toggle, w, cx)),
                            ),
                    ),
            );
        if !self.tools.open {
            return panel.children(self.tools_notice());
        }
        panel = panel.child(caption("Saved tools appear by name in the Diff and Merge menus. Changes take effect after Save.").w_full().flex_shrink_0());
        for (i, entry) in self.tools.draft.tools.iter().enumerate() {
            let id = entry.id.clone();
            panel = panel.child(
                Button::new(("tool-entry", i))
                    .child(
                        row()
                            .gap_2()
                            .child(self.tool_icon(&entry.executable))
                            .child(entry.name.clone()),
                    )
                    .tooltip(entry.executable.display().to_string())
                    .selected(self.tools.selected.as_ref() == Some(&entry.id))
                    .disabled(locked)
                    .on_click(cx.listener(move |v, _, w, cx| {
                        v.tools_action(Action::Select(id.clone()), w, cx)
                    })),
            );
        }
        panel = panel.child(
            row()
                .w_full()
                .gap_2()
                .flex_shrink_0()
                .child(
                    Button::new("add-tool")
                        .label("Add tool")
                        .disabled(locked || !self.tools.loaded)
                        .on_click(cx.listener(|v, _, w, cx| v.tools_action(Action::Add, w, cx))),
                )
                .child(
                    Button::new("remove-tool")
                        .label("Remove selected")
                        .disabled(locked || self.tools.selected.is_none())
                        .on_click(cx.listener(|v, _, w, cx| v.tools_action(Action::Remove, w, cx))),
                ),
        );
        if self.tools.draft.tools.is_empty() {
            panel = panel.child(caption(if self.tools.loading {
                "Loading tools…"
            } else {
                "No saved tools. Add a tool to configure an application."
            }));
        } else if let Some(inputs) = &self.tools.inputs {
            for (i, title) in [
                "Name",
                "Application (.exe)",
                "Diff arguments (blank disables Diff)",
                "Merge arguments (blank disables Merge)",
            ]
            .into_iter()
            .enumerate()
            {
                let height = px(if i < 2 { 32. } else { 92. });
                panel = panel.child(
                    column()
                        .w_full()
                        .flex_shrink_0()
                        .gap_1()
                        .child(title)
                        .child(
                            div()
                                .relative()
                                .w_full()
                                .h(height)
                                .flex_shrink_0()
                                .children(self.todo_probe(9 + i))
                                .child(
                                    Input::new(&inputs[i])
                                        .w_full()
                                        .h(height)
                                        .flex_shrink_0()
                                        .disabled(locked),
                                ),
                        ),
                );
            }
            panel = panel.child(caption("One argument per line; paths with spaces stay together. Diff: {left}, {right}. Merge: {left}, {base}, {right}, {output}. Include the application's wait option.").w_full().flex_shrink_0());
        }
        panel
            .child(
                Button::new("save-tools")
                    .label("Save tools")
                    .disabled(locked || !self.tools.loaded)
                    .on_click(cx.listener(|v, _, w, cx| v.tools_action(Action::Save, w, cx))),
            )
            .children(self.tools_notice())
    }
    pub(super) fn tools_scenario_entries(&self) -> Vec<(String, String)> {
        self.tools
            .config
            .tools
            .iter()
            .map(|e| (e.id.clone(), e.name.clone()))
            .collect()
    }
    pub(super) fn tools_scenario_loaded(&self) -> bool {
        self.tools.loaded && !self.tools.loading
    }
    pub(super) fn tools_scenario_scan_complete(&self) -> bool {
        self.tools.config.scanned
    }
    pub(super) fn tools_scenario_selected(&self) -> Option<String> {
        self.tools.selected.clone()
    }
    pub(super) fn tools_scenario_input(&self, index: usize, cx: &Context<Self>) -> String {
        self.tools.inputs.as_ref().expect("expanded tool editor")[index]
            .read(cx)
            .value()
            .to_string()
    }
    pub(super) fn tools_scenario_picker_ids(&self) -> Vec<String> {
        let choices = match &self.tools.picker {
            Some(Picker::Diff(_)) => &self.tools.diff_choices,
            Some(Picker::Merge(_)) => &self.tools.merge_choices,
            None => return Vec::new(),
        };
        choices
            .as_ref()
            .map(|c| c.iter().map(|(id, _)| id.clone()).collect())
            .unwrap_or_default()
    }
    pub(super) fn tools_scenario_edit_input(
        &mut self,
        index: usize,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let inputs = self.tools.inputs.as_ref().expect("expanded tool editor");
        inputs[index].update(cx, |s, cx| s.set_value(value.to_owned(), window, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::editor_arguments;

    #[test]
    fn unchanged_editor_preserves_literal_argument_boundaries_and_bytes() {
        for args in [
            vec!["--diff", "{left}", "", "  ", "line1\nline2", "{right}", ""],
            vec!["embedded\r\nnewline", "literal trailing\r"],
            vec![""],
            vec![],
        ] {
            let previous = Some(args.into_iter().map(str::to_owned).collect::<Vec<_>>());
            let text = previous.as_ref().unwrap().join("\n");
            assert_eq!(editor_arguments(&previous, &text), previous);
        }
    }

    #[test]
    fn edited_arguments_retain_whitespace_empty_and_final_arguments() {
        let previous = Some(vec!["{left}".into(), "{right}".into()]);
        assert_eq!(
            editor_arguments(&previous, "--diff\n{left}\n \n\n{right}\n"),
            Some(
                ["--diff", "{left}", " ", "", "{right}", ""]
                    .map(str::to_owned)
                    .to_vec()
            )
        );
        assert_eq!(editor_arguments(&previous, " "), Some(vec![" ".into()]));
        assert_eq!(editor_arguments(&previous, ""), None);
        assert_eq!(editor_arguments(&None, ""), None);
    }

    #[test]
    fn edited_windows_lines_normalize_only_crlf_delimiters() {
        assert_eq!(
            editor_arguments(&None, "--diff\r\n{left}\r\n{right}\r\n"),
            Some(
                ["--diff", "{left}", "{right}", ""]
                    .map(str::to_owned)
                    .to_vec()
            )
        );
        assert_eq!(
            editor_arguments(&None, "literal trailing\r"),
            Some(vec!["literal trailing\r".into()])
        );
    }
}
