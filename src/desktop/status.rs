//! One bounded footer; full messages are constructed only while details are open.
use super::*;
use std::borrow::Cow;

pub(super) const HEIGHT: f32 = 32.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Panel {
    Details,
    History,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    Info,
    Done,
    Working,
    Warning,
    Error,
}
impl Level {
    fn color(self) -> u32 {
        match self {
            Self::Info => MUTED,
            Self::Done => ADDED,
            Self::Working | Self::Warning => ACCENT,
            Self::Error => REMOVED,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Info => "Ready",
            Self::Done => "Done",
            Self::Working => "Working",
            Self::Warning => "Attention",
            Self::Error => "Error",
        }
    }
}
struct Message<'a> {
    source: &'static str,
    text: Cow<'a, str>,
    level: Level,
}
fn message<'a>(
    source: &'static str,
    text: impl Into<Cow<'a, str>>,
    level: Level,
) -> Option<Message<'a>> {
    let text = text.into();
    (!text.is_empty()).then_some(Message {
        source,
        text,
        level,
    })
}

// A fixed-size array keeps idle/scroll renders allocation-free until GPUI owns
// the selected label. Ties favor Git operation feedback over older checkout text.
fn messages<'a>(
    app: &'a crate::app::App,
    operation: &'a str,
    operation_error: bool,
    operation_busy: bool,
    notice: &'a str,
    closing: bool,
) -> [Option<Message<'a>>; 9] {
    use crate::app::LoadState;
    [
        match &app.load {
            LoadState::Empty | LoadState::Loading => {
                message("History", "Loading history…", Level::Working)
            }
            LoadState::Refreshing => message("History", "Refreshing history…", Level::Working),
            LoadState::Error(e) => message("History", e.as_str(), Level::Error),
            _ => None,
        },
        if app.search.busy {
            message(
                "Search",
                app.search.result.as_ref().map_or_else(
                    || Cow::Borrowed("Searching…"),
                    |r| Cow::Owned(format!("Searching… {}/{}", r.examined, r.total)),
                ),
                Level::Working,
            )
        } else {
            app.search
                .result
                .as_ref()
                .filter(|r| r.unavailable > 0 || r.error.is_some())
                .and_then(|r| {
                    message(
                        "Search",
                        r.error
                            .as_deref()
                            .unwrap_or("Search incomplete: objects unavailable"),
                        Level::Warning,
                    )
                })
        },
        if app.branches.busy {
            message("Checkout", "Checking out…", Level::Working)
        } else {
            app.checkout_notice.as_ref().and_then(|n| match n {
                Ok(text) => message("Checkout", text.as_str(), Level::Done),
                Err(text) => message("Checkout", text.as_str(), Level::Error),
            })
        },
        if app.pending.busy {
            message("Changes", "Committing changes…", Level::Working)
        } else if app.pending.error {
            message("Changes", app.pending.status.as_str(), Level::Error)
        } else {
            None
        },
        app.settings.saving.then(|| Message {
            source: "Settings",
            text: "Saving settings…".into(),
            level: Level::Working,
        }),
        app.branches.loading.then(|| Message {
            source: "Remote",
            text: "Refreshing remote branches…".into(),
            level: Level::Working,
        }),
        if app.ghost_status.ends_with(" ghost commits (remote only)") {
            None
        } else {
            let text = app.ghost_status.as_str();
            let detail = text
                .strip_prefix("Remote preview incomplete: ")
                .or_else(|| text.strip_prefix("Remote: "))
                .unwrap_or(text);
            let tracking_only =
                detail.starts_with("Local tracking refs only:") && !detail.contains(" | ");
            let level = if !tracking_only
                && (text.starts_with("Remote preview incomplete:") || text.starts_with("Remote:"))
            {
                Level::Warning
            } else {
                Level::Info
            };
            message("Remote", text, level)
        },
        message(
            "Git",
            if operation_busy {
                "Working… You can keep browsing while Git finishes."
            } else {
                operation
            },
            if operation_busy {
                Level::Working
            } else if operation_error {
                Level::Error
            } else {
                Level::Done
            },
        ),
        message(
            "App",
            notice,
            if closing {
                Level::Working
            } else {
                Level::Error
            },
        ),
    ]
}

fn primary<'a, 'b>(messages: &'b [Option<Message<'a>>]) -> Option<&'b Message<'a>> {
    messages.iter().flatten().max_by_key(|m| m.level)
}

impl Desktop {
    pub(super) fn toggle_status_panel(&mut self, panel: Panel) -> bool {
        self.status_panel = (self.status_panel != Some(panel)).then_some(panel);
        self.status_panel.is_some()
    }

    pub(super) fn dismiss_status_panel_outside(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Footer buttons handle their own click. Dismissing on their mouse-down
        // would reset the state before the click can toggle an open panel off.
        if event.position.y < window.viewport_size().height - px(HEIGHT) {
            self.status_panel = None;
            cx.notify();
        }
    }

    fn status_messages(&self) -> [Option<Message<'_>>; 9] {
        if self.engine.landing() {
            let mut messages = std::array::from_fn(|_| None);
            messages[0] = message("App", self.notice.as_str(), Level::Error);
            return messages;
        }
        messages(
            &self.engine.app,
            &self.engine.operation_status,
            self.engine.operation_error,
            self.engine.operation_busy,
            &self.notice,
            self.close_requested,
        )
    }

