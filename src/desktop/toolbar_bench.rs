//! Native geometry measurements for count transitions; enabled only by a scenario.
use super::*;
use std::{cell::RefCell, collections::BTreeSet, rc::Rc};
type PaintedBounds = (usize, Size<Pixels>, Bounds<Pixels>);

#[derive(Default)]
struct Probe {
    bounds: [Option<PaintedBounds>; 2],
}

#[derive(serde::Serialize)]
struct Observation {
    context: usize,
    stage: usize,
    viewport: [f32; 2],
    search: [f32; 4],
    count: [f32; 4],
}

#[derive(Default)]
pub(super) struct State {
    running: bool,
    initializing: bool,
    awaiting: bool,
    phase: usize,
    epoch: usize,
    phase_frames: usize,
    probe: Rc<RefCell<Probe>>,
    reference: [Option<[Bounds<Pixels>; 2]>; 4],
    all: Option<crate::search::Matches>,
    last_frame: Option<Instant>,
    samples: Vec<f64>,
    handlers: Vec<f64>,
    contexts: BTreeSet<usize>,
    states: BTreeSet<usize>,
    verified: bool,
    scheduled: bool,
    resize_barrier: bool,
    resize_started: Option<Instant>,
    observations: Vec<Observation>,
}

fn geometry(bounds: Bounds<Pixels>) -> [f32; 4] {
    [
        bounds.origin.x.into(),
        bounds.origin.y.into(),
        bounds.size.width.into(),
        bounds.size.height.into(),
    ]
}

impl Desktop {
    pub(super) fn toolbar_count_values(&self) -> (usize, usize) {
        let total = self.engine.app.history.len();
        let matched = self
            .engine
            .app
            .search
            .result
            .as_ref()
            .map_or(0, |r| r.rows.len());
        if self.toolbar_bench.running && self.toolbar_bench.phase / 6 >= 2 {
            // Stress a 64-bit count without fabricating billions of commits.
            let matched = match self.toolbar_bench.phase.min(23) % 6 {
                2 => usize::MAX,
                3 => usize::MAX / 2,
                5 => 1,
                _ => 0,
            };
            (usize::MAX, matched)
        } else {
            (total, matched)
        }
    }

    pub(super) fn toolbar_bounds_probe(&self, slot: usize) -> Option<AnyElement> {
        if !self.toolbar_bench.running {
            return None;
        }
        let probe = self.toolbar_bench.probe.clone();
        let epoch = self.toolbar_bench.epoch;
        Some(
            canvas(
                move |bounds, _, _| bounds,
                move |bounds, _, window, _| {
                    probe.borrow_mut().bounds[slot] = Some((epoch, window.viewport_size(), bounds));
                },
            )
            .absolute()
            .size_full()
            .into_any_element(),
        )
    }

