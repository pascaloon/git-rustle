//! Opt-in native checks for actual input focus, filter rendering and HEAD paint.
use super::*;
use std::{cell::RefCell, rc::Rc};

#[derive(Clone, Copy, serde::Serialize)]
struct Rendered {
    epoch: usize,
    active: bool,
    focused: bool,
    border: u32,
    background: u32,
}

#[derive(Default)]
struct Probe {
    inputs: [Option<Rendered>; 2],
    head_painted: bool,
}

pub(super) struct Observer {
    probe: Rc<RefCell<Probe>>,
    epoch: usize,
    slot: usize,
}

impl Observer {
    pub(super) fn record(self, active: bool, focused: bool, border: u32, background: u32) {
        self.probe.borrow_mut().inputs[self.slot] = Some(Rendered {
            epoch: self.epoch,
            active,
            focused,
            border,
            background,
        });
    }
}

#[derive(Default)]
pub(super) struct State {
    running: bool,
    scheduled: bool,
    verified: bool,
    phase: usize,
    epoch: usize,
    started: Option<Instant>,
    probe: Option<Rc<RefCell<Probe>>>,
    observations: Vec<Rendered>,
    owner_query: String,
    owner_verified: bool,
}

impl Desktop {
    pub(super) fn filter_probe(&self, slot: usize) -> Option<Observer> {
        self.filter_bench.probe.as_ref().map(|probe| Observer {
            probe: probe.clone(),
            epoch: self.filter_bench.epoch,
            slot,
        })
    }

    pub(super) fn filter_head_probe(&self) -> Option<AnyElement> {
        let probe = self.filter_bench.probe.as_ref()?.clone();
        Some(
            canvas(
                move |bounds, _, _| bounds,
                move |bounds, _, _, _| {
                    if bounds.size.width > px(0.) && bounds.size.height > px(0.) {
                        probe.borrow_mut().head_painted = true;
                    }
                },
            )
            .absolute()
            .size_full()
            .into_any_element(),
        )
    }

