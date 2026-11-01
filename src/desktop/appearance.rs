//! The same actions drive the General theme segments and unattended checks.
use super::*;
use crate::appearance::ThemeChoice;

type SegmentPaint = (Bounds<Pixels>, bool);
const TRANSITIONS: [ThemeChoice; 6] = [
    ThemeChoice::Dark,
    ThemeChoice::Light,
    ThemeChoice::Black,
    ThemeChoice::Light,
    ThemeChoice::Dark,
    ThemeChoice::Black,
];

#[derive(Default)]
pub(super) struct State {
    pub busy: bool,
    error: String,
    phase: usize,
    scheduled: bool,
    started: Option<Instant>,
    samples: Vec<f64>,
    bounds: std::rc::Rc<std::cell::Cell<Option<Bounds<Pixels>>>>,
    segments: std::rc::Rc<std::cell::Cell<[Option<SegmentPaint>; 3]>>,
}

impl Desktop {
    pub(super) fn appearance_settings_page(&self) -> Option<usize> {
        match self.scenario.as_deref() {
            Some("settings" | "theme-toggle") => Some(crate::settings::GENERAL_PAGE),
            Some("settings-project") => Some(0),
            Some("settings-gitlab") => Some(1),
            Some("settings-github") => Some(2),
            _ => None,
        }
    }

