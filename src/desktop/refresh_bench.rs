//! Shared baseline/candidate graph refresh scenario. Only the controlled loopback
//! fixture is advanced; all graph work uses the normal Refresh action.
use super::*;
use std::{cell::RefCell, collections::HashMap, rc::Rc};

#[derive(Clone, Copy)]
struct Paint {
    bounds: Bounds<Pixels>,
    settled: bool,
    local: bool,
    tips: bool,
}
type Paints = Rc<RefCell<HashMap<gix::ObjectId, Paint>>>;

#[derive(Default)]
pub(super) struct State {
    phase: u8,
    scheduled: bool,
    pub(super) verified: bool,
    old: Option<gix::ObjectId>,
    expected: Option<gix::ObjectId>,
    paints: Paints,
    local_painted: bool,
    tips_painted: bool,
    branches_visited: bool,
    retained: Option<(usize, u64, Option<gix::ObjectId>)>,
}

impl Desktop {
    pub(super) fn refresh_bench_probe(&self, id: gix::ObjectId) -> Option<AnyElement> {
        if self.scenario.as_deref() != Some("refresh-native") {
            return None;
        }
        let paints = self.refresh_bench.paints.clone();
        let settled = self.refresh_bench.phase >= 2
            && !self.engine.refresh_in_progress()
            && !self.provider.pending;
        let local = self.refresh_bench.phase >= 2
            && self.engine.local_refresh_ready()
            && !self.engine.app.pending.loading
            && !self.engine.app.pending.want_refresh;
        let tips = local
            && !self.engine.app.branches.loading
            && self.engine.app.branches.want_refresh.is_none();
        Some(
            canvas(
                |bounds, _, _| bounds,
                move |bounds, _, _, _| {
                    paints.borrow_mut().insert(
                        id,
                        Paint {
                            bounds,
                            settled,
                            local,
                            tips,
                        },
                    );
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .into_any_element(),
        )
    }

    pub(super) fn refresh_bench_schedule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.scenario.as_deref() != Some("refresh-native")
            || !self.scenario_started
            || self.refresh_bench.verified
            || self.refresh_bench.scheduled
        {
            return;
        }
        self.refresh_bench.scheduled = true;
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = weak.update(cx, |v, cx| {
                v.refresh_bench.scheduled = false;
                v.refresh_bench_step(window, cx);
            });
        });
    }

    fn refresh_bench_step(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.refresh_bench.old.is_none() {
            let oid = |key| {
                gix::ObjectId::from_hex(std::env::var(key).expect("refresh fixture OID").as_bytes())
                    .expect("valid fixture OID")
            };
            self.refresh_bench.old = Some(oid("GIT_RUSTLE_REFRESH_OLD"));
            self.refresh_bench.expected = Some(oid("GIT_RUSTLE_REFRESH_EXPECTED"));
        }
        let case = std::env::var("GIT_RUSTLE_REFRESH_CASE").unwrap_or_default();
        if case == "after-branches" && !self.refresh_bench.branches_visited {
            if self.engine.refresh_in_progress()
                || self.provider.pending
                || !self
                    .engine
                    .app
                    .ghosts
                    .contains_key(&self.refresh_bench.old.unwrap())
            {
                return;
            }
            self.refresh_bench.branches_visited = true;
            self.action(Action::Tab(Tab::Branches), window, cx);
            return;
        }
        if case == "after-branches" && self.engine.app.branches.open {
            if self.engine.refresh_in_progress() {
                return;
            }
            assert!(self.engine.app.branches.remotes.values().any(|u| !u.scoped));
            self.action(Action::Tab(Tab::Graph), window, cx);
            return;
        }
        let old = self.refresh_bench.old.unwrap();
        let expected = self.refresh_bench.expected.unwrap();
        if self.refresh_bench.phase == 0 {
            if self.engine.refresh_in_progress()
                || self.provider.pending
                || !self.engine.app.ghosts.contains_key(&old)
                || !self.refresh_bench.paints.borrow().contains_key(&old)
            {
                return;
            }
            assert!(
                expected == old || !self.engine.app.ghosts.contains_key(&expected),
                "fixture advanced before Refresh"
            );
            let control =
                std::env::var("GIT_RUSTLE_REFRESH_CONTROL").expect("refresh fixture control URL");
            let url = url::Url::parse(&control).expect("fixture control URL");
            assert!(
                url.scheme() == "http"
                    && url.host_str() == Some("127.0.0.1")
                    && url.port().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none()
                    && url.path() == "/control/advance"
            );
            self.refresh_bench.phase = 1;
            cx.spawn_in(window, async move |view, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        ureq::Agent::config_builder()
                            .timeout_global(Some(Duration::from_secs(5)))
                            .build()
                            .new_agent()
                            .post(&control)
                            .send_empty()
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    })
                    .await;
                let _ = view.update_in(cx, |v, window, cx| {
                    result.expect("advance controlled refresh fixture");
                    v.refresh_bench.retained = Some((
                        v.engine.app.history.commits.as_ptr() as usize,
                        v.engine.app.labels_revision(),
                        v.engine.app.selected_id(),
                    ));
                    v.refresh_bench.paints.borrow_mut().clear();
                    v.refresh_bench.phase = 2;
                    v.engine.mark("refresh native requested");
                    let force = std::env::var("GIT_RUSTLE_REFRESH_CASE").as_deref() == Ok("full");
                    // Same modifier dispatcher used by the real toolbar and F5.
                    v.action(refresh_action(force), window, cx);
                    cx.notify();
                });
            })
            .detach();
        } else if self.refresh_bench.phase >= 2 {
            if !self.refresh_bench.local_painted
                && self.refresh_bench.paints.borrow().values().any(|p| p.local)
            {
                self.engine.mark("refresh native local frame");
                self.refresh_bench.local_painted = true;
            }
            if !self.refresh_bench.tips_painted
                && self.refresh_bench.local_painted
                && self.refresh_bench.paints.borrow().values().any(|p| p.tips)
            {
                self.engine.mark("refresh native tips frame");
                self.refresh_bench.tips_painted = true;
            }
            let painted = self.refresh_bench.paints.borrow().get(&expected).copied();
            if let Some(Paint {
                bounds, settled, ..
            }) = painted
            {
                assert!(bounds.size.width > px(0.) && bounds.size.height > px(0.));
                let viewport = self.graph_scroll.0.borrow().base_handle.bounds();
                assert!(
                    bounds.bottom() > viewport.top() && bounds.top() < viewport.bottom(),
                    "updated ghost must be painted inside the viewport"
                );
                assert!(self.engine.app.ghosts.contains_key(&expected));
                if self.refresh_bench.phase == 2 {
                    self.engine.mark("refresh native first ghost frame");
                    self.refresh_bench.phase = 3;
                    cx.notify();
                }
                if settled && !self.engine.refresh_in_progress() && !self.provider.pending {
                    if expected == old {
                        let (history, labels, selected) = self.refresh_bench.retained.unwrap();
                        assert_eq!(self.engine.app.history.commits.as_ptr() as usize, history);
                        assert_eq!(self.engine.app.labels_revision(), labels);
                        assert_eq!(self.engine.app.selected_id(), selected);
                    }
                    if case == "after-branches" {
                        assert!(self.engine.app.branches.remotes.values().any(|u| !u.scoped
                            && u.branches.iter().any(|b| b.name == "server-only-00000")));
                    }
                    assert!(self.refresh_bench.local_painted && self.refresh_bench.tips_painted);
                    self.engine.mark("refresh native settled frame");
                    self.refresh_bench.verified = true;
                    cx.notify();
                }
            } else if let Some(index) = self
                .engine
                .app
                .history
                .commits
                .iter()
                .position(|c| c.id == expected)
            {
                let pending = &self.engine.app.pending;
                self.graph_scroll.scroll_to_item(
                    index + usize::from(pending.exists() && index >= pending.row()),
                    ScrollStrategy::Top,
                );
                cx.notify();
            }
        }
    }
}
