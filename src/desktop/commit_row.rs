//! One commit-row renderer shared by the graph and workflow commit lists.
use super::*;

impl Desktop {
    pub(super) fn commit_row_shell(
        id: impl Into<ElementId>,
        selected: bool,
        compact: bool,
    ) -> Stateful<Div> {
        row()
            .id(id)
            .w_full()
            .h(px(if compact { 58. } else { 32. }))
            .px_2()
            .pr(px(if compact { 12. } else { 24. }))
            .gap_2()
            .bg(rgb(if selected { SELECTED } else { BG }))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(HOVER)))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn commit_row_contents(
        &self,
        mut r: Stateful<Div>,
        id: gix::ObjectId,
        ghost: bool,
        subject: &str,
        author: &str,
        commit_time: i64,
        row_index: usize,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let app = &self.engine.app;
        if compact {
            return r
                .child(
                    div()
                        .w(px(66.))
                        .flex_shrink_0()
                        .font_family("Cascadia Mono")
                        .text_size(px(12.))
                        .text_color(rgb(ACCENT))
                        .child(id.to_hex_with_len(8).to_string()),
                )
                .child(
                    column()
                        .flex_1()
                        .gap_1()
                        .overflow_hidden()
                        .child(div().text_ellipsis().child(subject.to_owned()))
                        .child(
                            row()
                                .text_size(px(11.))
                                .text_color(rgb(MUTED))
                                .child(requests::avatar(self.avatars.image(&id), author, 18.))
                                .child(div().flex_1().text_ellipsis().child(author.to_owned()))
                                .child(crate::ui::time::format_relative(
                                    commit_time,
                                    crate::ui::time::now_unix(),
                                )),
                        ),
                );
        }
        let node_lane = if app.search.active() {
            app.search
                .result
                .as_ref()
                .and_then(|r| r.graph.get(row_index))
                .map_or(0, |row| row.1 as usize)
        } else {
            app.history_index(row_index)
                .and_then(|index| app.history.commits.get(index))
                .map_or(0, |record| record.node_lane as usize)
        };
        let on_path = self.graph_selected_path.node_on_path(row_index, node_lane);
        r = r.child(
            div()
                .w(px(62.))
                .flex()
                .items_center()
                .relative()
                .children(self.graph_feature_probe(row_index, 0))
                .font_family("Cascadia Mono")
                .flex_shrink_0()
                .text_color(rgb(if ghost { MUTED } else { ACCENT }))
                .child(
                    div()
                        .when(on_path, |s| {
                            s.bg(rgb(0x674125))
                                .text_color(rgb(ACCENT_HOVER))
                                .rounded_sm()
                        })
                        .child(self.matches.styled(id.to_hex_with_len(8).to_string())),
                ),
        );
        let mut middle = row()
            .relative()
            .children(self.graph_feature_probe(row_index, 1))
            .flex_1()
            .min_w_0()
            .overflow_hidden();
        let mut badges = row().max_w(relative(0.65)).min_w_0().overflow_hidden();
        if app.head_id() == Some(id) {
            badges = badges.child(
                row()
                    .id(("head-badge", row_index))
                    .relative()
                    .h(px(22.))
                    .gap_1()
                    .px_2()
                    .rounded_full()
                    .flex_shrink_0()
                    .bg(rgb(ACCENT))
                    .text_color(rgb(BG))
                    .text_size(px(12.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .children(self.filter_head_probe())
                    .child(icon("head").size(px(14.)).text_color(rgb(BG)))
                    .child("HEAD")
                    .tooltip(|window, cx| {
                        gpui_component::tooltip::Tooltip::new("Current checkout (HEAD)")
                            .build(window, cx)
                    }),
            );
        }
        if let Some(labels) = app.labels.get(&id) {
            for (label_index, label) in labels
                .iter()
                .filter(|label| label.name != "HEAD")
                .take(3)
                .enumerate()
            {
                let remote = label.kind == crate::git::RefKind::Remote;
                let color = if remote { REMOTE_TEXT } else { SEARCH_TEXT };
                badges = badges
                    .child(
                        row()
                            .id(("ref-badge", label_index))
                            .tooltip({
                                let name = label.name.clone();
                                move |w, cx| {
                                    gpui_component::tooltip::Tooltip::new(name.clone()).build(w, cx)
                                }
                            })
                            .gap_1()
                            .rounded_sm()
                            .px_1()
                            .flex_shrink_0()
                            .bg(rgb(if remote { REMOTE_BG } else { SELECTED }))
                            .text_color(rgb(color))
                            .max_w(px(170.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .when(label.remote_only, |badge| {
                                badge.child(icon("cloud").size(px(14.)).text_color(rgb(color)))
                            })
                            .child(self.matches.styled(label.name.clone())),
                    )
                    .children(self.provider_badge(id, label.kind, &label.name, cx));
            }
        }
        middle = middle.child(badges).child(
            div()
                .flex_1()
                .min_w_0()
                .text_ellipsis()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_color(rgb(if ghost { MUTED } else { TEXT }))
                .child(self.matches.styled(subject.to_owned())),
        );
        r = r
            .child(middle)
            .child(
                row()
                    .w(px(150.))
                    .relative()
                    .children(self.graph_feature_probe(row_index, 2))
                    .flex_shrink_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_color(rgb(MUTED))
                    .child(requests::avatar(
                        self.avatars
                            .image(&id)
                            .or(self.avatars.scenario_image.as_ref()),
                        author,
                        22.,
                    ))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .text_ellipsis()
                            .child(self.matches.styled(author.to_owned())),
                    ),
            )
            .child(
                div()
                    .w(px(62.))
                    .relative()
                    .children(self.graph_feature_probe(row_index, 3))
                    .flex_shrink_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_right()
                    .text_size(px(12.))
                    .text_color(rgb(MUTED))
                    .child(crate::ui::time::format_relative(
                        commit_time,
                        crate::ui::time::now_unix(),
                    )),
            );

        r
    }

    pub(super) fn conflict_commit_row(
        &self,
        commit: &crate::git::operations::conflicts::Summary,
        selected: bool,
        side: usize,
        index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = commit.id;
        let parent = commit.parents.first().copied();
        self.commit_row_contents(
            Self::commit_row_shell(
                (
                    if side == 0 {
                        "incoming-commit"
                    } else {
                        "local-commit"
                    },
                    index,
                ),
                selected,
                true,
            ),
            id,
            false,
            &commit.subject,
            &commit.author_name,
            commit.commit_time,
            index,
            true,
            cx,
        )
        .on_click(
            cx.listener(move |v, _, window, cx| v.select_conflict_commit(id, parent, window, cx)),
        )
        .into_any_element()
    }

    pub(super) fn select_conflict_commit(
        &mut self,
        id: gix::ObjectId,
        parent: Option<gix::ObjectId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.engine.app.park_diff();
        self.engine.app.inspect_commit(Some((id, parent)));
        self.changed(cx);
        self.sync_inputs(window, cx);
    }

    pub(super) fn conflict_details(&self, cx: &mut Context<Self>) -> AnyElement {
        self.details_panel(true, cx)
    }
}
