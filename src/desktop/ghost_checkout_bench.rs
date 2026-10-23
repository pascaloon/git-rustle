//! Opt-in graph checkout proof using real metadata from a marked local bare remote.
use super::*;
use crate::{git, model::History, remote};
use std::{
    cell::Cell,
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
};

const MARKER: &str = "disposable ghost checkout fixture";

struct Api {
    stop: Arc<AtomicBool>,
    requests: Arc<AtomicUsize>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Api {
    fn start() -> (String, Self) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/fixture/branches", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let (stopped, count) = (stop.clone(), requests.clone());
        let worker = thread::spawn(move || {
            let mut total_requests = 0;
            while !stopped.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                total_requests += 1;
                assert!(total_requests <= 16, "Fixture HTTP request loop");
                // The inspector may ask for the selected tip and its local
                // parent while the modal is open. Both have an empty API diff
                // in this checkout-only fixture; Git owns the real file proof.
                let body = if request.starts_with("GET /fixture/commits/") {
                    count.fetch_add(1, Ordering::Release);
                    r#"{"files":[]}"#
                } else if request.starts_with("GET /fixture/tags?per_page=100&page=1 ") {
                    "[]"
                } else {
                    panic!("Unexpected fixture HTTP request: {request}");
                };
                for _ in 0..100 {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap() == 0 || header == "\r\n" {
                        break;
                    }
                }
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        (
            endpoint,
            Self {
                stop,
                requests,
                worker: Some(worker),
            },
        )
    }
}
impl Drop for Api {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let result = self.worker.take().unwrap().join();
        if !thread::panicking() {
            result.unwrap();
        }
    }
}

struct Seed {
    preview: remote::history::Preview,
    api: Api,
}

#[derive(Clone, Copy)]
struct GraphPaint {
    head: Option<gix::ObjectId>,
    ghost: bool,
}

struct Run {
    case: &'static str,
    source: PathBuf,
    branch: String,
    tip: gix::ObjectId,
    phase: u8,
    scheduled: bool,
    started: Instant,
    verified: bool,
    checks: usize,
    operation_epoch: u64,
    modal: Rc<Cell<Option<Bounds<Pixels>>>>,
    graph: Rc<Cell<Option<GraphPaint>>>,
    api: Option<Api>,
}

#[derive(Default)]
pub(super) struct State {
    run: Option<Box<Run>>,
}

fn seed(
    root: &std::path::Path,
    source: &std::path::Path,
    branch: &str,
    tip: gix::ObjectId,
    existing: bool,
) -> Seed {
    assert_eq!(
        std::fs::read_to_string(root.join(".git/git-rustle-ghost-checkout-fixture")).unwrap(),
        MARKER
    );
    assert_eq!(
        std::fs::read_to_string(source.join("git-rustle-ghost-checkout-fixture")).unwrap(),
        MARKER
    );
    let local = git::open(root).unwrap();
    let before = git::snapshot_refs(&local).unwrap();
    assert_ne!(before.head_id, Some(tip));
    assert!(
        local.find_object(tip).is_err(),
        "remote tip must be absent before checkout"
    );
    assert_eq!(
        before.refs.iter().any(
            |r| matches!(r.kind, git::RefKind::Branch | git::RefKind::Head) && r.name == branch
        ),
        existing
    );
    let url = String::from_utf8(
        git::command::checked(root, &["config", "--get", "remote.origin.url"]).unwrap(),
    )
    .unwrap();
    assert_eq!(
        url.trim_end(),
        source.to_str().unwrap(),
        "fixture origin must be the marked bare remote"
    );
    let repo = git::open(source).unwrap();
    assert!(repo.workdir().is_none(), "fixture source must be bare");
    let refs = git::snapshot_refs(&repo).unwrap();
    assert!(refs.refs.len() <= 16);
    assert!(refs.refs.iter().any(|r| r.name == branch && r.id == tip));
    let mut history = History::default();
    git::walk(&repo, &refs.tips, &mut |batch| {
        history.append(batch);
        history.len() <= 32
    })
    .unwrap();
    assert!(
        history.complete && history.len() <= 32,
        "native fixture must remain small"
    );
    let (endpoint, api) = Api::start();
    let provider = remote::Remote {
        name: "origin".into(),
        url: source.to_str().unwrap().into(),
        provider: remote::Provider::Github,
        endpoint,
        project: "disposable/ghost-checkout".into(),
    };
    let mut ghosts = HashMap::new();
    let mut sources = HashMap::new();
    for record in &history.commits {
        if local.find_object(record.id).is_err() {
            ghosts.insert(record.id, git::commit_details(&repo, &record.id).unwrap());
            sources.insert(record.id, provider.clone());
        }
    }
    assert!(ghosts.contains_key(&tip));
    assert_eq!(
        ghosts.len(),
        1,
        "fixture advances exactly one remote commit"
    );
    Seed {
        api,
        preview: remote::history::Preview {
            history: Some(history),
            ghosts,
            sources,
            branches: vec![(tip, format!("origin/{branch}"))],
            tags: Vec::new(),
            errors: Vec::new(),
        },
    }
}

impl Desktop {
    pub(super) fn ghost_checkout_start(&mut self, cx: &mut Context<Self>) {
        let case = match self.scenario.as_deref() {
            Some("ghost-checkout-existing") => "existing",
            Some("ghost-checkout-new") => "new",
            Some("ghost-checkout-detached") => "detached",
            Some("ghost-checkout-menu") => "menu",
            _ => return,
        };
        assert!(self.ghost_checkout_bench.run.is_none());
        self.ghost_checkout_bench.run = Some(Box::new(Run {
            case,
            source: std::env::var_os("GIT_RUSTLE_GHOST_CHECKOUT_SOURCE")
                .expect("marked bare source")
                .into(),
            branch: std::env::var("GIT_RUSTLE_GHOST_CHECKOUT_BRANCH").expect("fixture branch"),
            tip: gix::ObjectId::from_hex(
                std::env::var("GIT_RUSTLE_GHOST_CHECKOUT_SHA")
                    .expect("fixture tip")
                    .as_bytes(),
            )
            .unwrap(),
            phase: 0,
            scheduled: false,
            started: Instant::now(),
            verified: false,
            checks: 0,
            operation_epoch: self.engine.operation_epoch,
            modal: Rc::new(Cell::new(None)),
            graph: Rc::new(Cell::new(None)),
            api: None,
        }));
        cx.notify();
    }

