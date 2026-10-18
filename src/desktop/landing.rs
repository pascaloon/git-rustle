//! The welcome screen deliberately does no repository probing while rendering.
use super::*;
use std::path::PathBuf;

impl Desktop {
    pub(super) fn open_repository(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.engine = self.engine.open_directory(path);
        self.opening_directory = true;
        self.notice.clear();
        cx.notify();
    }

    fn pick_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picking_directory {
            return;
        }
        self.picking_directory = true;
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open Directory".into()),
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = paths.await;
            let _ = view.update(cx, |v, cx| {
                v.picking_directory = false;
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            v.open_repository(path, cx);
                        }
                    }
                    Ok(Ok(None)) => {}
                    _ => v.notice = "Unable to open the folder picker. Please try again.".into(),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn landing_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        if self.scenario_started
            && !self.scenario_presented
            && !self.engine.recents_loading
            && self.scenario.as_deref() == Some("landing-forget")
        {
            let weak = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = weak.update(cx, |v, _| {
                    v.scenario_presented = true;
                    v.engine.mark("forget repository frame");
                });
            });
        }
        if self.engine.landing() && !self.engine.has_presented() {
            let weak = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = weak.update(cx, |v, cx| {
                    v.engine.mark("landing frame");
                    v.engine.presented();
                    cx.notify();
                });
            });
        }
        let loading = !self.engine.landing();
        let error = if self.opening_directory && self.engine.landing() {
            match &self.engine.app.load {
                crate::app::LoadState::Error(error) => {
                    Some(format!("Unable to open this directory: {error}"))
                }
                _ => None,
            }
        } else {
            None
        };
        column()
            .size_full()
            .items_center()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .text_size(px(14.))
            .p_6()
            .child(
                column()
                    .w(px(640.))
                    .max_w_full()
                    .h_full()
                    .gap_4()
                    .child(
                        row()
                            .justify_center()
                            .gap_4()
                            .pt_6()
                            .pb_4()
                            .child(img("rustle/app-icon.png").size(px(64.)).flex_shrink_0())
                            .child(div().text_size(px(36.)).child("Git Rustle")),
                    )
                    .child(
                        row().justify_center().child(
                            Button::new("open-directory")
                                .primary()
                                .label(if loading {
                                    "Opening…"
                                } else {
                                    "Open Directory"
                                })
                                .disabled(loading || self.picking_directory)
                                .on_click(
                                    cx.listener(|v, _, window, cx| v.pick_directory(window, cx)),
                                ),
                        ),
                    )
                    .when_some(error, |s, error| {
                        s.child(div().text_color(rgb(REMOVED)).child(error))
                    })
                    .when(!self.notice.is_empty(), |s| {
                        s.child(caption(self.notice.clone()))
                    })
                    .child(row().pt_4().child(caption("Recent repositories")))
                    .when(self.engine.recents_loading, |s| {
                        s.child(caption("Loading recent repositories…"))
                    })
                    .when(!self.engine.recents_error.is_empty(), |s| {
                        s.child(
                            div()
                                .text_color(rgb(REMOVED))
                                .child(self.engine.recents_error.clone()),
                        )
                    })
                    .when(
                        !self.engine.recents_loading && self.engine.recents.is_empty(),
                        |s| s.child(caption("Open a repository to get started.")),
                    )
                    .child(
                        column()
                            .id("recent-repositories")
                            .flex_1()
                            .overflow_y_scroll()
                            .gap_1()
                            .children(self.engine.recents.iter().enumerate().map(|(i, path)| {
                                let open_path = path.clone();
                                let forget_path = path.clone();
                                let name = path
                                    .file_name()
                                    .unwrap_or(path.as_os_str())
                                    .to_string_lossy()
                                    .into_owned();
                                row()
                                    .flex_shrink_0()
                                    .gap_1()
                                    .child(
                                        row()
                                            .id(("recent-open", i))
                                            .flex_1()
                                            .h(px(58.))
                                            .px_3()
                                            .rounded_md()
                                            .cursor_pointer()
                                            .hover(|s| s.bg(rgb(HOVER)))
                                            .child(icon("folder").text_color(rgb(ACCENT)))
                                            .child(
                                                column()
                                                    .flex_1()
                                                    .overflow_hidden()
                                                    .child(div().child(name))
                                                    .child(caption(display_path(path)).truncate()),
                                            )
                                            .on_click(cx.listener(move |v, _, _, cx| {
                                                if !v.picking_directory && !loading {
                                                    v.open_repository(open_path.clone(), cx);
                                                }
                                            })),
                                    )
                                    .child(
                                        Button::new(("recent-forget", i))
                                            .ghost()
                                            .label("×")
                                            .tooltip("Forget repository")
                                            .disabled(loading || self.engine.recents_loading)
                                            .on_click(cx.listener(move |v, _, _, cx| {
                                                v.engine.forget_repository(forget_path.clone());
                                                cx.notify();
                                            })),
                                    )
                            })),
                    ),
            )
    }
}

fn display_path(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
    }
}
