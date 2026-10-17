//! Event-driven orchestration shared by the GPUI view and integration harness.
use crate::diffview::DiffReq;
use crate::model::History;
use crate::{
    app::{self, App, LoadState},
    branches, cache, diff, git, pending, remote, search, settings, ui,
};
use async_channel::{Receiver, Sender};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};
pub(crate) enum Msg {
    Search(search::Matches),
    Pending(u32, Result<pending::Snapshot, String>),
    Committed(Result<String, String>),
    Remote(u32, remote::Event),
    Settings(u32, settings::Outcome),
    Preview(u32, remote::history::Preview),
    PreviewTags(u32, Vec<(gix::ObjectId, String)>, Vec<String>),
    BranchActivity(
        u32,
        Vec<(branches::Target, branches::Activity)>,
        Option<(branches::Target, String)>,
    ),
    Refreshed(Box<git::Result<(gix::Repository, git::RefSnapshot)>>),
    Checkout(Result<String, String>, bool),
    Details(gix::ObjectId, Result<git::CommitDetails, String>),
    Cache(Option<cache::Cached>),
    Repo(Box<git::Result<(gix::Repository, git::RefSnapshot)>>),
    Batch(u32, History),
    WalkDone(u32, git::Result<()>),
    CacheWritten(bool),
    /// Tree diff for the change set with this key.
    TreeDiff(u32, Result<diff::TreeDiff, String>),
    /// Per-file line counts for the change set with this key.
    Stats(u32, Vec<(usize, diff::Stat)>),
    /// File diff for diff view `id`: (view, seq, file index, result).
    FileDiff(u32, u32, usize, Result<diff::FileDiff, String>),
}

/// Diff requests run on one worker thread. A burst of requests (holding an
/// arrow key) is collapsed to the newest tree request and the newest file
/// request; older ones would only produce results nobody is waiting for.
fn spawn_diff_worker(
    repo: gix::Repository,
    tx: Sender<Msg>,
    no_cache: bool,
) -> std::sync::mpsc::Sender<DiffReq> {
    let (req_tx, req_rx) = std::sync::mpsc::channel::<DiffReq>();
    thread::Builder::new()
        .name("diff".into())
        .spawn(move || {
            while let Ok(first) = req_rx.recv() {
                let mut tree: Option<DiffReq> = None;
                let mut file: Option<DiffReq> = None;
                let mut next = Some(first);
                while let Some(req) = next.take() {
                    match req {
                        DiffReq::Tree { .. } => tree = Some(req),
                        DiffReq::File { .. } => file = Some(req),
                    }
                    next = req_rx.try_recv().ok();
                }
                if let Some(DiffReq::Tree {
                    key,
                    old,
                    new,
                    remote,
                    old_remote,
                    compare,
                }) = tree
                {
                    let result = if let Some(remote) = remote {
                        if compare {
                            remote::files::compare(
                                &repo,
                                &remote,
                                old_remote.as_ref(),
                                old,
                                new,
                                no_cache,
                            )
                        } else {
                            remote::files::tree(&repo, &remote, old, new, no_cache)
                        }
                    } else {
                        diff::tree_diff(&repo, old.as_ref(), &new).map_err(|e| e.to_string())
                    };
                    if tx.send_blocking(Msg::TreeDiff(key, result)).is_err() {
                        return;
                    }
                }
                if let Some(DiffReq::File {
                    view,
                    seq,
                    file,
                    change,
                }) = file
                {
                    let result = diff::file_diff(&repo, &change).map_err(|e| e.to_string());
                    if tx
                        .send_blocking(Msg::FileDiff(view, seq, file, result))
                        .is_err()
                    {
                        return;
                    }
                }
            }
        })
        .expect("spawn diff worker");
    req_tx
}

/// Line counts for a change set's files.
struct StatsReq {
    key: u32,
    files: Arc<Vec<diff::FileChange>>,
    /// Indices still lacking stats.
    todo: Vec<usize>,
}

/// Stats run on their own thread so a 5,000-file commit never delays the file
/// diff the user is looking at. Results stream in chunks; the job stops as
/// soon as `latest` names a different change set.
fn spawn_stats_worker(
    repo: gix::Repository,
    tx: Sender<Msg>,
    latest: Arc<AtomicU32>,
) -> std::sync::mpsc::Sender<StatsReq> {
    let (req_tx, req_rx) = std::sync::mpsc::channel::<StatsReq>();
    thread::Builder::new()
        .name("diff-stats".into())
        .spawn(move || {
            'jobs: while let Ok(mut req) = req_rx.recv() {
                while let Ok(newer) = req_rx.try_recv() {
                    req = newer;
                }
                let mut chunk = Vec::new();
                let mut last_send = Instant::now();
                for &i in &req.todo {
                    if latest.load(Ordering::Relaxed) != req.key {
                        continue 'jobs;
                    }
                    let stat = diff::file_stats(&repo, &req.files[i]).unwrap_or(diff::Stat::Opaque);
                    chunk.push((i, stat));
                    if chunk.len() >= 64 || last_send.elapsed() > Duration::from_millis(30) {
                        if tx
                            .send_blocking(Msg::Stats(req.key, std::mem::take(&mut chunk)))
                            .is_err()
                        {
                            return;
                        }
                        last_send = Instant::now();
                    }
                }
                if !chunk.is_empty() && tx.send_blocking(Msg::Stats(req.key, chunk)).is_err() {
                    return;
                }
            }
        })
        .expect("spawn stats worker");
    req_tx
}

/// Startup milestone timings, printed to stderr at exit when `GIT_RUSTLE_TIMING` is set.
struct Timing {
    start: Instant,
    enabled: bool,
    marks: Vec<(String, Duration)>,
}

impl Timing {
    fn new(start: Instant) -> Self {
        Timing {
            start,
            enabled: std::env::var_os("GIT_RUSTLE_TIMING").is_some(),
            marks: Vec::new(),
        }
    }
    fn mark(&mut self, what: impl Into<String>) {
        if self.enabled {
            let label = what.into();
            let elapsed = self.start.elapsed();
            if std::env::var_os("GIT_RUSTLE_TRACE").is_some() {
                eprintln!("{:>9.3} ms  {}", elapsed.as_secs_f64() * 1000., label);
            }
            self.marks.push((label, elapsed));
        }
    }
    fn report(&self) {
        if !self.enabled {
            return;
        }
        for (what, at) in &self.marks {
            eprintln!("{:>9.3} ms  {}", at.as_secs_f64() * 1000.0, what);
        }
    }
}

