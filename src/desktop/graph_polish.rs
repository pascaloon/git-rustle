//! Optional native paint evidence; absent from ordinary graph rendering.
use super::*;
use std::{cell::RefCell, collections::HashMap, rc::Rc};
type BadgePaints = Rc<RefCell<HashMap<String, (bool, Bounds<Pixels>)>>>;
#[derive(Clone, Default)]
pub(super) struct PaintTrace(Rc<RefCell<(usize, usize, bool)>>);
impl PaintTrace {
    pub(super) fn begin(&self, _: Bounds<Pixels>, _: bool) {
        *self.0.borrow_mut() = (0, 0, false);
    }
    pub(super) fn connection(&self) {
        let mut p = self.0.borrow_mut();
        p.0 += 1;
        p.2 |= p.1 > 0;
    }
    pub(super) fn node(&self) {
        self.0.borrow_mut().1 += 1;
    }
}
#[derive(Default)]
pub(super) struct State {
    pub(super) verified: bool,
    scheduled: bool,
    prepared: bool,
    traces: RefCell<HashMap<usize, PaintTrace>>,
    badges: BadgePaints,
}
impl Desktop {
    pub(super) fn graph_polish_trace(&self, row: usize) -> Option<PaintTrace> {
        (self.scenario.as_deref() == Some("graph-polish")).then(|| {
            self.graph_polish
                .traces
                .borrow_mut()
                .entry(row)
                .or_default()
                .clone()
        })
    }
    pub(super) fn graph_polish_badge_probe(
        &self,
        name: &str,
        remote_only: bool,
    ) -> Option<AnyElement> {
        if self.scenario.as_deref() != Some("graph-polish") {
            return None;
        }
        let badges = self.graph_polish.badges.clone();
        let name = name.to_owned();
        Some(
            canvas(
                |b, _, _| b,
                move |b, _, _, _| {
                    badges.borrow_mut().insert(name.clone(), (remote_only, b));
                },
            )
            .absolute()
            .size_full()
            .into_any_element(),
        )
    }
    pub(super) fn graph_polish_schedule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.scenario.as_deref() != Some("graph-polish")
            || !self.scenario_started
            || self.graph_polish.verified
            || self.graph_polish.scheduled
        {
            return;
        }
        self.graph_polish.scheduled = true;
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = weak.update(cx, |v, cx| {
                v.graph_polish.scheduled = false;
                if !v.engine.app.history.complete || !v.engine.app.pending.known {
                    cx.notify();
                    return;
                }
                if !v.graph_polish.prepared {
                    let root = v.engine.repository_root().unwrap();
                    assert_eq!(
                        std::fs::read_to_string(root.join(".git/git-rustle-graph-fixture"))
                            .unwrap()
                            .trim(),
                        "disposable graph feature fixture"
                    );
                    assert!(root.join(".git/refs/remotes/origin/fetched").is_file());
                    assert!(!root.join(".git/refs/remotes/origin/server-only").exists());
                    let index = (0..v.engine.app.history.len())
                        .find(|i| v.engine.app.history.subject(*i) == "Merge dense graph fixture")
                        .unwrap();
                    let id = v.engine.app.history.commits[index].id;
                    // The API-only label is a fixture overlay, just as dense-labels
                    // supplies synthetic labels. The fetched label has a real ref.
                    v.engine.app.labels.insert(
                        id,
                        vec![
                            crate::app::Label {
                                name: "main".into(),
                                kind: crate::git::RefKind::Branch,
                                remote_only: false,
                            },
                            crate::app::Label {
                                name: "origin/fetched".into(),
                                kind: crate::git::RefKind::Remote,
                                remote_only: false,
                            },
                            crate::app::Label {
                                name: "origin/server-only".into(),
                                kind: crate::git::RefKind::Remote,
                                remote_only: true,
                            },
                        ],
                    );
                    let row = (0..v.engine.app.graph_len())
                        .find(|row| v.engine.app.history_index(*row) == Some(index))
                        .unwrap();
                    v.action(Action::Select(row, false), window, cx);
                    v.graph_scroll.scroll_to_item(0, ScrollStrategy::Top);
                    v.graph_polish.prepared = true;
                    v.graph_polish.traces.borrow_mut().clear();
                    v.engine.mark("graph polish requested");
                    cx.notify();
                    return;
                }
                let traces = v.graph_polish.traces.borrow();
                if traces.is_empty() {
                    cx.notify();
                    return;
                }
                assert!(
                    traces.values().any(|t| t.0.borrow().1 > 0),
                    "Graph nodes must actually paint"
                );
                assert!(
                    traces.values().all(|t| !t.0.borrow().2),
                    "All connections must paint before nodes"
                );
                let badges = v.graph_polish.badges.borrow();
                let (Some(fetched), Some(server)) = (
                    badges.get("origin/fetched"),
                    badges.get("origin/server-only"),
                ) else {
                    cx.notify();
                    return;
                };
                assert!(!fetched.0 && server.0);
                for (_, bounds) in [fetched, server] {
                    assert!(
                        (f32::from(bounds.size.width)-14.).abs()<=1. && (f32::from(bounds.size.height)-14.).abs()<=1.,
                        "Remote icon must paint at 14px within 1 logical pixel of fractional DPI rounding on both axes: {bounds:?}"
                    );
                }
                v.graph_polish.verified = true;
                v.engine.mark("graph polish verified frame");
                cx.notify();
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::PaintTrace;
    #[test]
    fn paint_trace_rejects_connections_after_nodes() {
        let trace = PaintTrace::default();
        trace.connection();
        trace.node();
        assert!(!trace.0.borrow().2);
        trace.connection();
        assert!(trace.0.borrow().2);
    }
}
