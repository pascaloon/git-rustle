//! Branch-base comparisons never read Git objects on the rendering thread.
use super::*;
use crate::git::branch_base::{self, Prepared, Target};

#[derive(Default)]
pub(super) struct State {
    epoch: u64,
    pub(super) busy: bool,
    pub(super) choices: Vec<Target>,
    selected: Option<gix::ObjectId>,
    repository: String,
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
        let repository = self.engine.repository_path();
        self.base_diff.epoch = self.base_diff.epoch.wrapping_add(1);
        let epoch = self.base_diff.epoch;
        self.base_diff.busy = true;
        self.base_diff.choices.clear();
        self.base_diff.selected = Some(selected);
        self.base_diff.repository = repository.clone();
        self.context_menu = None;
        let (tx, rx) = async_channel::bounded(1);
        let path = std::path::PathBuf::from(&repository);
        self.engine.mark("branch base requested");
        std::thread::spawn(move || {
            let result = (|| {
                let target = match target {
                    Some(target) => target,
                    None => match branch_base::prepare(&path, selected)? {
                        Prepared::Default(target) => target,
                        Prepared::Choose(choices) => return Ok(Reply::Choose(choices)),
                    },
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
                    if v.selected_commits
                        .checkout_target(v.engine.app.selected_id())
                        != Some(selected)
                    {
                        v.base_diff.choices.clear();
                        cx.notify();
                        return;
                    }
                    match result {
                        Ok(Reply::Ready(base)) => {
                            v.engine.app.open_commit_pair(base, selected);
                            v.engine.tick(None);
                            v.engine.mark("branch base ready");
                        }
                        Ok(Reply::Choose(choices)) => v.base_diff.choices = choices,
                        Err(error) => v.notice = error,
                    }
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }
    pub(super) fn base_diff_dialog(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if !self.base_diff.busy && self.base_diff.choices.is_empty() {
            return None;
        }
        if self.base_diff.repository != self.engine.repository_path() {
            return None;
        }
        if self.base_diff.selected
            != self
                .selected_commits
                .checkout_target(self.engine.app.selected_id())
        {
            return None;
        }
        Some(
            dialog_overlay(
                dialog_body("Diff against branch base")
                    .child(caption(if self.base_diff.busy {
                        "Finding the common ancestor…"
                    } else {
                        "Choose the branch this work started from."
                    }))
                    .children(self.base_diff.choices.iter().map(|target| {
                        let target = target.clone();
                        div()
                            .id(SharedString::from(format!("base-{}", target.name)))
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(HOVER)))
                            .child(target.name.clone())
                            .on_click(cx.listener(move |v, _, _, cx| {
                                v.start_base_diff(Some(target.clone()), cx)
                            }))
                    }))
                    .child(
                        Button::new("cancel-base-diff")
                            .label("Cancel")
                            .on_click(cx.listener(|v, _, _, cx| {
                                v.base_diff.epoch = v.base_diff.epoch.wrapping_add(1);
                                v.base_diff.busy = false;
                                v.base_diff.choices.clear();
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element(),
        )
    }
}