/// Cheap upward search for the git directory, used to locate the cache file
/// before the repository itself is opened.
fn find_git_dir(start: &Path) -> Option<PathBuf> {
    let start = std::fs::canonicalize(start).ok()?;
    for dir in start.ancestors() {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() {
            return std::fs::canonicalize(&dot_git).ok();
        }
        if dot_git.is_file() {
            // Linked worktree or submodule: "gitdir: <path>"
            let content = std::fs::read_to_string(&dot_git).ok()?;
            let rel = content.trim().strip_prefix("gitdir:")?.trim();
            let target = dir.join(rel);
            return std::fs::canonicalize(target).ok();
        }
        if dir.join("HEAD").is_file() && dir.join("objects").is_dir() {
            return Some(dir.to_path_buf());
        }
    }
    None
}

struct Walker {
    id: u32,
    cancel: Arc<AtomicBool>,
    /// Joined at exit so a cache write in progress is not cut short.
    handle: Option<thread::JoinHandle<()>>,
}

impl Walker {
    fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    fn join(mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn spawn_walk(
    id: u32,
    repo: gix::Repository,
    tips: Vec<gix::ObjectId>,
    cache_target: Option<(PathBuf, PathBuf, u64)>,
    tx: Sender<Msg>,
) -> Walker {
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel2 = Arc::clone(&cancel);
    let handle = thread::Builder::new()
        .name(format!("walk-{id}"))
        .spawn(move || {
            let mut full = cache_target.as_ref().map(|_| History::default());
            let result = git::walk(&repo, &tips, &mut |chunk: History| {
                if cancel2.load(Ordering::Relaxed) {
                    return false;
                }
                if let Some(full) = full.as_mut() {
                    full.append(chunk.clone());
                }
                tx.send_blocking(Msg::Batch(id, chunk)).is_ok()
            });
            let cancelled = cancel2.load(Ordering::Relaxed);
            let ok = result.is_ok() && !cancelled;
            let _ = tx.send_blocking(Msg::WalkDone(id, result));
            if ok && let (Some(full), Some((path, git_dir, fp))) = (full, cache_target) {
                let written = cache::write(&path, &git_dir, fp, &full).is_ok();
                let _ = tx.send_blocking(Msg::CacheWritten(written));
            }
        })
        .expect("spawn walker");
    Walker {
        id,
        cancel,
        handle: Some(handle),
    }
}

fn refresh_repo(path: PathBuf, tx: Sender<Msg>) {
    thread::spawn(move || {
        let result = git::open(&path).and_then(|repo| {
            let snap = git::snapshot_refs(&repo)?;
            Ok((repo, snap))
        });
        let _ = tx.send_blocking(Msg::Refreshed(Box::new(result)));
    });
}

pub struct Engine {
    pub app: App,
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    timing: Timing,
    cache_path: Option<PathBuf>,
    no_cache: bool,
    details_requested: Option<gix::ObjectId>,
    details_tx: Option<std::sync::mpsc::Sender<gix::ObjectId>>,
    repo: Option<gix::Repository>,
    pending_generation: u32,
    pending_checked: Instant,
    diff_tx: Option<std::sync::mpsc::Sender<DiffReq>>,
    search_tx: Option<std::sync::mpsc::Sender<search::Request>>,
    file_search_tx: Option<std::sync::mpsc::Sender<search::Request>>,
    search_latest: Arc<std::sync::atomic::AtomicU64>,
    remote_diff_tx: Option<std::sync::mpsc::Sender<DiffReq>>,
    stats_tx: Option<std::sync::mpsc::Sender<StatsReq>>,
    stats_latest: Arc<AtomicU32>,
    stats_sent_for: Option<u32>,
    cache_fp: Option<u64>,
    cache_seen: bool,
    repo_seen: bool,
    refreshing: bool,
    walker: Option<Walker>,
    finishing: Vec<Walker>,
    next_walk_id: u32,
    first_frame: bool,
    remote_generation: u32,
    preview_epoch: u32,
    preview_cancel: Arc<AtomicBool>,
    preview_busy: bool,
    preview_wanted: bool,
    preview_force: bool,
    activity_busy: bool,
    local_activity_done: Option<u64>,
    auto_remote: bool,
    first_frame_at: Option<Instant>,
}
impl Engine {
    pub fn repository_path(&self) -> String {
        self.repo
            .as_ref()
            .map(|r| r.workdir().unwrap_or(r.path()).display().to_string())
            .unwrap_or_default()
    }
    pub fn new(path: PathBuf, no_cache: bool, start: Instant) -> Self {
        let (tx, rx) = async_channel::unbounded();
        let cache_path = if no_cache {
            None
        } else {
            find_git_dir(&path).as_deref().and_then(cache::cache_path)
        };
        let cache = cache_path.clone();
        let cache_tx = tx.clone();
        thread::spawn(move || {
            let _ = cache_tx.send_blocking(Msg::Cache(cache.and_then(|p| cache::read(&p).ok())));
        });
        let repo_tx = tx.clone();
        thread::spawn(move || {
            let result =
                git::open(&path).and_then(|repo| Ok((repo.clone(), git::snapshot_refs(&repo)?)));
            let _ = repo_tx.send_blocking(Msg::Repo(Box::new(result)));
        });
        let mut app = App::new();
        app.pending.known = false;
        app.pending.autofocus = true;
        app.pending.want_refresh = true;
        Self {
            app,
            tx,
            rx,
            timing: Timing::new(start),
            cache_path,
            no_cache,
            details_requested: None,
            details_tx: None,
            repo: None,
            pending_generation: 0,
            pending_checked: Instant::now(),
            diff_tx: None,
            search_tx: None,
            file_search_tx: None,
            search_latest: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            remote_diff_tx: None,
            stats_tx: None,
            stats_latest: Arc::new(AtomicU32::new(0)),
            stats_sent_for: None,
            cache_fp: None,
            cache_seen: false,
            repo_seen: false,
            refreshing: false,
            walker: None,
            finishing: Vec::new(),
            next_walk_id: 1,
            first_frame: false,
            remote_generation: 0,
            preview_epoch: 0,
            preview_cancel: Arc::new(AtomicBool::new(false)),
            preview_busy: false,
            preview_wanted: false,
            preview_force: false,
            activity_busy: false,
            local_activity_done: None,
            auto_remote: false,
            first_frame_at: None,
        }
    }
    pub fn messages(&self) -> Receiver<Msg> {
        self.rx.clone()
    }
    pub fn tick(&mut self, message: Option<Msg>) {
        self.advance(message);
        self.advance(None);
    }
    fn advance(&mut self, message: Option<Msg>) {
        let app = &mut self.app;
        let tx = &self.tx;
        let timing = &mut self.timing;
        let cache_path = &self.cache_path;
        let no_cache = self.no_cache;
        // Start network work only after the local graph has rendered and finished loading.
        if self.first_frame && !app.is_walking() && self.repo.is_some() && !self.auto_remote {
            self.auto_remote = true;
            app.branches.want_refresh.get_or_insert(false);
        }
        if let Some(r) = self.repo.as_ref() {
            if !app.pending.busy
                && !app.branches.busy
                && let Some(request) = app.pending.request.take()
            {
                app.pending.busy = true;
                app.pending.loading = false;
                self.pending_generation = self.pending_generation.wrapping_add(1);
                app.pending.status = "Committing changes…".into();
                app.pending.error = false;
                app.needs_redraw = true;
                let path = r.workdir().unwrap_or_else(|| r.git_dir()).to_path_buf();
                let tx = tx.clone();
                timing.mark("commit requested");
                thread::spawn(move || {
                    let _ =
                        tx.send_blocking(Msg::Committed(git::worktree::commit(&path, &request)));
                });
            }
            if self.first_frame
                && !app.pending.busy
                && !app.branches.busy
                && !app.pending.loading
                && matches!(app.mode, app::Mode::Graph)
                && (app.pending.want_refresh
                    || self.pending_checked.elapsed() >= Duration::from_secs(2))
            {
                app.pending.want_refresh = false;
                app.pending.loading = true;
                self.pending_checked = Instant::now();
                self.pending_generation = self.pending_generation.wrapping_add(1);
                let generation = self.pending_generation;
                let r = r.clone();
                let tx = tx.clone();
                timing.mark("pending status requested");
                thread::spawn(move || {
                    let _ = tx.send_blocking(Msg::Pending(generation, git::worktree::snapshot(&r)));
                });
            }
        }
        if std::mem::take(&mut app.branches.preview_requested) {
            self.preview_epoch = self.preview_epoch.wrapping_add(1);
            self.preview_cancel.store(true, Ordering::Relaxed);
            self.preview_wanted = true;
        }
        if self.preview_wanted
            && !self.preview_busy
            && !app.is_walking()
            && !app.want_rewalk
            && !app.settings.saving
            && !app.branches.busy
            && let Some(r) = self.repo.as_ref()
            && let Some(refs) = app.refs.clone()
        {
            self.preview_wanted = false;
            {
                self.preview_busy = true;
                let local = app
                    .local_history
                    .get_or_insert_with(|| Arc::new(app.history.clone()))
                    .clone();
                let updates = app.branches.remotes.values().cloned().collect::<Vec<_>>();
                let requested = app.branches.preview_targets.clone();
                let r = r.clone();
                let tx = tx.clone();
                let epoch = self.preview_epoch;
                let force = std::mem::take(&mut self.preview_force);
                self.preview_cancel = Arc::new(AtomicBool::new(false));
                let cancel = self.preview_cancel.clone();
                app.ghost_status = "Checking remote commits and tags…".into();
                app.needs_redraw = true;
                timing.mark("ghost preview requested");
                thread::spawn(move || {
                    let preview = remote::history::load_stream(
                        &r,
                        &local,
                        &refs,
                        &updates,
                        &requested,
                        force,
                        no_cache,
                        &cancel,
                        Some(&mut |preview| {
                            let _ = tx.send_blocking(Msg::Preview(epoch, preview));
                        }),
                    );
                    let _ = tx.send_blocking(Msg::PreviewTags(epoch, preview.tags, preview.errors));
                });
            }
        }
        if !app.branches.busy
            && let Some(r) = self.repo.as_ref()
            && let Some(request) = app.settings.request.take()
        {
            let path = r.workdir().unwrap_or_else(|| r.git_dir()).to_path_buf();
            let session = app.settings.session;
            let tx = tx.clone();
            timing.mark("settings requested");
            thread::spawn(move || {
                let _ = tx.send_blocking(Msg::Settings(session, settings::execute(&path, request)));
            });
        }
        if !app.settings.saving
            && (app.want_refresh
                || app.branches.want_refresh.is_some()
                || app.checkout_request.is_some())
            && let Some(r) = self.repo.as_ref()
        {
            let path = r.workdir().unwrap_or_else(|| r.git_dir()).to_path_buf();
            if app.want_refresh && !self.refreshing && !app.branches.busy {
                app.want_refresh = false;
                self.refreshing = true;
                refresh_repo(path.clone(), tx.clone());
            }
            if !app.branches.loading
                && !app.branches.busy
                && let Some(force) = app.branches.want_refresh.take()
            {
                self.preview_epoch = self.preview_epoch.wrapping_add(1);
                self.preview_cancel.store(true, Ordering::Relaxed);
                self.preview_wanted = app.branches.pending_jump.is_some();
                self.remote_generation = self.remote_generation.wrapping_add(1);
                let generation = self.remote_generation;
                self.preview_force |= force;
                if force {
                    self.local_activity_done = None;
                    app.branches.activity_cache.clear();
                    app.branches.activity_attempted.clear();
                    app.branches.activity_errors.clear();
                    app.branches.activity_requests = 0;
                }
                app.branches.loading = true;
                app.branches.errors.clear();
                app.needs_redraw = true;
                let git_dir = git::git_dir(r);
                let tx = tx.clone();
                let path = path.clone();
                let local_only = !app.branches.open;
                timing.mark("remote discovery requested");
                thread::spawn(move || {
                    remote::discover(&path, &git_dir, force, no_cache, local_only, |event| {
                        let _ = tx.send_blocking(Msg::Remote(generation, event));
                    })
                });
                if force {
                    app.want_refresh = true;
                }
            }
            if !app.branches.busy
                && !app.pending.busy
                && let Some(target) = app.checkout_request.take()
            {
                self.pending_generation = self.pending_generation.wrapping_add(1);
                app.pending.loading = false;
                app.branches.busy = true;
                app.branches.status = format!("Checking out {}…", target.label());
                app.checkout_notice = Some(Ok(app.branches.status.clone()));
                app.needs_redraw = true;
                let tx = tx.clone();
                timing.mark("checkout requested");
                thread::spawn(move || {
                    let result = git::checkout::execute(&path, &target);
                    let _ = tx.send_blocking(Msg::Checkout(result, target.is_remote()));
                });
            }
        }
        // Browser metadata is lazy; neither the initial frame nor redraws walk history.
        if app.branches.open
            && !self.activity_busy
            && !app.is_walking()
            && !app.want_rewalk
            && let Some(r) = self.repo.as_ref()
            && let Some(refs) = app.refs.as_ref()
        {
            let generation = self.remote_generation;
            if self.local_activity_done != Some(refs.fingerprint) {
                self.local_activity_done = Some(refs.fingerprint);
                self.activity_busy = true;
                let tips = app
                    .branches
                    .entries
                    .iter()
                    .map(|e| e.target.clone())
                    .collect::<Vec<_>>();
                let r = r.clone();
                let tx = tx.clone();
                timing.mark("branch activity requested");
                thread::spawn(move || {
                    let values = branches::local_activity(&r, &tips, ui::time::now_unix());
                    let _ = tx.send_blocking(Msg::BranchActivity(generation, values, None));
                });
            } else if let Some((target, remote)) = app.branches.next_remote_activity() {
                self.activity_busy = true;
                let r = r.clone();
                let tx = tx.clone();
                thread::spawn(move || {
                    let (values, error) = match remote::history::branch_activity(
                        &r,
                        &remote,
                        &target.sha,
                        ui::time::now_unix(),
                        no_cache,
                    ) {
                        Ok(value) => (vec![(target, value)], None),
                        Err(error) => (Vec::new(), Some((target, error))),
                    };
                    let _ = tx.send_blocking(Msg::BranchActivity(generation, values, error));
                });
            }
        }
        // Object reads stay off the event thread, including on cache hits.
        if let (Some(r), Some(id)) = (self.repo.as_ref(), app.selected_id())
            && app.details.as_ref().map(|d| d.id) != Some(id)
            && self.details_requested != Some(id)
        {
            self.details_requested = Some(id);
            if let Some(details) = app.ghosts.get(&id) {
                self.details_requested = None;
                app.details = Some(details.clone());
                app.needs_redraw = true;
            } else {
                let worker = self.details_tx.get_or_insert_with(|| {
                    let r = r.clone();
                    let tx = tx.clone();
                    let (sender, receiver) = mpsc::channel();
                    thread::spawn(move || {
                        let mut cache =
                            std::collections::VecDeque::<(gix::ObjectId, git::CommitDetails)>::new(
                            );
                        while let Ok(mut id) = receiver.recv() {
                            while let Ok(newer) = receiver.try_recv() {
                                id = newer;
                            }
                            let result =
                                if let Some(i) = cache.iter().position(|(key, _)| *key == id) {
                                    let entry = cache.remove(i).unwrap();
                                    let result = Ok(entry.1.clone());
                                    cache.push_back(entry);
                                    result
                                } else {
                                    let result =
                                        git::commit_details(&r, &id).map_err(|e| e.to_string());
                                    if let Ok(details) = &result {
                                        if cache.len() == 64 {
                                            cache.pop_front();
                                        }
                                        cache.push_back((id, details.clone()));
                                    }
                                    result
                                };
                            if tx.send_blocking(Msg::Details(id, result)).is_err() {
                                break;
                            }
                        }
                    });
                    sender
                });
                let _ = worker.send(id);
            }
        }
        // Hand new diff work to the workers.
        if let Some(r) = self.repo.as_ref() {
            self.search_latest
                .store(app.search.generation, Ordering::Relaxed);
            if app.search.wanted {
                timing.mark(format!("search requested ({})", app.search.generation));
            }
            if let Some(request) = app.search_request() {
                // A slow ghost file-list request cannot hold up metadata searches.
                let slot = if search::Query::parse(&request.text).field == search::Field::File {
                    &mut self.file_search_tx
                } else {
                    &mut self.search_tx
                };
                let worker = slot.get_or_insert_with(|| {
                    let (stx, srx) = mpsc::channel::<search::Request>();
                    let search_repo = r.clone();
                    let tx = tx.clone();
                    let latest = self.search_latest.clone();
                    thread::Builder::new()
                        .name("graph-search".into())
                        .spawn(move || {
                            let mut index = search::Index::default();
                            while let Ok(mut request) = srx.recv() {
                                while let Ok(newer) = srx.try_recv() {
                                    request = newer;
                                }
                                index.run(&search_repo, request, &latest, no_cache, |result| {
                                    let _ = tx.send_blocking(Msg::Search(result));
                                });
                            }
                        })
                        .expect("spawn graph search");
                    stx
                });
                let _ = worker.send(request);
            }
            app.sync_details_changes();
            let reqs = app.pending_requests();
            if !reqs.is_empty() {
                for mut req in reqs {
                    if let DiffReq::Tree {
                        old,
                        new,
                        remote,
                        old_remote,
                        compare,
                        ..
                    } = &mut req
                    {
                        *remote = app
                            .ghost_sources
                            .get(new)
                            .or_else(|| old.as_ref().and_then(|id| app.ghost_sources.get(id)))
                            .cloned();
                        *old_remote = old
                            .as_ref()
                            .and_then(|id| app.ghost_sources.get(id))
                            .cloned();
                        *compare = remote.is_some()
                            && (!app.ghost_sources.contains_key(new) || *old != app.parent_of(new));
                    }
                    if matches!(req, DiffReq::Tree { .. }) {
                        timing.mark("diff requested");
                    } else {
                        timing.mark("file diff requested");
                    }
                    let remote_request = match &req {
                        DiffReq::Tree { remote, .. } => remote.is_some(),
                        DiffReq::File { change, .. } => change.remote.is_some(),
                    };
                    let worker = if remote_request {
                        &mut self.remote_diff_tx
                    } else {
                        &mut self.diff_tx
                    };
                    let dtx = worker
                        .get_or_insert_with(|| spawn_diff_worker(r.clone(), tx.clone(), no_cache));
                    let _ = dtx.send(req);
                }
            }
            // Line counts for whichever change set is on screen.
            let want = app
                .active_changes_mut()
                .filter(|c| c.wants_stats())
                .map(|c| StatsReq {
                    key: c.key,
                    files: Arc::clone(&c.files),
                    todo: (0..c.stats.len())
                        .filter(|&i| c.stats[i].is_none())
                        .collect(),
                });
            if let Some(req) = want
                && self.stats_sent_for != Some(req.key)
            {
                self.stats_sent_for = Some(req.key);
                self.stats_latest.store(req.key, Ordering::Relaxed);
                let stx = self.stats_tx.get_or_insert_with(|| {
                    spawn_stats_worker(r.clone(), tx.clone(), Arc::clone(&self.stats_latest))
                });
                let _ = stx.send(req);
            }
        }

        // Start a walk when asked (or when the cache turned out stale).
        if app.want_rewalk
            && let Some(r) = self.repo.as_ref()
            && let Some(refs) = app.refs.as_ref()
        {
            app.want_rewalk = false;
            if let Some(wk) = self.walker.take() {
                wk.cancel();
                self.finishing.push(wk);
            }
            let id = self.next_walk_id;
            self.next_walk_id += 1;
            let target = if no_cache {
                None
            } else {
                cache_path
                    .clone()
                    .map(|p| (p, git::git_dir(r), refs.fingerprint))
            };
            self.walker = Some(spawn_walk(
                id,
                r.clone(),
                refs.tips.clone(),
                target,
                tx.clone(),
            ));
            app.start_walk();
            timing.mark("walk started");
        }

        let mut pending = Vec::new();
        if let Some(msg) = message {
            pending.push(msg);
        }
        while let Ok(m) = self.rx.try_recv() {
            pending.push(m);
            if pending.len() >= 256 {
                break;
            }
        }
        for msg in pending {
            match msg {
                Msg::Search(result) => {
                    let done = result.done;
                    let generation = result.generation;
                    let count = result.rows.len();
                    if app.install_search(result) && done {
                        timing.mark(format!("search ready ({generation}; {count} matches)"));
                    }
                }
                Msg::Pending(generation, result) => {
                    if generation == self.pending_generation {
                        if let Ok(snapshot) = &result {
                            timing.mark(format!(
                                "pending status ready ({} files)",
                                snapshot.files.len()
                            ));
                            if snapshot.head != app.head_id()
                                || app
                                    .refs
                                    .as_ref()
                                    .is_some_and(|r| r.head_branch != snapshot.branch)
                            {
                                app.want_refresh = true;
                            }
                        } else {
                            timing.mark("pending status failed");
                        }
                        app.install_pending(result);
                    }
                }
                Msg::Committed(result) => {
                    timing.mark("commit done");
                    app.pending.busy = false;
                    app.pending.error = result.is_err();
                    app.pending.status = match &result {
                        Ok(message) => message.clone(),
                        Err(error) => format!("Commit failed: {error}"),
                    };
                    if result.is_ok() {
                        app.pending.description = Default::default();
                        app.pending.branch = Default::default();
                    }
                    app.pending.want_refresh = true;
                    app.want_refresh = true;
                    app.needs_redraw = true;
                }
                Msg::BranchActivity(generation, values, error) => {
                    self.activity_busy = false;
                    if generation == self.remote_generation {
                        timing.mark(format!("branch activity ready ({} tips)", values.len()));
                        app.branches.install_activity(values, app.refs.as_ref());
                        if let Some((target, error)) = error {
                            app.branches.activity_errors.insert(target, error);
                        }
                        app.needs_redraw = true;
                    } else {
                        self.local_activity_done = None;
                    }
                }
                Msg::Preview(epoch, mut preview) => {
                    if epoch == self.preview_epoch
                        && !app.branches.preview_requested
                        && !app.is_walking()
                        && !app.want_rewalk
                    {
                        timing.mark(format!(
                            "ghost preview ready ({} commits)",
                            preview.ghosts.len()
                        ));
                        preview.errors.extend(app.branches.errors.iter().cloned());
                        app.install_preview(preview);
                        app.finish_branch_jump();
                    }
                }
                Msg::PreviewTags(epoch, tags, errors) => {
                    self.preview_busy = false;
                    if epoch == self.preview_epoch
                        && !app.branches.preview_requested
                        && !app.is_walking()
                        && !app.want_rewalk
                    {
                        app.ghost_tags = tags;
                        if !errors.is_empty() {
                            app.ghost_status =
                                format!("Remote preview incomplete: {}", errors.join(" | "));
                        }
                        if let Some(refs) = app.refs.clone() {
                            app.set_refs(refs);
                        }
                    }
                }
                Msg::Settings(session, outcome) => {
                    if matches!(&outcome, settings::Outcome::Saved(Ok(_))) {
                        self.preview_epoch = self.preview_epoch.wrapping_add(1);
                        self.preview_cancel.store(true, Ordering::Relaxed);
                        self.preview_wanted = false;
                        app.install_preview(remote::history::Preview {
                            history: None,
                            ghosts: Default::default(),
                            sources: Default::default(),
                            branches: Vec::new(),
                            tags: Vec::new(),
                            errors: Vec::new(),
                        });
                        remote::clear_auth_cache();
                        app.local_history = None;
                        self.remote_generation = self.remote_generation.wrapping_add(1);
                        app.branches.loading = false;
                        app.branches.remotes.clear();
                        app.branches.preview_targets.clear();
                        app.branches.pending_jump = None;
                        app.branches.activity_cache.clear();
                        app.branches.activity_attempted.clear();
                        app.branches.activity_errors.clear();
                        app.branches.activity_requests = 0;
                        self.local_activity_done = None;
                        app.branches.last_check = None;
                        app.branches.want_refresh = Some(true);
                        if let Some(refs) = app.refs.clone() {
                            app.set_refs(refs);
                        }
                    }
                    if app.settings.open && session == app.settings.session {
                        app.settings.finish(outcome);
                    }
                    app.needs_redraw = true;
                    timing.mark("settings completed");
                }
                Msg::Remote(generation, event) => {
                    if generation != self.remote_generation {
                        continue;
                    }
                    match event {
                        remote::Event::Configured(remotes) => {
                            if remotes.is_empty() && app.local_history.is_some() {
                                self.preview_wanted = true;
                            }
                            app.branches.remotes.retain(|name, update| {
                                remotes
                                    .iter()
                                    .any(|(n, url)| n == name && url == &update.remote.url)
                            });
                            if let Some(refs) = app.refs.clone() {
                                app.set_refs(refs);
                            }
                        }
                        remote::Event::Branches(update) => {
                            self.preview_wanted |= self.preview_force
                                || app.branches.remotes.get(&update.remote.name).is_none_or(
                                    |old| {
                                        old.remote != update.remote
                                            || old.branches != update.branches
                                    },
                                );
                            timing.mark(format!(
                                "remote {} branches ({})",
                                if update.cached { "cached" } else { "live" },
                                update.branches.len()
                            ));
                            app.branches.update(update, app.refs.as_ref());
                            app.remote_labels();
                        }
                        remote::Event::Status(error) => {
                            app.ghost_status = format!("Remote: {error}");
                            app.branches.errors.push(error);
                        }
                        remote::Event::Done => {
                            app.branches.loading = false;
                            app.branches.last_check = Some(Instant::now());
                            // If b was opened during the startup head check, finish with a full listing.
                            if app.branches.open
                                && app.branches.errors.is_empty()
                                && app.branches.remotes.values().any(|u| u.scoped)
                            {
                                app.branches.want_refresh = Some(false);
                            }
                            timing.mark("remote discovery done");
                        }
                    }
                    app.needs_redraw = true;
                }
                Msg::Checkout(result, remote) => {
                    app.pending.want_refresh = true;
                    app.branches.busy = false;
                    if result.is_ok() && remote {
                        app.branches.want_refresh = Some(true);
                    }
                    app.checkout_notice = Some(result.clone());
                    app.branches.status = match result {
                        Ok(message) => message,
                        Err(error) => format!("Checkout failed: {error}"),
                    };
                    app.want_refresh = true;
                    app.needs_redraw = true;
                    timing.mark("checkout done");
                }
                Msg::Refreshed(result) => {
                    self.refreshing = false;
                    match *result {
                        Ok((r, snap)) => {
                            let changed = app
                                .refs
                                .as_ref()
                                .is_none_or(|old| old.fingerprint != snap.fingerprint);
                            app.set_refs(snap);
                            self.repo = Some(r);
                            if changed {
                                self.preview_epoch = self.preview_epoch.wrapping_add(1);
                                self.preview_cancel.store(true, Ordering::Relaxed);
                                self.preview_wanted = true;
                                app.want_rewalk = true;
                            }
                        }
                        Err(error) => app.set_error(error.to_string()),
                    }
                }
                Msg::Details(id, result) => {
                    // A completed read must not suppress a later read of the same
                    // commit after Pending changes has cleared the details view.
                    if self.details_requested == Some(id)
                        && (result.is_ok() || app.selected_id() != Some(id))
                    {
                        self.details_requested = None;
                    }
                    if app.selected_id() == Some(id) {
                        match result {
                            Ok(details) => app.details = Some(details),
                            Err(error) => app.checkout_notice = Some(Err(error)),
                        }
                        app.needs_redraw = true;
                    }
                }
                Msg::Cache(cached) => {
                    self.cache_seen = true;
                    if let Some(c) = cached {
                        timing.mark(format!("cache read ({} rows)", c.history.len()));
                        self.cache_fp = Some(c.fingerprint);
                        let complete = c.history.complete;
                        app.set_cached(c.history, true);
                        if !complete {
                            app.want_rewalk = true;
                        }
                    } else {
                        timing.mark("no cache");
                    }
                    if self.repo_seen {
                        decide(app, self.cache_fp);
                    }
                }
                Msg::Repo(result) => {
                    self.repo_seen = true;
                    match *result {
                        Ok((r, snap)) => {
                            timing.mark(format!("repo open ({} refs)", snap.refs.len()));
                            app.repo_name = git::display_name(&r);
                            app.set_refs(snap);
                            self.repo = Some(r);
                            if self.cache_seen {
                                decide(app, self.cache_fp);
                            }
                        }
                        Err(e) => {
                            if app.history.is_empty() {
                                app.set_error(e.to_string());
                            }
                            app.set_error(e.to_string());
                        }
                    }
                }
                Msg::Batch(id, batch) => {
                    if self.walker.as_ref().is_some_and(|w| w.id == id) {
                        if batch.complete {
                            let so_far = match app.incoming.as_ref() {
                                Some(incoming) => incoming.len(),
                                None if matches!(app.load, LoadState::Loading) => app.history.len(),
                                None => 0,
                            };
                            timing.mark(format!("walk done ({} rows)", so_far + batch.len()));
                        }
                        app.push_batch(batch);
                    }
                }
                Msg::WalkDone(id, result) => {
                    if self.walker.as_ref().is_some_and(|w| w.id == id) {
                        self.finishing.extend(self.walker.take());
                        self.first_frame_at.get_or_insert_with(Instant::now);
                        if let Err(e) = result {
                            app.set_error(e.to_string());
                        } else if app.is_walking() {
                            app.load = LoadState::Complete;
                            app.needs_redraw = true;
                        }
                    }
                }
                Msg::CacheWritten(ok) => {
                    timing.mark(if ok {
                        "cache written"
                    } else {
                        "cache write failed"
                    });
                    // Reap finished threads.
                    self.finishing
                        .retain(|w| w.handle.as_ref().is_some_and(|h| !h.is_finished()));
                }
                Msg::TreeDiff(key, result) => {
                    if let Ok(td) = &result {
                        timing.mark(format!("tree diff ({} files)", td.files.len()));
                    }
                    if let Some(view) = app.diff_view_mut()
                        && view.changes.key == key
                    {
                        view.set_tree(result);
                        app.needs_redraw = true;
                    } else if let Some(cs) = app.changes_by_key_mut(key) {
                        cs.set_tree(result);
                        app.needs_redraw = true;
                    }
                }
                Msg::Stats(key, chunk) => {
                    if let Some(cs) = app.changes_by_key_mut(key) {
                        cs.apply_stats(&chunk);
                        if !cs.wants_stats() {
                            timing.mark("stats done");
                        }
                        app.needs_redraw = true;
                    }
                }
                Msg::FileDiff(view_id, seq, file, result) => {
                    if let Some(view) = app.diff_view_mut()
                        && view.id == view_id
                    {
                        if let Ok(d) = &result {
                            timing.mark(format!("file diff ({} rows)", d.rows.len()));
                        }
                        view.set_file_diff(seq, file, result);
                        app.needs_redraw = true;
                    }
                }
            }
            if app.quit {
                break;
            }
        }
    }
    pub fn has_presented(&self) -> bool {
        self.first_frame
    }
    pub fn presented(&mut self) {
        if !self.first_frame
            && (!self.app.history.is_empty()
                || matches!(
                    self.app.load,
                    LoadState::Cached | LoadState::Complete | LoadState::Error(_)
                ))
        {
            self.first_frame = true;
            self.first_frame_at = Some(Instant::now());
            self.timing
                .mark(format!("first frame ({} rows)", self.app.history.len()));
        }
    }
    pub fn autoquit_ready(&self, delay: Duration) -> bool {
        if self.first_frame && matches!(self.app.load, LoadState::Error(_)) {
            return self.first_frame_at.is_some_and(|t| t.elapsed() >= delay);
        }
        self.first_frame
            && self.first_frame_at.is_some_and(|t| t.elapsed() >= delay)
            && self.walker.is_none()
            && self.app.pending.known
            && !self.app.pending.loading
            && !self.app.pending.busy
            && self.app.pending.request.is_none()
            && !self.app.branches.busy
            && !self.app.settings.saving
            && !self.app.search.busy
    }
    pub fn mark(&mut self, label: impl Into<String>) {
        self.timing.mark(label);
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        self.search_latest.store(u64::MAX, Ordering::Relaxed);
        self.preview_cancel.store(true, Ordering::Relaxed);
        if let Some(walker) = self.walker.take() {
            walker.cancel();
            walker.join();
        }
        for walker in self.finishing.drain(..) {
            walker.join();
        }
        self.timing.report();
    }
}
/// Both the cache and the repository are known: decide whether a walk is needed.
fn decide(app: &mut App, cache_fp: Option<u64>) {
    let Some(refs) = app.refs.as_ref() else {
        return;
    };
    let fresh = cache_fp == Some(refs.fingerprint) && app.history.complete;
    if fresh {
        app.load = LoadState::Cached;
        app.needs_redraw = true;
    } else {
        app.want_rewalk = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            String::from_utf8(crate::git::command::checked(dir.path(), args).unwrap()).unwrap()
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.name", "Desktop Test"]);
        git(&["config", "user.email", "desktop@example.invalid"]);
        git(&["config", "commit.gpgSign", "false"]);
        git(&["config", "core.hooksPath", "disabled-test-hooks"]);
        std::fs::write(dir.path().join("file.txt"), "first\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "First message"]);
        std::fs::write(dir.path().join("file.txt"), "second\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "Second message"]);
        dir
    }
    fn until(e: &mut Engine, ready: impl Fn(&Engine) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            e.tick(None);
            e.presented();
            if ready(e) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "engine timed out: {:?}",
                e.app.load
            );
            thread::sleep(Duration::from_millis(2));
        }
    }
    #[test]
    fn desktop_engine_load_search_diff_refresh_and_stale_result() {
        let dir = fixture();
        let mut e = Engine::new(dir.path().into(), true, Instant::now());
        until(&mut e, |e| {
            e.app.history.complete && e.app.pending.known && e.app.details.is_some()
        });
        assert_eq!(e.app.history.len(), 2);
        let head = e.app.head_id().unwrap();
        assert_eq!(e.app.selected_id(), Some(head));
        let stale = e.app.details.clone().unwrap();
        e.app.select(1);
        e.tick(Some(Msg::Details(head, Ok(stale))));
        until(&mut e, |e| {
            e.app.details.as_ref().map(|d| d.id) == e.app.selected_id()
        });
        assert_ne!(e.app.details.as_ref().unwrap().id, head);
        e.app.search.text.value = "m: First".into();
        e.app.search.changed();
        until(&mut e, |e| {
            e.app.search.result.as_ref().is_some_and(|r| r.done)
        });
        assert_eq!(e.app.graph_len(), 1);
        e.app.open_diff();
        until(
            &mut e,
            |e| matches!(&e.app.mode,app::Mode::Diff(v) if v.diff.is_some()),
        );
        if let app::Mode::Diff(v) = &e.app.mode {
            assert_eq!(v.changes.files[0].path, "file.txt");
        }
        e.app.close_diff();
        e.app.clear_search();
        e.app.want_refresh = true;
        until(&mut e, |e| {
            !e.refreshing && !e.app.want_refresh && !e.app.is_walking()
        });
        assert_eq!(e.app.history.len(), 2);
    }
    #[test]
    fn desktop_engine_commits_selected_files_and_preserves_unchecked_changes() {
        let dir = fixture();
        std::fs::write(dir.path().join("file.txt"), "selected change\n").unwrap();
        std::fs::write(dir.path().join("keep.txt"), "unselected\n").unwrap();
        let mut e = Engine::new(dir.path().into(), true, Instant::now());
        until(&mut e, |e| {
            e.app.pending.selected && e.app.details_changes.as_ref().is_some_and(|c| c.loaded)
        });
        let cs = e.app.details_changes.as_mut().unwrap();
        cs.set_all_checked(false);
        let node = cs
            .tree
            .nodes
            .iter()
            .position(|n| n.name == "file.txt")
            .unwrap();
        cs.toggle_checked(node);
        e.app.pending.description.value = "Desktop selected commit\n\nBody text".into();
        e.app.pending.branch.value = "desktop-topic".into();
        e.app.request_commit();
        assert!(e.app.pending.request.is_some());
        until(&mut e, |e| {
            !e.app.pending.busy
                && e.app.pending.request.is_none()
                && e.app.pending.description.value.is_empty()
        });
        assert!(!e.app.pending.error, "{}", e.app.pending.status);
        let git = |args: &[&str]| {
            String::from_utf8(crate::git::command::checked(dir.path(), args).unwrap()).unwrap()
        };
        assert!(git(&["branch", "--show-current"]).contains("desktop-topic"));
        assert!(git(&["show", "HEAD:file.txt"]).contains("selected change"));
        assert!(git(&["status", "--porcelain"]).contains("?? keep.txt"));
    }
    #[test]
    fn invalid_repository_produces_displayable_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = Engine::new(dir.path().into(), true, Instant::now());
        until(&mut e, |e| matches!(e.app.load, LoadState::Error(_)));
    }
    #[test]
    fn relative_launch_paths_show_the_repository_name_and_full_path() {
        const EXPECTED: &str = "GIT_RUSTLE_HEADER_TEST_ROOT";
        const INPUT: &str = "GIT_RUSTLE_HEADER_TEST_PATH";
        if let Some(root) = std::env::var_os(EXPECTED) {
            let root = std::path::absolute(root).unwrap();
            let input = std::env::var_os(INPUT).unwrap();
            let mut e = Engine::new(input.into(), true, Instant::now());
            until(&mut e, |e| e.repo.is_some());
            assert_eq!(e.repository_path(), root.display().to_string());
            assert_eq!(e.app.repo_name, root.file_name().unwrap().to_string_lossy());
            return;
        }

        let dir = fixture();
        let nested = dir.path().join("nested path é");
        std::fs::create_dir_all(nested.join("deeper")).unwrap();
        // Separate processes exercise the actual launch directory without
        // changing the working directory of concurrently running tests.
        for (cwd, input) in [
            (dir.path(), "."),
            (dir.path(), "nested path é/.."),
            (dir.path(), "nested path é/deeper"),
            (nested.as_path(), ".."),
        ] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "engine::tests::relative_launch_paths_show_the_repository_name_and_full_path",
                    "--nocapture",
                ])
                .current_dir(cwd)
                .env(EXPECTED, dir.path())
                .env(INPUT, input)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "launch from {} with {input}:\n{}\n{}",
                cwd.display(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
        }
    }
    #[test]
    fn returning_from_pending_reloads_the_same_commit_details() {
        let dir = fixture();
        std::fs::write(dir.path().join("pending.txt"), "uncommitted\n").unwrap();
        let mut e = Engine::new(dir.path().into(), true, Instant::now());
        until(&mut e, |e| e.app.history.complete && e.app.pending.known);
        let head = e.app.head_id().unwrap();
        let index = e.app.history.index_of(&head).unwrap();
        e.app.begin_navigation();
        e.app.select(index);
        until(&mut e, |e| {
            e.app.details.as_ref().is_some_and(|d| d.id == head)
        });
        e.app.select_graph(e.app.pending.row());
        e.tick(None);
        assert!(e.app.pending.selected);
        assert!(e.app.details.is_none());
        e.app.select(index);
        until(&mut e, |e| {
            e.app.details.as_ref().is_some_and(|d| d.id == head)
        });
        assert_eq!(
            e.app.details.as_ref().unwrap().message.trim(),
            "Second message"
        );
        // Remote preview details are installed synchronously from the ghost cache.
        // They must obey the same completed-request lifecycle as worker results.
        e.app.ghosts.insert(head, e.app.details.clone().unwrap());
        e.app.details = None;
        e.tick(None);
        assert!(e.app.details.is_some());
        e.app.select_graph(e.app.pending.row());
        e.tick(None);
        assert!(e.app.details.is_none());
        e.app.select(index);
        until(&mut e, |e| {
            e.app.details.as_ref().is_some_and(|d| d.id == head)
        });
    }
    #[test]
    #[ignore = "benchmarks commits and checkout in disposable repositories"]
    fn benchmark_desktop_mutations() {
        for sample in -1..15 {
            let dir = fixture();
            std::fs::write(dir.path().join("file.txt"), "benchmark change\n").unwrap();
            let start = Instant::now();
            let mut e = Engine::new(dir.path().into(), true, Instant::now());
            until(&mut e, |e| {
                e.app.pending.selected && e.app.details_changes.as_ref().is_some_and(|c| c.loaded)
            });
            let status_ms = start.elapsed().as_secs_f64() * 1000.;
            let main = e.app.head_id().unwrap();
            e.app.pending.description.value = "Benchmark selected commit".into();
            e.app.pending.branch.value = "benchmark-topic".into();
            let start = Instant::now();
            e.app.request_commit();
            until(&mut e, |e| {
                !e.app.pending.busy
                    && e.app.pending.request.is_none()
                    && e.app.pending.description.value.is_empty()
            });
            let commit_ms = start.elapsed().as_secs_f64() * 1000.;
            assert!(!e.app.pending.error, "{}", e.app.pending.status);
            until(&mut e, |e| {
                !e.app.is_walking()
                    && !e.refreshing
                    && !e.app.want_refresh
                    && e.app.head_id() != Some(main)
            });
            let start = Instant::now();
            e.app.checkout_request = Some(crate::checkout::Request::Branch(branches::Target {
                name: "main".into(),
                remote: None,
                url: None,
                sha: main.to_string(),
            }));
            until(&mut e, |e| {
                e.app.checkout_request.is_none()
                    && !e.app.branches.busy
                    && e.app.checkout_notice.is_some()
            });
            let checkout_ms = start.elapsed().as_secs_f64() * 1000.;
            assert!(e.app.checkout_notice.as_ref().unwrap().is_ok());
            if sample >= 0 {
                println!(
                    "{}",
                    serde_json::json!({"sample":sample,"statusAndHistoryMs":status_ms,"commitMs":commit_ms,"checkoutMs":checkout_ms})
                );
            }
        }
    }
    #[test]
    fn early_desktop_navigation_survives_late_worktree_status() {
        let dir = fixture();
        let mut e = Engine::new(dir.path().into(), true, Instant::now());
        until(&mut e, |e| e.app.history.complete && e.app.pending.known);
        e.app.begin_navigation();
        e.app.select_graph(1);
        let selected = e.app.selected_id();
        std::fs::write(dir.path().join("late.txt"), "late change").unwrap();
        let snapshot = git::worktree::snapshot(e.repo.as_ref().unwrap()).unwrap();
        e.app.install_pending(Ok(snapshot));
        assert!(!e.app.pending.selected);
        assert_eq!(e.app.selected_id(), selected);
    }
}