    pub(super) fn appearance_scenario_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.scenario_started
            && self.engine.app.settings.open
            && !self.engine.app.settings.busy
            && let Some(page) = self.appearance_settings_page()
            && self.engine.app.settings.page != page
        {
            self.action(Action::SettingsTab(page), window, cx);
        }
    }

    pub(super) fn set_appearance_theme(
        &mut self,
        theme: ThemeChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous = current_theme();
        if self.appearance.busy || theme == previous {
            return;
        }
        self.appearance.busy = true;
        self.appearance.error.clear();
        crate::desktop_theme::apply(theme, cx);
        window.refresh();
        // Persist off the UI thread. Disable repeated writes until this finishes.
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { crate::appearance::Preferences { theme }.save() })
                .await;
            let _ = view.update(cx, |v, cx| {
                v.appearance.busy = false;
                if let Err(error) = result {
                    crate::desktop_theme::apply(previous, cx);
                    v.appearance.error = error;
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn appearance_controls(&self, cx: &Context<Self>) -> Div {
        let bounds = self.appearance.bounds.clone();
        let selected = current_theme();
        let scenario = self.scenario.as_deref() == Some("theme-toggle");
        column()
            .relative()
            .w_full()
            .flex_shrink_0()
            .gap_3()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette().line))
            .bg(rgb(palette().panel))
            .child(
                column()
                    .gap_1()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Appearance"))
                    .child(caption("Choose your theme. Saved for every repository.")),
            )
            .child(
                row()
                    .w(px(324.))
                    .gap_1()
                    .p_1()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(palette().line))
                    .bg(rgb(palette().bg))
                    .children(
                        ThemeChoice::ALL
                            .into_iter()
                            .enumerate()
                            .map(|(index, choice)| {
                                let segments = self.appearance.segments.clone();
                                let active = choice == selected;
                                div()
                                    .relative()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        Button::new(("appearance-theme", index))
                                            .label(choice.label())
                                            .ghost()
                                            .selected(active)
                                            .w_full()
                                            .h(px(32.))
                                            .disabled(self.appearance.busy)
                                            .tooltip(match choice {
                                                ThemeChoice::Black => "Pure black · Default",
                                                ThemeChoice::Dark => "Charcoal · VS Code inspired",
                                                ThemeChoice::Light => "Warm paper",
                                            })
                                            .on_click(cx.listener(move |v, _, window, cx| {
                                                v.action(
                                                    Action::AppearanceTheme(choice),
                                                    window,
                                                    cx,
                                                );
                                            })),
                                    )
                                    .when(scenario, |s| {
                                        s.child(
                                            canvas(
                                                |bounds, _, _| bounds,
                                                move |bounds, _, _, _| {
                                                    let mut painted = segments.get();
                                                    painted[index] = Some((bounds, active));
                                                    segments.set(painted);
                                                },
                                            )
                                            .absolute()
                                            .top_0()
                                            .left_0()
                                            .size_full(),
                                        )
                                    })
                            }),
                    ),
            )
            .when(!self.appearance.error.is_empty(), |s| {
                s.child(
                    div()
                        .text_color(rgb(palette().removed))
                        .child(self.appearance.error.clone()),
                )
            })
            .when(scenario, |s| {
                s.child(
                    canvas(
                        |bounds, _, _| bounds,
                        move |value, _, _, _| bounds.set(Some(value)),
                    )
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
                )
            })
    }

    pub(super) fn appearance_scenario_schedule(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.scenario.as_deref() != Some("theme-toggle")
            || !self.scenario_started
            || self.engine.app.settings.busy
            || self.engine.app.settings.page != crate::settings::GENERAL_PAGE
            || self.appearance.scheduled
            || self.appearance.phase == 9
        {
            return;
        }
        self.appearance.scheduled = true;
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = weak.update(cx, |v, cx| {
                v.appearance.scheduled = false;
                if v.appearance.busy {
                    cx.notify();
                    return;
                }
                let phase = v.appearance.phase;
                let expected = if phase == 0 || phase == 7 {
                    ThemeChoice::Black
                } else if phase == 8 {
                    ThemeChoice::Dark
                } else {
                    TRANSITIONS[phase - 1]
                };
                let painted = v.appearance.segments.get();
                // Await the selected segment's actual paint after persistence or rollback.
                if painted.iter().enumerate().any(|(i, p)| {
                    p.is_none_or(|(_, selected)| selected != (ThemeChoice::ALL[i] == expected))
                }) {
                    cx.notify();
                    return;
                }
                assert_eq!(current_theme(), expected);
                assert_eq!(
                    crate::appearance::Preferences::load().unwrap().theme,
                    expected
                );
                assert_eq!(
                    gpui_component::Theme::global(cx).mode == gpui_component::ThemeMode::Light,
                    expected == ThemeChoice::Light
                );
                let background: Hsla = rgb(palette().bg).into();
                assert_eq!(gpui_component::Theme::global(cx).background, background);
                assert_eq!(
                    gpui_component::Theme::global(cx)
                        .highlight_theme
                        .style
                        .editor_background,
                    Some(rgb(palette().input_bg).into())
                );
                let bounds = v.appearance.bounds.get().expect("Appearance card painted");
                assert!(bounds.size.width >= px(400.) && bounds.size.height >= px(60.));
                assert!(bounds.left() >= px(0.) && bounds.right() <= window.viewport_size().width);
                assert!(bounds.top() >= px(0.) && bounds.bottom() <= window.viewport_size().height);
                for (index, paint) in painted.into_iter().enumerate() {
                    let (segment, _) = paint.unwrap();
                    assert!(segment.size.width >= px(90.) && segment.size.height >= px(32.));
                    assert!(segment.left() >= bounds.left() && segment.right() <= bounds.right());
                    assert!(segment.top() >= bounds.top() && segment.bottom() <= bounds.bottom());
                    if index > 0 {
                        assert!(painted[index - 1].unwrap().0.right() <= segment.left());
                    }
                }
                if (1..=6).contains(&phase) {
                    v.appearance
                        .samples
                        .push(v.appearance.started.take().unwrap().elapsed().as_secs_f64() * 1000.);
                }
                if phase == 8 {
                    assert!(v.appearance.error.is_empty());
                    v.appearance.phase = 9;
                    v.engine
                        .mark(format!("theme toggle verified {:?}", v.appearance.samples));
                } else if phase == 7 {
                    assert!(
                        !v.appearance.error.is_empty(),
                        "failed save must be reported"
                    );
                    let file = crate::appearance::settings_file().unwrap();
                    std::fs::remove_dir(file.with_extension("json.tmp")).unwrap();
                    v.appearance.phase = 8;
                    v.action(Action::AppearanceTheme(ThemeChoice::Dark), window, cx);
                } else {
                    assert!(v.appearance.error.is_empty(), "{}", v.appearance.error);
                    let next = if phase == 6 {
                        // Restrict failure injection to runner-owned isolated application data.
                        let file = crate::appearance::settings_file().unwrap();
                        assert_eq!(
                            std::fs::read_to_string(file.with_extension("fixture")).unwrap(),
                            "disposable appearance scenario"
                        );
                        std::fs::create_dir(file.with_extension("json.tmp")).unwrap();
                        ThemeChoice::Dark
                    } else {
                        TRANSITIONS[phase]
                    };
                    v.appearance.phase += 1;
                    v.appearance.started = Some(Instant::now());
                    v.action(Action::AppearanceTheme(next), window, cx);
                }
                cx.notify();
            });
        });
    }

    pub(super) fn appearance_scenario_ready(&self) -> bool {
        self.appearance.phase == 9
    }
}
