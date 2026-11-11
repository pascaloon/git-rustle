//! Settings share one content grid, compact groups, and a persistent save bar.
use super::*;

pub(super) fn group(symbol: &'static str, title: &'static str, body: Div) -> Div {
    column()
        .w_full()
        .flex_shrink_0()
        .rounded_lg()
        .border_1()
        .border_color(rgb(palette().line))
        .bg(rgb(palette().panel))
        .overflow_hidden()
        .child(
            section_heading(symbol, title)
                .w_full()
                .h(px(28.))
                .flex_shrink_0()
                .px_3()
                .text_size(px(13.))
                .bg(rgb(palette().control)),
        )
        .child(divider())
        .child(body)
}

/// One visual language for mutually exclusive Settings choices.
pub(super) fn segments() -> Div {
    row()
        .w(px(324.))
        .max_w_full()
        .gap_1()
        .p_1()
        .rounded_lg()
        .border_1()
        .border_color(rgb(palette().line))
        .bg(rgb(palette().bg))
}

pub(super) fn segment(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    disabled: bool,
) -> Button {
    Button::new(id)
        .label(label)
        .ghost()
        .selected(selected)
        .flex_1()
        .min_w_0()
        .h(px(32.))
        .disabled(disabled)
}

pub(super) fn field(
    id: &'static str,
    label: &'static str,
    help: &'static str,
    control: impl IntoElement,
) -> Div {
    row()
        .w_full()
        .items_start()
        .gap_4()
        .py_2()
        .child(
            div()
                .id(id)
                .w(px(148.))
                .flex_shrink_0()
                .pt_1()
                .text_color(rgb(palette().muted))
                .child(label)
                .tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(help).build(w, cx)),
        )
        .child(div().flex_1().min_w_0().child(control))
}

pub(super) fn divider() -> Div {
    div()
        .w_full()
        .h(px(1.))
        .flex_shrink_0()
        .bg(rgb(palette().line))
}

fn content_width() -> Div {
    column().w_full().max_w(px(880.)).flex_shrink_0()
}

impl Desktop {
    pub(super) fn settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let settings = &self.engine.app.settings;
        let page = settings.page;
        let mut form = content_width().gap_4();
        if page == ai::SETTINGS_PAGE {
            form = form.child(self.ai_settings(cx));
        } else if page == crate::settings::GENERAL_PAGE {
            form = form
                .gap_3()
                .child(group(
                    "settings",
                    "Preferences",
                    settings_body()
                        .p_2()
                        .child(self.appearance_controls(cx))
                        .child(divider())
                        .child(
                            div()
                                .relative()
                                .w_full()
                                .child(self.tools_controls(cx))
                                .children(self.todo_probe(3)),
                        ),
                ))
                .child(group(
                    "image",
                    "Maintenance",
                    settings_body().p_2().child(
                        self.image_cache_controls(cx)
                            .relative()
                            .children(self.general_settings_probe(1)),
                    ),
                ))
                .child(group(
                    "info",
                    "About Git Rustle",
                    settings_body()
                        .p_2()
                        .child(
                            self.update_controls(cx)
                                .relative()
                                .children(self.general_settings_probe(0)),
                        )
                        .child(divider())
                        .child(
                            row()
                                .relative()
                                .justify_end()
                                .gap_3()
                                .children(self.ui_feature_probe(3))
                                .child(
                                    Button::new("project-website")
                                        .small()
                                        .ghost()
                                        .icon(
                                            gpui_component::Icon::default()
                                                .path("rustle/external-link"),
                                        )
                                        .tooltip("Open the Git Rustle website")
                                        .label("Website")
                                        .on_click(|_, _, cx| {
                                            cx.open_url("https://pascaloon.github.io/git-rustle/")
                                        }),
                                )
                                .child(
                                    Button::new("project-releases")
                                        .small()
                                        .ghost()
                                        .icon(
                                            gpui_component::Icon::default().path("rustle/history"),
                                        )
                                        .tooltip("Open release notes in your browser")
                                        .label("Release notes")
                                        .on_click(|_, _, cx| {
                                            cx.open_url(
                                                "https://github.com/pascaloon/git-rustle/releases",
                                            )
                                        }),
                                ),
                        ),
                ));
        } else if page == 0 {
            let repository = self.engine.repository_path();
            let mut connection = settings_body()
                .child(field(
                    "repository-path-help",
                    "Location",
                    "This repository's working directory.",
                    div()
                        .id("settings-repository-path")
                        .text_ellipsis()
                        .child(repository.clone())
                        .tooltip(move |w, cx| {
                            gpui_component::tooltip::Tooltip::new(repository.clone()).build(w, cx)
                        }),
                ))
                .child(divider())
                .child(field(
                    "remote-help",
                    "Remote",
                    "Choose the remote whose provider settings you want to edit.",
                    row()
                        .flex_wrap()
                        .when(settings.projects.is_empty(), |r| {
                            r.child(caption(if settings.busy {
                                "Loading…"
                            } else {
                                "No remotes configured"
                            }))
                        })
                        .children(settings.projects.iter().enumerate().map(|(i, d)| {
                            Button::new(("remote", i))
                                .small()
                                .label(d.name.clone())
                                .selected(settings.project_selected == i)
                                .disabled(settings.busy)
                                .tooltip(d.url.clone())
                                .on_click(cx.listener(move |v, _, w, cx| {
                                    v.action(Action::SettingsRemote(i), w, cx)
                                }))
                        })),
                ));
            if let Some(d) = settings.projects.get(settings.project_selected) {
                let url = d.url.clone();
                connection = connection
                    .child(field(
                        "remote-url-help",
                        "URL",
                        "Git transport URL configured for this remote.",
                        div()
                            .id("settings-remote-url")
                            .text_ellipsis()
                            .child(url.clone())
                            .tooltip(move |w, cx| {
                                gpui_component::tooltip::Tooltip::new(url.clone()).build(w, cx)
                            }),
                    ))
                    .child(field(
                        "project-provider-help",
                        "Provider",
                        "Auto detects the hosting service from this remote.",
                        segments().children(
                            [
                                ("Auto", None),
                                ("GitLab", Some("gitlab")),
                                ("GitHub", Some("github")),
                            ]
                            .into_iter()
                            .enumerate()
                            .map(|(i, (label, provider))| {
                                segment(
                                    ("project-provider", i),
                                    label,
                                    d.provider.as_deref() == provider,
                                    settings.busy,
                                )
                                .on_click(cx.listener(
                                    move |v, _, w, cx| {
                                        v.action(Action::SettingsProvider(provider), w, cx)
                                    },
                                ))
                            }),
                        ),
                    ))
                    .child(field(
                        "project-path-help",
                        "Project path",
                        "Use group/project for GitLab or owner/repository for GitHub.",
                        themed_input(&self.project).disabled(settings.busy),
                    ));
            }
            form = form
                .child(group("folder", "Repository", connection))
                .child(group(
                    "branch",
                    "Reference storage",
                    settings_body().child(
                        div()
                            .relative()
                            .child(self.ref_storage_controls(cx))
                            .children(self.todo_probe(4)),
                    ),
                ));
        } else if let Some(index) = settings.provider_index() {
            let draft = &settings.providers[index];
            form = form
                .child(group("cloud", "Connection", settings_body()
                    .child(field("provider-api-help", "API URL", "Service API endpoint. Use your organization's endpoint for a self-hosted service.",
                        themed_input(&self.api).disabled(settings.busy)))))
                .child(group("shield-check", "Authentication", settings_body()
                    .child(field("token-help", "Access token", "Stored in Windows Credential Manager. Leave a saved token masked to keep it. Environment tokens and Git credentials are also supported.",
                        column().gap_2().child(themed_input(&self.token).disabled(settings.busy))
                            .child(row().child(caption(if draft.forget { "Token removal pending" } else if draft.has_token { "Token saved" } else { "No custom token" }).flex_1())
                                .when(draft.has_token, |r| r.child(self.button("forget", if draft.forget { "Keep saved token" } else { "Remove saved token" }, Action::ForgetToken, cx))))))));
        }

