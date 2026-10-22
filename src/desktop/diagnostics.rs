//! Fixed-schema refresh observations; never record repository identifiers.
use super::*;

#[derive(Default)]
pub(super) struct State {
    previous: Option<(bool, bool, bool)>,
    last_tick: Option<Instant>,
    started: Option<Instant>,
    generation: u64,
}

impl Desktop {
    pub(super) fn diagnostic_refresh_requested(&mut self) {
        if !crate::diagnostics::enabled() {
            return;
        }
        self.refresh_diagnostics.started = Some(Instant::now());
        self.refresh_diagnostics.generation += 1;
        crate::diagnostics::event(
            "refresh_explicit_request",
            &[
                ("request", self.refresh_diagnostics.generation),
                ("branches_tab", self.engine.app.branches.open as u64),
                ("cleanup_tab", self.cleanup.open as u64),
                ("stashes_tab", self.stashes.open as u64),
                (
                    "diff_tab",
                    matches!(self.engine.app.mode, Mode::Diff(_)) as u64,
                ),
                ("provider_pending", self.provider.pending as u64),
            ],
        );
    }

    pub(super) fn diagnostic_refresh_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !crate::diagnostics::enabled() {
            return;
        }
        let now = Instant::now();
        let state = (
            self.engine.refresh_in_progress()
                || (!self.engine.has_presented() && !self.engine.repository_path().is_empty()),
            self.provider.pending,
            self.engine.app.pending.loading
                || (self.engine.app.pending.want_refresh
                    && matches!(self.engine.app.mode, Mode::Graph)),
        );
        let busy = state.0 || state.1 || state.2;
        let d = &mut self.refresh_diagnostics;
        if let Some(last) = d.last_tick.replace(now)
            && (busy || d.started.is_some())
            && now.duration_since(last) > Duration::from_millis(500)
        {
            crate::diagnostics::event(
                "refresh_ui_cadence_gap",
                &[("gap_us", now.duration_since(last).as_micros() as u64)],
            );
        }
        if d.previous.is_none() {
            crate::diagnostics::event("refresh_startup_scope", &[]);
            d.started.get_or_insert(now);
        }
        if d.previous != Some(state) {
            crate::diagnostics::event(
                "refresh_busy_transition",
                &[
                    ("engine", state.0 as u64),
                    ("provider", state.1 as u64),
                    ("pending_status", state.2 as u64),
                    (
                        "pending_queued",
                        self.engine.app.pending.want_refresh as u64,
                    ),
                ],
            );
            d.previous = Some(state);
        }
        if !busy && let Some(started) = d.started.take() {
            let generation = d.generation;
            crate::diagnostics::event(
                "refresh_workers_settled",
                &[
                    ("elapsed_us", started.elapsed().as_micros() as u64),
                    ("request", generation),
                ],
            );
            let weak = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = weak.update(cx, |v, _| {
                    if v.refresh_diagnostics.generation == generation {
                        crate::diagnostics::event(
                            "refresh_next_frame",
                            &[
                                ("request", generation),
                                ("elapsed_us", started.elapsed().as_micros() as u64),
                                ("engine_busy", v.engine.refresh_in_progress() as u64),
                                ("provider_busy", v.provider.pending as u64),
                                ("pending_status_busy", v.engine.app.pending.loading as u64),
                            ],
                        );
                    }
                });
            });
            cx.notify();
        }
    }
}