    pub(super) fn filter_scenario_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter_bench = State {
            running: true,
            started: Some(Instant::now()),
            probe: Some(Rc::new(RefCell::new(Probe::default()))),
            ..State::default()
        };
        self.engine.mark("desktop filter requested");
        self.search_field = "";
        self.action(Action::Head, window, cx);
        let id = self
            .engine
            .app
            .head_id()
            .expect("filter fixture requires HEAD");
        self.search.update(cx, |s, cx| {
            s.set_value(format!("s: {id}"), window, cx);
            s.focus(window, cx);
        });
        cx.notify();
    }

    pub(super) fn filter_scenario_schedule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.filter_bench.running || self.filter_bench.verified || self.filter_bench.scheduled {
            return;
        }
        self.filter_bench.scheduled = true;
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = weak.update(cx, |v, cx| {
                v.filter_bench.scheduled = false;
                v.filter_scenario_frame(window, cx);
            });
        });
    }

    fn filter_scenario_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        assert!(
            self.filter_bench.started.unwrap().elapsed() < Duration::from_secs(30),
            "filter visual scenario stalled at phase {}",
            self.filter_bench.phase
        );
        let phase = self.filter_bench.phase;
        if phase == 4 && self.filter_bench.owner_query.is_empty() {
            let author = self.engine.app.branches.entries.iter().find_map(|entry| {
                let author = &entry.activity.as_ref()?.author;
                (!author.is_empty()
                    && !entry
                        .target
                        .label()
                        .to_lowercase()
                        .contains(&author.to_lowercase()))
                .then(|| author.to_lowercase())
            });
            let Some(author) = author else {
                cx.notify();
                return;
            };
            self.filter_bench.owner_query = author.clone();
            self.filter_bench.epoch += 1;
            self.branch_filter.update(cx, |s, cx| {
                s.set_value(author, window, cx);
                s.focus(window, cx);
            });
            cx.notify();
            return;
        }
        let slot = usize::from(phase >= 4);
        let input = if slot == 0 {
            &self.search
        } else {
            &self.branch_filter
        };
        let active = matches!(phase, 0 | 1 | 4 | 5);
        let focused = matches!(phase, 0 | 4);
        if input.read(cx).focus_handle(cx).is_focused(window) != focused {
            cx.notify();
            return;
        }
        let ready = match phase {
            0 | 1 => self
                .engine
                .app
                .search
                .result
                .as_ref()
                .is_some_and(|r| r.done && r.rows.len() == 1),
            2 => self.engine.app.search.text.value == "a:",
            3 => !self.engine.app.search.active(),
            4 | 5 => self.engine.app.branches.query == self.filter_bench.owner_query,
            6 => self.engine.app.branches.query.is_empty(),
            _ => unreachable!(),
        };
        if !ready {
            cx.notify();
            return;
        }
        let probe = self.filter_bench.probe.as_ref().unwrap().borrow();
        let Some(rendered) = probe.inputs[slot].filter(|p| p.epoch == self.filter_bench.epoch)
        else {
            drop(probe);
            cx.notify();
            return;
        };
        if phase == 0 && !probe.head_painted {
            drop(probe);
            cx.notify();
            return;
        }
        drop(probe);
        assert_eq!(
            rendered.active, active,
            "filter active style at phase {phase}"
        );
        assert_eq!(rendered.focused, focused);
        assert_eq!(
            rendered.border,
            if focused {
                ACCENT
            } else if active {
                FILTER_ACTIVE_BORDER
            } else {
                LINE
            }
        );
        assert_eq!(
            rendered.background,
            if active { FILTER_ACTIVE_BG } else { BG }
        );
        if matches!(phase, 4 | 5) {
            let browser = &self.engine.app.branches;
            let query = self.filter_bench.owner_query.to_lowercase();
            assert!(!browser.visible.is_empty());
            assert!(browser.visible.iter().all(|&i| {
                let entry = &browser.entries[i];
                entry.target.label().to_lowercase().contains(&query)
                    || entry
                        .activity
                        .as_ref()
                        .is_some_and(|a| a.author.to_lowercase().contains(&query))
            }));
            assert!(
                browser.visible.iter().any(|&i| {
                    let entry = &browser.entries[i];
                    !entry.target.label().to_lowercase().contains(&query)
                        && entry
                            .activity
                            .as_ref()
                            .is_some_and(|a| a.author.to_lowercase().contains(&query))
                }),
                "owner query must exercise an author-only match"
            );
            self.filter_bench.owner_verified = true;
        }
        if phase == 6 {
            assert_eq!(
                self.engine.app.branches.visible.len(),
                self.engine.app.branches.entries.len()
            );
        }
        self.filter_bench.observations.push(rendered);
        self.filter_bench.phase += 1;
        self.filter_bench.epoch += 1;
        match phase {
            0 | 4 => self.select_focus.focus(window),
            1 => {
                self.search
                    .update(cx, |s, cx| s.set_value("a:", window, cx));
            }
            2 => {
                self.search.update(cx, |s, cx| s.set_value("", window, cx));
            }
            3 => self.action(Action::Branches, window, cx),
            5 => {
                self.branch_filter
                    .update(cx, |s, cx| s.set_value("", window, cx));
            }
            6 => {
                self.filter_bench.verified = true;
            }
            _ => unreachable!(),
        }
        cx.notify();
    }

    pub(super) fn filter_scenario_ready(&self) -> Option<bool> {
        (self.scenario.as_deref() == Some("filter-visuals")).then_some(self.filter_bench.verified)
    }

    pub(super) fn filter_scenario_assert(&self) {
        assert!(self.filter_bench.verified);
        assert_eq!(self.filter_bench.observations.len(), 7);
        assert!(self.filter_bench.owner_verified);
        assert!(
            self.filter_bench
                .probe
                .as_ref()
                .unwrap()
                .borrow()
                .head_painted
        );
    }

    pub(super) fn filter_scenario_summary(&self) -> String {
        format!(
            "filter_visual_verified={} filter_visual_states={} branch_owner_verified={} head_badge_painted={}\nfilter_render_observations={}",
            self.filter_bench.verified,
            self.filter_bench.observations.len(),
            self.filter_bench.owner_verified,
            self.filter_bench
                .probe
                .as_ref()
                .is_some_and(|p| p.borrow().head_painted),
            serde_json::to_string(&self.filter_bench.observations).unwrap()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{search_has_terms, search_query};

    #[test]
    fn search_indicator_uses_effective_terms_including_selected_field_and_unicode() {
        for (field, text, expected) in [
            ("", "", false),
            ("m: ", "  ", false),
            ("", "m:", false),
            ("a: ", " f: \t", false),
            ("s: ", "abc123", true),
            ("", "a: ÅNGSTRÖM", true),
            ("", "M:", true),
            ("", "日本語", true),
        ] {
            assert_eq!(
                search_has_terms(&search_query(field, text)),
                expected,
                "field={field:?} text={text:?}"
            );
        }
    }
}