    pub(super) fn status_bar(&self, cx: &Context<Self>) -> AnyElement {
        let messages = self.status_messages();
        let primary = primary(&messages);
        let ready = if self.engine.landing() {
            "Ready"
        } else {
            "Repository ready"
        };
        let (text, level) = primary.map_or((ready, Level::Info), |m| (m.text.as_ref(), m.level));
        row()
            .id("status-bar")
            .h(px(HEIGHT))
            .flex_shrink_0()
            .px_3()
            .gap_3()
            .border_t_1()
            .border_color(rgb(LINE))
            .bg(rgb(PANEL))
            .text_size(px(12.))
            .children(self.update_button(cx))
            .child(
                div()
                    .size(px(6.))
                    .flex_shrink_0()
                    .rounded_full()
                    .bg(rgb(level.color())),
            )
            .when(primary.is_some(), |s| {
                s.child(
                    div()
                        .flex_shrink_0()
                        .text_color(rgb(level.color()))
                        .child(level.label()),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_ellipsis()
                    .text_color(rgb(if level == Level::Error {
                        REMOVED
                    } else {
                        MUTED
                    }))
                    .child(text.to_string()),
            )
            .child(
                Button::new("status-details")
                    .xsmall()
                    .ghost()
                    .label("Details")
                    .selected(self.status_panel == Some(Panel::Details))
                    .tooltip("Toggle status details")
                    .on_click(cx.listener(|v, _, window, cx| {
                        v.select_focus.focus(window);
                        v.toggle_status_panel(Panel::Details);
                        cx.notify();
                    })),
            )
            .child(div().w(px(1.)).h(px(12.)).bg(rgb(LINE)))
            .child(
                Button::new("operation-history")
                    .xsmall()
                    .ghost()
                    .label("History")
                    .selected(self.status_panel == Some(Panel::History))
                    .tooltip("Toggle undo / redo history")
                    .on_click(cx.listener(|v, _, w, cx| {
                        v.select_focus.focus(w);
                        v.operation_action(operations::Action::History, w, cx);
                    })),
            )
            .into_any_element()
    }

    pub(super) fn status_details(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if self.status_panel != Some(Panel::Details) {
            return None;
        }
        let messages = self.status_messages();
        Some(
            deferred(
                column()
                    .id("status-details-panel")
                    .absolute()
                    .right(px(12.))
                    .bottom(px(40.))
                    .w(px(520.))
                    .max_h(px(360.))
                    .overflow_y_scroll()
                    .p_4()
                    .gap_3()
                    .bg(rgb(PANEL))
                    .border_1()
                    .border_color(rgb(LINE))
                    .rounded_md()
                    .shadow_lg()
                    .occlude()
                    .on_mouse_down_out(cx.listener(Self::dismiss_status_panel_outside))
                    .child(
                        row().justify_between().child("Status details").child(
                            Button::new("clear-status")
                                .xsmall()
                                .ghost()
                                .label("Clear notifications")
                                .on_click(cx.listener(|v, _, _, cx| {
                                    if !v.engine.operation_busy {
                                        v.engine.operation_status.clear();
                                        v.engine.operation_error = false;
                                    }
                                    v.engine.app.checkout_notice = None;
                                    if !v.close_requested {
                                        v.notice.clear();
                                    }
                                    v.status_panel = None;
                                    cx.notify();
                                })),
                        ),
                    )
                    .when(messages.iter().all(Option::is_none), |s| {
                        s.child(caption("Ready"))
                    })
                    .children(messages.into_iter().flatten().map(|m| {
                        column()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(rgb(m.level.color()))
                                    .child(m.source),
                            )
                            .child(div().text_size(px(12.)).child(m.text.into_owned()))
                    })),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Level, messages, primary};
    #[test]
    fn errors_survive_simultaneous_success_and_background_activity() {
        let mut app = crate::app::App::new();
        app.load = crate::app::LoadState::Complete;
        app.checkout_notice = Some(Err("Checkout failed".into()));
        app.branches.loading = true;
        let items = messages(&app, "Pushed branch", false, false, "", false);
        assert_eq!(primary(&items).unwrap().text, "Checkout failed");
        assert_eq!(items.iter().flatten().count(), 3);
        app.checkout_notice = Some(Ok("Checkout completed".into()));
        let items = messages(&app, "Pushed branch", false, false, "", false);
        assert_eq!(primary(&items).unwrap().level, Level::Working);
        app.branches.loading = false;
        let items = messages(&app, "Pushed branch", false, false, "", false);
        assert_eq!(primary(&items).unwrap().text, "Pushed branch");
    }
    #[test]
    fn idle_remote_information_does_not_hide_operations_but_remote_errors_do() {
        let mut app = crate::app::App::new();
        app.load = crate::app::LoadState::Complete;
        app.ghost_status =
            "Remote: Local tracking refs only: origin (provider not configured)".into();
        let items = messages(&app, "Pushed branch", false, false, "", false);
        // Provider configuration is informational even with the Remote prefix.
        assert_eq!(primary(&items).unwrap().text, "Pushed branch");
        app.ghost_status =
            "Remote preview incomplete: Local tracking refs only: origin (provider not configured)"
                .into();
        app.checkout_notice = Some(Ok("Checkout topic completed.".into()));
        let items = messages(&app, "", false, false, "", false);
        assert_eq!(primary(&items).unwrap().text, "Checkout topic completed.");
        app.ghost_status.push_str(" | upstream: permission denied");
        let items = messages(&app, "", false, false, "", false);
        assert_eq!(primary(&items).unwrap().level, Level::Warning);
        app.ghost_status = "Remote preview incomplete: permission denied".into();
        let items = messages(&app, "Pushed branch", false, false, "", false);
        assert_eq!(primary(&items).unwrap().level, Level::Warning);
    }
}
