//! Branch-base comparisons never read Git objects on the rendering thread.
use super::*;
use crate::git::branch_base::{self, Prepared, Target};

#[derive(Default)]
pub(super) struct State {
    epoch: u64,
    pub(super) busy: bool,
    pub(super) choices: Vec<Target>,
    filtered: Vec<usize>,
    query: String,
    choice: Option<usize>,
    input: Option<Entity<InputState>>,
    subscription: Option<Subscription>,
    reset_filter: bool,
    scroll: UniformListScrollHandle,
    selected: Option<gix::ObjectId>,
    branch: Option<String>,
    repository: String,
}
impl State {
    fn filter(&mut self, query: String) {
        self.query = query;
        let query = self.query.trim().to_lowercase();
        self.filtered = self
            .choices
            .iter()
            .enumerate()
            .filter(|(_, target)| target.name.to_lowercase().contains(&query))
            .map(|(index, _)| index)
            .collect();
        if self
            .choice
            .is_none_or(|choice| !self.filtered.contains(&choice))
        {
            self.choice = self.filtered.first().copied();
        }
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }
}
enum Reply {
    Ready(gix::ObjectId),
    Choose(Vec<Target>),
}
impl Desktop {
    pub(super) fn start_base_diff(&mut self, target: Option<Target>, cx: &mut Context<Self>) {
        let Some(selected) = self
            .selected_commits
            .checkout_target(self.engine.app.selected_id())
        else {
            return;
        };
        if self.engine.app.pending.selected {
            return;
        }
        self.start_base_diff_for(selected, target, cx);
    }
    pub(super) fn start_base_diff_choose(&mut self, cx: &mut Context<Self>) {
        let Some(selected) = self
            .selected_commits
            .checkout_target(self.engine.app.selected_id())
        else {
            return;
        };
        if !self.engine.app.pending.selected {
            self.start_base_diff_choose_for(selected, cx);
        }
    }
    pub(super) fn start_base_diff_for(
        &mut self,
        selected: gix::ObjectId,
        target: Option<Target>,
        cx: &mut Context<Self>,
    ) {
        self.start_base_request(selected, None, target, false, cx);
    }
    pub(super) fn start_base_diff_choose_for(
        &mut self,
        selected: gix::ObjectId,
        cx: &mut Context<Self>,
    ) {
        self.start_base_request(selected, None, None, true, cx);
    }
    /// Preserve the branch identity independently of current checkout/graph selection.
    pub(super) fn start_base_diff_branch(
        &mut self,
        selected: gix::ObjectId,
        branch: String,
        choose: bool,
        cx: &mut Context<Self>,
    ) {
        self.start_base_request(selected, Some(branch), None, choose, cx);
    }
    fn start_base_request(
        &mut self,
        selected: gix::ObjectId,
        branch: Option<String>,
        target: Option<Target>,
        choose: bool,
        cx: &mut Context<Self>,
    ) {
        let repository = self.engine.repository_path();
        self.base_diff.epoch = self.base_diff.epoch.wrapping_add(1);
        let epoch = self.base_diff.epoch;
        self.base_diff.busy = true;
        self.base_diff.choices.clear();
        self.base_diff.filtered.clear();
        self.base_diff.choice = None;
        self.base_diff.reset_filter = true;
        self.base_diff.selected = Some(selected);
        self.base_diff.branch = branch.clone();
        self.base_diff.repository = repository.clone();
        self.context_menu = None;
        let (tx, rx) = async_channel::bounded(1);
        let path = std::path::PathBuf::from(&repository);
        self.engine.mark("branch base requested");
        std::thread::spawn(move || {
            let result = (|| {
                let target = match target {
                    Some(target) => target,
                    None => {
                        match branch_base::prepare_for(&path, selected, branch.as_deref(), choose)?
                        {
                            Prepared::Default(target) => target,
                            Prepared::Choose(choices) => return Ok(Reply::Choose(choices)),
                        }
                    }
                };
                branch_base::resolve(&path, selected, &target).map(Reply::Ready)
            })();
            let _ = tx.send_blocking(result);
        });
        cx.spawn(async move |view, cx| {
            if let Ok(result) = rx.recv().await {
                let _ = view.update(cx, |v, cx| {
                    if v.base_diff.epoch != epoch || v.engine.repository_path() != repository {
                        return;
                    }
                    v.base_diff.busy = false;
                    match result {
                        Ok(Reply::Ready(base)) => {
                            v.engine.app.open_commit_pair(base, selected);
                            v.engine.app.branches.open = false;
                            v.engine.app.settings.open = false;
                            v.stashes.open = false;
                            v.cleanup.open = false;
                            v.operations.tab = None;
                            v.diff_scroll.scroll_to_item(0, ScrollStrategy::Top);
                            v.diff_horizontal.set_x(px(0.));
                            v.engine.tick(None);
                            v.engine.mark("branch base ready");
                        }
                        Ok(Reply::Choose(choices)) => {
                            v.base_diff.choices = choices;
                            v.base_diff.filter(String::new());
                        }
                        Err(error) => v.notice = error,
                    }
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }
    fn select_base(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.base_diff.filtered.contains(&index) {
            self.base_diff.choice = Some(index);
            cx.notify();
        }
    }
    fn compare_base(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.base_diff.choice else {
            return;
        };
        let Some(target) = self.base_diff.choices.get(index).cloned() else {
            return;
        };
        let Some(selected) = self.base_diff.selected else {
            return;
        };
        self.start_base_request(
            selected,
            self.base_diff.branch.clone(),
            Some(target),
            false,
            cx,
        );
    }
    pub(super) fn base_diff_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.base_diff.busy && self.base_diff.choices.is_empty() {
            return None;
        }
        if self.base_diff.repository != self.engine.repository_path() {
            return None;
        }
        if self.base_diff.input.is_none() {
            let input =
                cx.new(|cx| InputState::new(window, cx).placeholder("Filter base branches…"));
            self.base_diff.subscription =
                Some(cx.subscribe(&input, |v, input, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        v.base_diff.filter(input.read(cx).value().to_string());
                        cx.notify();
                    }
                }));
            self.base_diff.input = Some(input);
        }
        if self.base_diff.reset_filter {
            self.base_diff.reset_filter = false;
            self.base_diff
                .input
                .as_ref()
                .unwrap()
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        let list = uniform_list(
            "branch-base-choices",
            self.base_diff.filtered.len(),
            cx.processor(|v, range: std::ops::Range<usize>, _, cx| {
                range
                    .map(|row_index| {
                        let index = v.base_diff.filtered[row_index];
                        let target = &v.base_diff.choices[index];
                        row()
                            .id(("base-choice", index))
                            .w_full()
                            .h(px(34.))
                            .px_3()
                            .gap_2()
                            .bg(rgb(if v.base_diff.choice == Some(index) {
                                SELECTED
                            } else {
                                BG
                            }))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(HOVER)))
                            .child(icon("branch").text_color(rgb(MUTED)))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(target.name.clone()),
                            )
                            .child(
                                caption(target.id.to_hex_with_len(8).to_string()).flex_shrink_0(),
                            )
                            .on_click(cx.listener(move |v, _, _, cx| v.select_base(index, cx)))
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .size_full()
        .track_scroll(self.base_diff.scroll.clone());
        Some(
            dialog_overlay(
                dialog_body("Diff against branch base")
                    .relative()
                    .children(self.graph_feature_probe(0, 15))
                    .child(caption(if self.base_diff.busy {
                        "Finding the common ancestor…"
                    } else {
                        "Choose the branch this work started from."
                    }))
                    .when(!self.base_diff.busy, |s| {
                        s.child(
                            div()
                                .relative()
                                .children(self.graph_feature_probe(0, 14))
                                .child(Input::new(self.base_diff.input.as_ref().unwrap()).small()),
                        )
                        .child(caption(format!(
                            "{} matching branches",
                            self.base_diff.filtered.len()
                        )))
                        .child(
                            column()
                                .relative()
                                .h(px(280.))
                                .min_h_0()
                                .children(self.graph_feature_probe(0, 12))
                                .child(self.scroll_region(
                                    7,
                                    list,
                                    Scrollbar::vertical(&self.base_diff.scroll),
                                    cx,
                                )),
                        )
                    })
                    .child(
                        row()
                            .justify_end()
                            .gap_2()
                            .child(
                                div()
                                    .relative()
                                    .children(self.graph_feature_probe(0, 13))
                                    .child(
                                        Button::new("cancel-base-diff").label("Cancel").on_click(
                                            cx.listener(|v, _, _, cx| {
                                                v.base_diff.epoch =
                                                    v.base_diff.epoch.wrapping_add(1);
                                                v.base_diff.busy = false;
                                                v.base_diff.choices.clear();
                                                cx.notify();
                                            }),
                                        ),
                                    ),
                            )
                            .child(
                                div()
                                    .relative()
                                    .children(self.graph_feature_probe(0, 16))
                                    .child(
                                        Button::new("compare-base-diff")
                                            .primary()
                                            .label("Compare")
                                            .disabled(
                                                self.base_diff.busy
                                                    || self.base_diff.choice.is_none(),
                                            )
                                            .on_click(
                                                cx.listener(|v, _, _, cx| v.compare_base(cx)),
                                            ),
                                    ),
                            ),
                    ),
            )
            .into_any_element(),
        )
    }
    pub(super) fn base_diff_scenario_filter(
        &mut self,
        query: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(input) = self.base_diff.input.clone() else {
            return false;
        };
        input.update(cx, |input, cx| {
            input.set_value(query.to_owned(), window, cx)
        });
        true
    }
    pub(super) fn base_diff_scenario_choose(&mut self, name: &str, cx: &mut Context<Self>) {
        let index = self
            .base_diff
            .choices
            .iter()
            .position(|target| target.name == name)
            .expect("fixture base choice");
        assert!(self.base_diff.filtered.contains(&index));
        self.select_base(index, cx);
        self.compare_base(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::{State, Target};
    #[test]
    fn base_filter_keeps_all_frozen_candidates_beyond_virtual_viewport() {
        let mut state = State {
            choices: (0..4700)
                .map(|index| Target {
                    name: format!("lab/base/{index:04}"),
                    id: gix::ObjectId::from_bytes_or_panic(&[1; 20]),
                })
                .collect(),
            ..State::default()
        };
        state.filter("4699".into());
        assert_eq!(state.filtered, [4699]);
        assert_eq!(state.choices[state.filtered[0]].name, "lab/base/4699");
        state.filter(String::new());
        assert_eq!(state.filtered.len(), 4700);
    }
}