    pub(super) fn toolbar_scenario_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toolbar_bench = State {
            running: true,
            initializing: true,
            ..State::default()
        };
        self.search_field = "";
        self.engine.mark("desktop toolbar requested");
        self.search
            .update(cx, |s, cx| s.set_value("s:", window, cx));
        cx.notify();
    }

    fn toolbar_install_stage(&mut self, stage: usize) -> f64 {
        let mut result = self
            .toolbar_bench
            .all
            .as_ref()
            .expect("all-results fixture ready")
            .clone();
        let n = match stage {
            1 => 0,
            3 => result.rows.len() / 2,
            _ => result.rows.len(),
        };
        result.rows.truncate(n);
        result.graph.truncate(n);
        for (row, position) in result.positions.iter_mut().enumerate() {
            *position = if row < n { row } else { usize::MAX };
        }
        result.done = stage == 2;
        result.examined = n;
        // Reject any late delivery from the completed query used to make the
        // fixture, then exercise the normal result-installation handler.
        let started = Instant::now();
        let app = &mut self.engine.app;
        app.search.generation += 1;
        app.search.wanted = false;
        result.generation = app.search.generation;
        assert!(app.install_search(result));
        started.elapsed().as_secs_f64() * 1000.
    }

    fn toolbar_configure_phase(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let phase = self.toolbar_bench.phase;
        let context = phase / 6;
        let stage = phase % 6;
        self.toolbar_bench.epoch += 1;
        self.toolbar_bench.phase_frames = 0;
        self.toolbar_bench.last_frame = None;
        self.toolbar_bench.probe.borrow_mut().bounds = [None, None];
        self.toolbar_bench.awaiting = matches!(stage, 1 | 4 | 5);
        if stage == 0 {
            self.toolbar_bench.resize_barrier = true;
            self.toolbar_bench.resize_started = Some(Instant::now());
            window.resize(size(
                px(if context.is_multiple_of(2) {
                    1440.
                } else {
                    900.
                }),
                px(900.),
            ));
        }
        // Fixture preparation is excluded from handler CPU and frame samples.
        let started = Instant::now();
        let handler_ms = match stage {
            0 => {
                self.search.update(cx, |s, cx| s.set_value("", window, cx));
                None
            }
            1 => {
                self.search
                    .update(cx, |s, cx| s.set_value("s:", window, cx));
                None
            }
            2 | 3 => Some(self.toolbar_install_stage(stage)),
            4 => {
                self.search.update(cx, |s, cx| {
                    s.set_value("s: zzz-no-sha-can-contain-this", window, cx)
                });
                None
            }
            5 => {
                let id = self.engine.app.history.commits[0].id.to_string();
                self.search
                    .update(cx, |s, cx| s.set_value(format!("s: {id}"), window, cx));
                None
            }
            _ => unreachable!(),
        };
        self.toolbar_bench
            .handlers
            .push(handler_ms.unwrap_or_else(|| started.elapsed().as_secs_f64() * 1000.));
        cx.notify();
    }

    pub(super) fn toolbar_scenario_schedule(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.toolbar_bench.running
            || self.toolbar_bench.verified
            || self.toolbar_bench.scheduled
        {
            return;
        }
        self.toolbar_bench.scheduled = true;
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = weak.update(cx, |v, cx| {
                v.toolbar_bench.scheduled = false;
                v.toolbar_scenario_frame(window, cx);
            });
        });
    }

    fn toolbar_scenario_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.toolbar_bench.initializing {
            let app = &self.engine.app;
            if app.branches.last_check.is_none()
                || app.branches.loading
                || app.branches.want_refresh.is_some()
                || !app
                    .search
                    .result
                    .as_ref()
                    .is_some_and(|r| r.done && r.rows.len() == app.history.len())
            {
                cx.notify();
                return;
            }
            assert!(app.history.len() >= 2, "toolbar fixture needs two commits");
            self.toolbar_bench.all = app.search.result.clone();
            self.toolbar_bench.initializing = false;
            self.toolbar_configure_phase(window, cx);
            return;
        }
        let stage = self.toolbar_bench.phase % 6;
        if self.toolbar_bench.awaiting {
            let Some(result) = self.engine.app.search.result.as_ref().filter(|r| r.done) else {
                cx.notify();
                return;
            };
            match stage {
                1 => {
                    assert_eq!(result.rows.len(), self.engine.app.history.len());
                    self.toolbar_bench.all = Some(result.clone());
                    let elapsed = self.toolbar_install_stage(1);
                    *self.toolbar_bench.handlers.last_mut().unwrap() += elapsed;
                }
                4 => assert!(result.rows.is_empty()),
                5 => assert_eq!(result.rows.len(), 1),
                _ => unreachable!(),
            }
            self.toolbar_bench.awaiting = false;
            self.toolbar_bench.last_frame = None;
            cx.notify();
            return;
        }
        let context = self.toolbar_bench.phase / 6;
        let target_width = if context.is_multiple_of(2) {
            1440.
        } else {
            900.
        };
        if let Some(started) = self.toolbar_bench.resize_started {
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "toolbar resize never produced a current paint: context={context} viewport={:?} probe={:?}",
                window.viewport_size(),
                self.toolbar_bench.probe.borrow().bounds
            );
        }
        if (f32::from(window.viewport_size().width) - target_width).abs() > 1. {
            cx.notify();
            return;
        }
        if self.toolbar_bench.resize_barrier {
            // Windows can deliver its new viewport after painting the old size.
            // Once resize has arrived, require one freshly painted probe epoch.
            self.toolbar_bench.resize_barrier = false;
            self.toolbar_bench.epoch += 1;
            self.toolbar_bench.probe.borrow_mut().bounds = [None, None];
            self.toolbar_bench.last_frame = None;
            cx.notify();
            return;
        }
        let bounds = self.toolbar_bench.probe.borrow().bounds;
        let (
            Some((search_epoch, search_viewport, search)),
            Some((count_epoch, count_viewport, count)),
        ) = (bounds[0], bounds[1])
        else {
            cx.notify();
            return;
        };
        if search_epoch != self.toolbar_bench.epoch
            || count_epoch != self.toolbar_bench.epoch
            || search_viewport != window.viewport_size()
            || count_viewport != window.viewport_size()
        {
            cx.notify();
            return;
        }
        self.toolbar_bench.resize_started = None;
        let app = &self.engine.app;
        if stage == 0 {
            assert!(!app.search.active());
        } else {
            assert!(app.search.active());
            let result = app
                .search
                .result
                .as_ref()
                .expect("prepared search state rendered");
            match stage {
                1 => {
                    assert!(!result.done);
                    assert!(result.rows.is_empty());
                }
                2 => {
                    assert!(result.done);
                    assert_eq!(result.rows.len(), app.history.len());
                }
                3 => {
                    assert!(!result.done);
                    assert_eq!(result.rows.len(), app.history.len() / 2);
                }
                4 => {
                    assert!(result.done);
                    assert!(result.rows.is_empty());
                }
                5 => {
                    assert!(result.done);
                    assert_eq!(result.rows.len(), 1);
                }
                _ => unreachable!(),
            }
        }
        assert!(search.size.width >= px(180.));
        assert!(search.right() <= count.left());
        assert!(
            count.right() <= window.viewport_size().width,
            "context={context} phase={} search={search:?} count={count:?} viewport={:?}",
            self.toolbar_bench.phase,
            window.viewport_size()
        );
        if let Some(reference) = self.toolbar_bench.reference[context] {
            assert_eq!(
                search, reference[0],
                "search moved for context={context} stage={stage}"
            );
            assert_eq!(
                count, reference[1],
                "count slot moved for context={context} stage={stage}"
            );
        } else {
            self.toolbar_bench.reference[context] = Some([search, count]);
        }
        let now = Instant::now();
        if let Some(last) = self.toolbar_bench.last_frame.replace(now) {
            self.toolbar_bench
                .samples
                .push(now.duration_since(last).as_secs_f64() * 1000.);
            self.toolbar_bench.phase_frames += 1;
            self.toolbar_bench.contexts.insert(context);
            self.toolbar_bench.states.insert(stage);
            self.toolbar_bench.observations.push(Observation {
                context,
                stage,
                viewport: [
                    window.viewport_size().width.into(),
                    window.viewport_size().height.into(),
                ],
                search: geometry(search),
                count: geometry(count),
            });
        }
        if self.toolbar_bench.phase_frames == 5 {
            self.toolbar_bench.phase += 1;
            if self.toolbar_bench.phase == 24 {
                assert_eq!(self.toolbar_bench.samples.len(), 120);
                assert_eq!(self.toolbar_bench.handlers.len(), 24);
                assert_eq!(self.toolbar_bench.observations.len(), 120);
                self.toolbar_bench.verified = true;
                self.engine
                    .mark("desktop scroll done (120 measured frames)");
                if let Some(path) = std::env::var_os("GIT_RUSTLE_FRAME_SAMPLES") {
                    let path = std::path::PathBuf::from(path);
                    std::fs::write(
                        &path,
                        serde_json::to_vec_pretty(&self.toolbar_bench.samples).unwrap(),
                    )
                    .unwrap();
                    std::fs::write(
                        path.with_extension("bounds.json"),
                        serde_json::to_vec_pretty(&self.toolbar_bench.observations).unwrap(),
                    )
                    .unwrap();
                }
            } else {
                self.toolbar_configure_phase(window, cx);
            }
        }
        cx.notify();
    }

    pub(super) fn toolbar_scenario_ready(&self) -> Option<bool> {
        (self.scenario.as_deref() == Some("toolbar-counts")).then_some(self.toolbar_bench.verified)
    }

    pub(super) fn toolbar_scenario_summary(&self) -> String {
        let search: Vec<_> = self
            .toolbar_bench
            .reference
            .iter()
            .flatten()
            .map(|b| geometry(b[0]))
            .collect();
        let count: Vec<_> = self
            .toolbar_bench
            .reference
            .iter()
            .flatten()
            .map(|b| geometry(b[1]))
            .collect();
        format!(
            "toolbar_bounds_verified={} toolbar_frames={} toolbar_contexts={} toolbar_states={} toolbar_bounds_observations={} toolbar_synthetic_large_counts=true\ntoolbar_handler_ms={}\ntoolbar_search_bounds={}\ntoolbar_count_bounds={}",
            self.toolbar_bench.verified,
            self.toolbar_bench.samples.len(),
            self.toolbar_bench.contexts.len(),
            self.toolbar_bench.states.len(),
            self.toolbar_bench.observations.len(),
            serde_json::to_string(&self.toolbar_bench.handlers).unwrap(),
            serde_json::to_string(&search).unwrap(),
            serde_json::to_string(&count).unwrap()
        )
    }
}
