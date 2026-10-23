//! Bounded remote diff workers. Superseded API pagination must not monopolize
//! the queue needed by the commit the user has just selected.
use super::*;
use std::{
    collections::VecDeque,
    sync::{Condvar, Mutex},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Id {
    Tree(u32),
    File(u32, u32),
}
impl Id {
    fn of(req: &DiffReq) -> Self {
        match req {
            DiffReq::Tree { key, .. } => Self::Tree(*key),
            DiffReq::File { view, seq, .. } => Self::File(*view, *seq),
        }
    }
    fn live(self, live: Live) -> bool {
        match self {
            Self::Tree(key) => live.keys.contains(&Some(key)),
            Self::File(view, _) => live.views.contains(&Some(view)),
        }
    }
}
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Live {
    keys: [Option<u32>; 3],
    views: [Option<u32>; 2],
}
impl Live {
    fn of(app: &App) -> Self {
        let current = match &app.mode {
            app::Mode::Diff(v) => Some(v),
            _ => None,
        };
        let parked = app.parked_diff.as_deref();
        Self {
            keys: [
                app.details_changes.as_ref().map(|c| c.key),
                current.map(|v| v.changes.key),
                parked.map(|v| v.changes.key),
            ],
            views: [current.map(|v| v.id), parked.map(|v| v.id)],
        }
    }
}
struct Job {
    req: DiffReq,
    cancelled: Arc<AtomicBool>,
}
#[derive(Default)]
struct Queue {
    pending: VecDeque<Job>,
    active: Vec<(Id, Arc<AtomicBool>)>,
    stopped: bool,
}
#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
}
pub(super) struct Worker {
    shared: Arc<Shared>,
    live: Live,
}
impl Worker {
    pub fn new(repo: gix::Repository, tx: Sender<Msg>, no_cache: bool) -> Self {
        let shared = Arc::new(Shared::default());
        // At most two remote requests can be in flight, even during rapid clicks.
        // A slow obsolete comparison leaves a slot for the latest selection.
        for _ in 0..2 {
            let (shared, repo, tx) = (shared.clone(), repo.clone(), tx.clone());
            thread::Builder::new()
                .name("remote-diff".into())
                .spawn(move || {
                    let mut syntax = None;
                    loop {
                        let job = {
                            let mut queue = shared.queue.lock().unwrap();
                            loop {
                                if queue.stopped {
                                    return;
                                }
                                if let Some(job) = queue.pending.pop_front() {
                                    queue.active.push((Id::of(&job.req), job.cancelled.clone()));
                                    break job;
                                }
                                queue = shared.wake.wait(queue).unwrap();
                            }
                        };
                        let id = Id::of(&job.req);
                        let mut highlight = None;
                        let result = match job.req {
                            DiffReq::Tree {
                                key,
                                old,
                                new,
                                remote,
                                old_remote,
                                compare,
                            } => {
                                let remote =
                                    remote.expect("remote worker requires provider provenance");
                                let result = if compare {
                                    remote::files::compare_cancellable(
                                        &repo,
                                        &remote,
                                        old_remote.as_ref(),
                                        old,
                                        new,
                                        no_cache,
                                        Some(&job.cancelled),
                                    )
                                } else {
                                    remote::files::tree_cancellable(
                                        &repo,
                                        &remote,
                                        old,
                                        new,
                                        no_cache,
                                        Some(&job.cancelled),
                                    )
                                };
                                Msg::TreeDiff(key, result)
                            }
                            DiffReq::File {
                                view,
                                seq,
                                file,
                                change,
                            } => {
                                let result =
                                    diff::file_diff(&repo, &change).map_err(|e| e.to_string());
                                if !job.cancelled.load(Ordering::Acquire)
                                    && let Ok(diff) = &result
                                {
                                    highlight = Some((
                                        (view, seq, file),
                                        diff.source.clone(),
                                        [
                                            change.old_path.unwrap_or_else(|| change.path.clone()),
                                            change.path,
                                        ],
                                    ));
                                }
                                Msg::FileDiff(view, seq, file, result)
                            }
                        };
                        if !job.cancelled.load(Ordering::Acquire)
                            && tx.send_blocking(result).is_err()
                        {
                            return;
                        }
                        // Deliver the file before syntax: a fast highlighter may
                        // otherwise produce spans before the view has its rows.
                        if !job.cancelled.load(Ordering::Acquire)
                            && let Some((key, source, paths)) = highlight
                        {
                            syntax
                                .get_or_insert_with(|| diff::syntax::Worker::new(tx.clone()))
                                .submit(key, source, paths);
                        }
                        shared
                            .queue
                            .lock()
                            .unwrap()
                            .active
                            .retain(|(active, token)| {
                                *active != id || !Arc::ptr_eq(token, &job.cancelled)
                            });
                    }
                })
                .expect("spawn remote diff worker");
        }
        Self {
            shared,
            live: Live::default(),
        }
    }
    pub fn update(&mut self, app: &App, requests: Vec<DiffReq>) {
        let live = Live::of(app);
        if self.live == live && requests.is_empty() {
            return;
        }
        self.live = live;
        let mut queue = self.shared.queue.lock().unwrap();
        let stale = |id: Id| {
            !id.live(live)
                || requests
                    .iter()
                    .any(|r| matches!((id,Id::of(r)),(Id::File(a,x),Id::File(b,y)) if a==b && x!=y))
        };
        queue.pending.retain(|job| !stale(Id::of(&job.req)));
        for (id, token) in &queue.active {
            if stale(*id) {
                token.store(true, Ordering::Release);
            }
        }
        for req in requests {
            if Id::of(&req).live(live) {
                queue.pending.push_back(Job {
                    req,
                    cancelled: Arc::new(AtomicBool::new(false)),
                });
            }
        }
        self.shared.wake.notify_all();
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let mut queue = self.shared.queue.lock().unwrap();
        queue.stopped = true;
        queue.pending.clear();
        for (_, token) in &queue.active {
            token.store(true, Ordering::Release);
        }
        self.shared.wake.notify_all();
    }
}
