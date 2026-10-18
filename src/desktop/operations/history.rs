//! Compact recovery timeline. Closed panels do no list work.
use super::*;

impl Desktop {
    fn history_enabled(&self) -> bool {
        !self.engine.operation_busy
            && !self.engine.app.pending.busy
            && !self.engine.app.branches.busy
            && !self.engine.app.settings.saving
            && self.engine.operations.conflict.is_none()
            && !self.engine.operations.interactive_rebase
    }
    pub(super) fn history_panel(&self, cx: &Context<Self>) -> AnyElement {
        let view = &self.engine.operations;
        let count = view.labels.len();
        let enabled = self.history_enabled() && !self.operations.history_clear;
        let mut panel = column()
            .id("operation-history-panel")
            .absolute()
            .right(px(12.))
            .bottom(px(42.))
            .w(px(440.))
            .p_4()
            .gap_3()
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgb(LINE))
            .rounded_lg()
            .shadow_lg()
            .occlude()
            .on_mouse_down_out(cx.listener(Self::dismiss_status_panel_outside))
            .child(
                row()
                    .gap_3()
                    .child(icon("history").text_color(rgb(ACCENT)))
                    .child(div().flex_1().text_size(px(16.)).child("Undo history"))
                    .child(caption(format!(
                        "{count} {}",
                        if count == 1 { "step" } else { "steps" }
                    )))
                    .child(
                        Button::new("close-history")
                            .xsmall()
                            .ghost()
                            .child(icon("close"))
                            .tooltip("Close history")
                            .on_click(cx.listener(|v, _, window, cx| {
                                v.operation_action(Action::CloseHistory, window, cx)
                            })),
                    ),
            )
            .child(
                row()
                    .gap_2()
                    .child(
                        Button::new("history-undo")
                            .small()
                            .ghost()
                            .child(icon("undo"))
                            .label("Undo")
                            .disabled(!enabled || view.cursor == 0)
                            .on_click(cx.listener(|v, _, window, cx| {
                                let cursor = v.engine.operations.cursor;
                                v.operation_action(
                                    Action::Run(Request::MoveTo(cursor.saturating_sub(1))),
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .child(
                        Button::new("history-redo")
                            .small()
                            .ghost()
                            .child(icon("redo"))
                            .label("Redo")
                            .disabled(!enabled || view.cursor >= count)
                            .on_click(cx.listener(|v, _, window, cx| {
                                v.operation_action(
                                    Action::Run(Request::MoveTo(v.engine.operations.cursor + 1)),
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("history-clear")
                            .small()
                            .ghost()
                            .label("Clear…")
                            .disabled(!enabled || count == 0)
                            .on_click(cx.listener(|v, _, window, cx| {
                                v.operation_action(Action::ReviewClear, window, cx)
                            })),
                    ),
            );
        if self.operations.history_clear {
            panel = panel.child(
                column()
                    .gap_2()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(CONTROL))
                    .child(
                        div()
                            .text_size(px(14.))
                            .child("Clear undo and redo history?"),
                    )
                    .child(caption("Your files and commits stay as they are."))
                    .child(
                        row()
                            .justify_end()
                            .child(
                                Button::new("cancel-clear-history")
                                    .small()
                                    .ghost()
                                    .label("Cancel")
                                    .on_click(cx.listener(|v, _, window, cx| {
                                        v.operation_action(Action::CancelClear, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("confirm-clear-history")
                                    .small()
                                    .danger()
                                    .label("Clear history")
                                    .disabled(!self.history_enabled())
                                    .on_click(cx.listener(|v, _, window, cx| {
                                        v.operation_action(Action::ClearHistory, window, cx)
                                    })),
                            ),
                    ),
            );
        } else if count == 0 {
            panel = panel.child(
                column()
                    .h(px(120.))
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(icon("history").size(px(24.)).text_color(rgb(MUTED)))
                    .child(div().text_size(px(14.)).child("No undo history"))
                    .child(caption("New actions will appear here.")),
            );
        } else {
            let height = (count + 1).min(6) as f32 * 44.;
            let list = uniform_list(
                "undo-timeline",
                count + 1,
                cx.processor(|v, range: std::ops::Range<usize>, _, cx| {
                    v.operations.history_visible = v.operations.history_visible.max(range.len());
                    range.map(|row| v.history_row(row, cx)).collect::<Vec<_>>()
                }),
            )
            .flex_1()
            .h_full()
            .track_scroll(self.operations.history_scroll.clone());
            panel = panel
                .child(
                    row().h(px(height)).gap_0().child(list).child(
                        Scrollbar::vertical(&self.operations.history_scroll)
                            .scrollbar_show(ScrollbarShow::Always),
                    ),
                )
                .child(caption("Double-click to restore").text_size(px(11.)));
        }
        if !view.boundary.is_empty() {
            let detail = view.boundary.clone();
            panel = panel.child(
                caption(view.boundary.clone())
                    .id("history-boundary")
                    .truncate()
                    .text_size(px(11.))
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(detail.clone()).build(window, cx)
                    }),
            );
        }
        panel.into_any_element()
    }
    fn history_row(&self, row_index: usize, cx: &Context<Self>) -> AnyElement {
        let view = &self.engine.operations;
        let position = view.labels.len() - row_index;
        let label = position
            .checked_sub(1)
            .map_or("Initial state", |i| view.labels[i].as_str());
        let current = position == view.cursor;
        let future = position > view.cursor;
        let enabled = self.history_enabled() && !self.operations.history_clear && !current;
        let tooltip = label.to_string();
        row()
            .id(("history-position", position))
            .relative()
            .w_full()
            .h(px(44.))
            .px_3()
            .pr(px(28.))
            .gap_3()
            .rounded_md()
            .bg(rgb(if current { SELECTED } else { PANEL }))
            .when(enabled, |r| r.cursor_pointer().hover(|s| s.bg(rgb(HOVER))))
            .child(
                div()
                    .w(px(14.))
                    .h_full()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .relative()
                    .when(row_index > 0, |r| {
                        r.child(
                            div()
                                .absolute()
                                .top_0()
                                .left(px(6.))
                                .w(px(1.))
                                .h(px(16.))
                                .bg(rgb(LINE)),
                        )
                    })
                    .when(position > 0, |r| {
                        r.child(
                            div()
                                .absolute()
                                .bottom_0()
                                .left(px(6.))
                                .w(px(1.))
                                .h(px(16.))
                                .bg(rgb(LINE)),
                        )
                    })
                    .child(
                        div()
                            .size(px(if current { 8. } else { 6. }))
                            .rounded_full()
                            .bg(rgb(if current { ACCENT } else { MUTED })),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .truncate()
                    .text_size(px(13.))
                    .text_color(rgb(if future { MUTED } else { TEXT }))
                    .child(label.to_string()),
            )
            .when(current, |r| {
                r.child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgb(ACCENT))
                        .child("Current"),
                )
            })
            .when(future, |r| r.child(caption("Redo").text_size(px(11.))))
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
            })
            .on_click(cx.listener(move |v, event: &ClickEvent, window, cx| {
                if event.click_count() == 2
                    && v.history_enabled()
                    && !v.operations.history_clear
                    && position != v.engine.operations.cursor
                {
                    v.operation_action(Action::Run(Request::MoveTo(position)), window, cx);
                }
            }))
            .into_any_element()
    }
    pub(in crate::desktop) fn history_scenario_scroll(&mut self, step: usize) {
        self.operations.history_scroll.scroll_to_item(
            (step * 7) % (self.engine.operations.labels.len() + 1),
            ScrollStrategy::Top,
        );
    }
    pub(in crate::desktop) fn history_scenario_ready(&self) -> bool {
        self.status_panel == Some(status::Panel::History)
            && self.history_enabled()
            && !self.engine.operations.labels.is_empty()
    }
}
