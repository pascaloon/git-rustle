//! The same action drives the General switch and unattended round-trip checks.
use super::*;
use gpui_component::switch::Switch;

#[derive(Default)]
pub(super) struct State {
    pub busy: bool,
    error: String,
    phase: usize,
    scheduled: bool,
    started: Option<Instant>,
    samples: Vec<f64>,
    bounds: std::rc::Rc<std::cell::Cell<Option<Bounds<Pixels>>>>,
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

    pub(super) fn set_light_theme(
        &mut self,
        light: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.appearance.busy || light == is_light() {
            return;
        }
        self.appearance.busy = true;
        self.appearance.error.clear();
        crate::desktop_theme::apply(light, cx);
        window.refresh();
        // Persist off the UI thread. Disable repeated writes until this finishes.
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { crate::appearance::Preferences { light_theme: light }.save() })
                .await;
            let _ = view.update(cx, |v, cx| {
                v.appearance.busy = false;
                if let Err(error) = result {
                    crate::desktop_theme::apply(!light, cx);
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
        column()
            .relative()
            .w_full()
            .flex_shrink_0()
            .gap_2()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette().line))
            .bg(rgb(palette().panel))
            .child(
                row()
                    .justify_between()
                    .child(
                        column()
                            .gap_1()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child("Appearance"))
                            .child(caption(
                                "Warm surfaces, clear contrast. Saved for every repository.",
                            )),
                    )
                    .child(
                        Switch::new("light-theme")
                            .label("Light theme")
                            .checked(is_light())
                            .disabled(self.appearance.busy)
                            .on_click(cx.listener(|v, light, window, cx| {
                                v.action(Action::LightTheme(*light), window, cx);
                            })),
                    ),
            )
            .when(!self.appearance.error.is_empty(), |s| {
                s.child(
                    div()
                        .text_color(rgb(palette().removed))
                        .child(self.appearance.error.clone()),
                )
            })
            .when(self.scenario.as_deref() == Some("theme-toggle"), |s| {
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
            || self.appearance.phase == 4
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
                assert!(v.appearance.error.is_empty(), "{}", v.appearance.error);
                let bounds = v.appearance.bounds.get().expect("Appearance card painted");
                assert!(bounds.size.width >= px(400.) && bounds.size.height >= px(60.));
                assert!(bounds.left() >= px(0.) && bounds.right() <= window.viewport_size().width);
                assert!(bounds.top() >= px(0.) && bounds.bottom() <= window.viewport_size().height);
                if phase > 0 {
                    let light = phase % 2 == 1;
                    assert_eq!(is_light(), light);
                    assert_eq!(
                        gpui_component::Theme::global(cx).mode == gpui_component::ThemeMode::Light,
                        light
                    );
                    assert_eq!(
                        crate::appearance::Preferences::load().unwrap().light_theme,
                        light
                    );
                    v.appearance
                        .samples
                        .push(v.appearance.started.take().unwrap().elapsed().as_secs_f64() * 1000.);
                }
                if phase == 3 {
                    v.appearance.phase = 4;
                    v.engine
                        .mark(format!("theme toggle verified {:?}", v.appearance.samples));
                } else {
                    // This fixture always starts in dark mode; each transition uses the switch action.
                    if phase == 0 {
                        assert!(!is_light());
                    }
                    v.appearance.phase += 1;
                    v.appearance.started = Some(Instant::now());
                    v.action(Action::LightTheme(phase % 2 == 0), window, cx);
                }
                cx.notify();
            });
        });
    }

    pub(super) fn appearance_scenario_ready(&self) -> bool {
        self.appearance.phase == 4
    }
}