    pub(super) fn ghost_checkout_schedule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(run) = self
            .ghost_checkout_bench
            .run
            .as_mut()
            .filter(|r| !r.verified && !r.scheduled)
        else {
            return;
        };
        run.scheduled = true;
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = weak.update(cx, |v, cx| {
                v.ghost_checkout_bench.run.as_mut().unwrap().scheduled = false;
                v.ghost_checkout_tick(window, cx);
                cx.notify();
            });
        });
    }

    fn ghost_checkout_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let run = self.ghost_checkout_bench.run.as_ref().unwrap();
        assert!(
            run.started.elapsed() < Duration::from_secs(45),
            "ghost checkout stalled in phase {}",
            run.phase
        );
        match run.phase {
            0 if self.engine.external_tools_idle() => {
                assert!(
                    !self.engine.app.pending.exists(),
                    "checkout fixture starts clean"
                );
                let root = self.engine.repository_root().unwrap().to_path_buf();
                let (source, branch, tip, existing) = (
                    run.source.clone(),
                    run.branch.clone(),
                    run.tip,
                    run.case == "existing",
                );
                self.ghost_checkout_bench.run.as_mut().unwrap().phase = 1;
                cx.spawn_in(window, async move |view, cx| {
                    let target_branch = branch.clone();
                    let fixture = cx
                        .background_executor()
                        .spawn(async move { seed(&root, &source, &target_branch, tip, existing) })
                        .await;
                    let _ = view.update_in(cx, |v, window, cx| {
                        let Seed { preview, api } = fixture;
                        v.ghost_checkout_bench.run.as_mut().unwrap().api = Some(api);
                        let remote = preview.sources[&tip].clone();
                        v.engine.app.branches.update(
                            remote::Update {
                                remote,
                                branches: vec![remote::Branch {
                                    name: branch.clone(),
                                    sha: tip.to_string(),
                                }],
                                scoped: false,
                                cached: true,
                                stale: false,
                            },
                            v.engine.app.refs.as_ref(),
                        );
                        v.engine.app.install_preview(preview);
                        let index = v
                            .engine
                            .app
                            .history
                            .index_of(&tip)
                            .expect("real remote metadata row");
                        assert!(v.engine.app.ghosts.contains_key(&tip));
                        let row = index
                            + usize::from(
                                v.engine.app.pending.exists()
                                    && index >= v.engine.app.pending.row(),
                            );
                        v.action(Action::Select(row, false), window, cx);
                        v.engine.mark("desktop scenario requested");
                        v.engine.mark("ghost checkout requested");
                        v.action(Action::Checkout, window, cx);
                        let choices = &v.engine.app.checkout_confirmation.as_ref().unwrap().choices;
                        assert_eq!(choices.len(), 2);
                        assert!(matches!(
                            choices[0],
                            crate::checkout::Request::DetachedBranch(_)
                        ));
                        assert!(matches!(
                            choices[1],
                            crate::checkout::Request::FetchBranch(_)
                        ));
                        v.ghost_checkout_bench.run.as_mut().unwrap().phase = 2;
                        cx.notify();
                    });
                })
                .detach();
            }
            2 if run.modal.get().is_some_and(|b| {
                b.size.width > px(0.)
                    && b.size.height > px(0.)
                    && b.top() >= px(0.)
                    && b.bottom() <= window.viewport_size().height
            }) =>
            {
                if run.case == "menu" {
                    self.engine.mark("ghost checkout final frame");
                    let run = self.ghost_checkout_bench.run.as_mut().unwrap();
                    run.checks = 2;
                    run.verified = true;
                    return;
                }
                let sha = run.tip.to_string();
                let choice = self
                    .engine
                    .app
                    .checkout_confirmation
                    .as_ref()
                    .unwrap()
                    .choices
                    .iter()
                    .position(|choice| {
                        let target = match choice {
                            crate::checkout::Request::DetachedBranch(t)
                                if run.case == "detached" =>
                            {
                                Some(t)
                            }
                            crate::checkout::Request::FetchBranch(t) if run.case != "detached" => {
                                Some(t)
                            }
                            _ => None,
                        };
                        target.is_some_and(|t| {
                            t.name == run.branch
                                && t.sha == sha
                                && t.remote.as_deref() == Some("origin")
                                && t.url.as_deref() == run.source.to_str()
                        })
                    })
                    .expect("remote branch must offer detached and Pull choices");
                self.engine.mark("ghost checkout modal painted");
                self.engine.mark("ghost checkout confirmed");
                self.action(Action::Confirm(choice), window, cx);
                self.ghost_checkout_bench.run.as_mut().unwrap().phase = 3;
            }
            3 if self.engine.external_tools_idle()
                && self.engine.operation_epoch != run.operation_epoch =>
            {
                if run.api.as_ref().unwrap().requests.load(Ordering::Acquire) == 0 {
                    return;
                }
                assert!(
                    !self.engine.operation_error,
                    "{}",
                    self.engine.operation_status
                );
                assert!(
                    self.engine
                        .app
                        .checkout_notice
                        .as_ref()
                        .is_some_and(|result| result.is_ok()),
                    "{:?}",
                    self.engine.app.checkout_notice
                );
                assert_eq!(self.engine.app.head_id(), Some(run.tip));
                assert_eq!(
                    self.engine
                        .app
                        .refs
                        .as_ref()
                        .unwrap()
                        .head_branch
                        .as_deref(),
                    if run.case == "detached" {
                        None
                    } else {
                        Some(run.branch.as_str())
                    }
                );
                assert!(!self.engine.app.pending.exists());
                assert!(
                    !self.engine.app.ghosts.contains_key(&run.tip),
                    "fetched commit must become local"
                );
                if !run
                    .graph
                    .get()
                    .is_some_and(|paint| paint.head == Some(run.tip) && !paint.ghost)
                {
                    return;
                }
                self.engine.mark("ghost checkout final frame");
                let run = self.ghost_checkout_bench.run.as_mut().unwrap();
                run.checks = 12;
                run.verified = true;
            }
            _ => {}
        }
    }

    pub(super) fn ghost_checkout_modal_probe(&self) -> Option<AnyElement> {
        let slot = self.ghost_checkout_bench.run.as_ref()?.modal.clone();
        Some(
            canvas(
                |bounds, _, _| bounds,
                move |bounds, _, _, _| slot.set(Some(bounds)),
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .into_any_element(),
        )
    }

    pub(super) fn ghost_checkout_graph_probe(&self) -> Option<AnyElement> {
        let run = self.ghost_checkout_bench.run.as_ref()?;
        let (slot, head, ghost) = (
            run.graph.clone(),
            self.engine.app.head_id(),
            self.engine.app.ghosts.contains_key(&run.tip),
        );
        Some(
            canvas(
                |bounds, _, _| bounds,
                move |bounds, _, _, _| {
                    if bounds.size.width > px(0.) && bounds.size.height > px(0.) {
                        slot.set(Some(GraphPaint { head, ghost }));
                    }
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .into_any_element(),
        )
    }

    pub(super) fn ghost_checkout_ready(&self) -> Option<bool> {
        self.ghost_checkout_bench.run.as_ref().map(|r| r.verified)
    }
    pub(super) fn ghost_checkout_assert(&self) {
        if let Some(run) = &self.ghost_checkout_bench.run {
            assert!(run.verified && run.checks > 0);
            if run.case == "menu" {
                return;
            }
            assert_eq!(self.engine.app.head_id(), Some(run.tip));
            assert!(!self.engine.app.ghosts.contains_key(&run.tip));
        }
    }
    pub(super) fn ghost_checkout_summary(&self) -> String {
        self.ghost_checkout_bench.run.as_ref().map(|r| format!("ghost_checkout_verified={} ghost_checkout_case={} ghost_checkout_checks={} ghost_checkout_target={}", r.verified, r.case, r.checks, r.tip)).unwrap_or_default()
    }
}