        let header = content_width()
            .relative()
            .children(self.general_settings_probe(2))
            .child(
                row()
                    .w_full()
                    .child(div().flex_1().text_size(px(20.)).child("Settings"))
                    .child(
                        tab_bar().px_0().children(
                            [
                                (
                                    crate::settings::GENERAL_PAGE,
                                    "General",
                                    "Preferences for all repositories",
                                ),
                                (0, "Project", "Settings for the current repository"),
                                (1, "GitLab", "GitLab credentials shared by all repositories"),
                                (2, "GitHub", "GitHub credentials shared by all repositories"),
                                (
                                    ai::SETTINGS_PAGE,
                                    "AI",
                                    "AI preferences shared by all repositories",
                                ),
                            ]
                            .into_iter()
                            .map(|(target, label, help)| {
                                tab_item(("settings-tab", target), page == target, !settings.busy)
                                    .child(label)
                                    .tooltip(move |w, cx| {
                                        gpui_component::tooltip::Tooltip::new(help).build(w, cx)
                                    })
                                    .when(!settings.busy, |s| {
                                        s.on_click(cx.listener(move |v, _, w, cx| {
                                            v.action(Action::SettingsTab(target), w, cx)
                                        }))
                                    })
                            }),
                        ),
                    ),
            );
        let footer = if page == ai::SETTINGS_PAGE {
            self.ai_settings_footer(cx)
        } else {
            row()
                .w_full()
                .gap_4()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(12.))
                        .text_color(rgb(if settings.error {
                            palette().removed
                        } else {
                            palette().muted
                        }))
                        .child(settings.status.clone()),
                )
                .child(self.button(
                    "save",
                    if settings.saving {
                        "Saving…"
                    } else {
                        "Save changes"
                    },
                    Action::SaveSettings,
                    cx,
                ))
        };
        column()
            .w_full()
            .flex_1()
            .min_h_0()
            .child(
                column()
                    .w_full()
                    .items_center()
                    .px_4()
                    .py_2()
                    .flex_shrink_0()
                    .child(header),
            )
            .child(
                column()
                    .id("settings-page")
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .items_center()
                    .px_4()
                    .pb_4()
                    .when_some(self.todo_settings_scroll(), |s, scroll| {
                        s.track_scroll(&scroll)
                    })
                    .child(form.relative().children(self.general_settings_probe(3))),
            )
            .when(page != crate::settings::GENERAL_PAGE, |s| {
                s.child(
                    column()
                        .w_full()
                        .items_center()
                        .px_4()
                        .py_3()
                        .flex_shrink_0()
                        .border_t_1()
                        .border_color(rgb(palette().line))
                        .child(content_width().child(footer)),
                )
            })
            .into_any_element()
    }
}
