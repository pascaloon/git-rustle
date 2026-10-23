//! Event-driven orchestration shared by the GPUI view and integration harness.
use crate::diffview::DiffReq;
mod remote_diff;
use crate::model::History;
use crate::{
    app::{self, App, LoadState, RefreshMode},
    branches, cache, diff, git, pending, remote, search, settings, ui,
};
use async_channel::{Receiver, Sender};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};
pub(crate) enum Msg {
    Wake,
    Recents(Result<Vec<PathBuf>, String>),
    Operation(Box<git::operations::Outcome>),
    OperationsView(Box<git::operations::View>),
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
    ConflictRefresh(
        u64,
        String,
        Result<
            (
                git::operations::conflicts::Refresh,
                Option<git::operations::ConflictPreview>,
            ),
            String,
        >,
    ),
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
    DiffSyntax(u32, u32, usize, Arc<diff::syntax::Syntax>),
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
            let mut syntax: Option<diff::syntax::Worker> = None;
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
                    let source = result.as_ref().ok().map(|d| d.source.clone());
                    if tx
                        .send_blocking(Msg::FileDiff(view, seq, file, result))
                        .is_err()
                    {
                        return;
                    }
                    if let Some(source) = source {
                        syntax
                            .get_or_insert_with(|| diff::syntax::Worker::new(tx.clone()))
                            .submit(
                                (view, seq, file),
                                source,
                                [
                                    change.old_path.unwrap_or_else(|| change.path.clone()),
                                    change.path,
                                ],
                            );
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

fn graph_requests(app: &App) -> Vec<branches::Target> {
    let mut requested = app.branches.preview_targets.clone();
    for update in app.branches.remotes.values() {
        if let Some(refs) = &app.refs {
            for name in remote::graph_names(refs, &update.remote) {
                requested.push(branches::Target {
                    name,
                    remote: Some(update.remote.name.clone()),
                    url: Some(update.remote.url.clone()),
                    sha: String::new(),
                });
            }
        }
    }
    for (id, label) in &app.ghost_branches {
        if let Some(source) = app.ghost_sources.get(id)
            && let Some(name) = label.strip_prefix(&format!("{}/", source.name))
        {
            requested.push(branches::Target {
                name: name.into(),
                remote: Some(source.name.clone()),
                url: Some(source.url.clone()),
                sha: id.to_string(),
            });
        }
    }
    requested
}

fn graph_updates(app: &App) -> Vec<remote::Update> {
    let requested = graph_requests(app);
    app.branches
        .remotes
        .values()
        .map(|update| {
            let names: std::collections::HashSet<_> = requested
                .iter()
                .filter(|t| {
                    t.remote.as_ref() == Some(&update.remote.name)
                        && t.url.as_ref().is_none_or(|url| url == &update.remote.url)
                })
                .map(|t| &t.name)
                .collect();
            let mut result = remote::Update {
                remote: update.remote.clone(),
                branches: update
                    .branches
                    .iter()
                    .filter(|b| names.contains(&b.name))
                    .cloned()
                    .collect(),
                scoped: true,
                cached: update.cached,
                stale: update.stale,
            };
            result.branches.sort_by(|a, b| a.name.cmp(&b.name));
            result
        })
        .collect()
}

fn same_graph_updates(a: &[remote::Update], b: &[remote::Update]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(a, b)| a.remote == b.remote && a.branches == b.branches)
}

pub struct Engine {
    pub recents: Vec<PathBuf>,
    pub recents_loading: bool,
    pub recents_error: String,
    pub app: App,
    pub operations: git::operations::View,
    pub operation_status: String,
    pub operation_error: bool,
    pub operation_busy: bool,
    /// A merge editor or explicit ref-storage conversion owns repository writes.
    pub repository_edit_busy: bool,
    pub operation_epoch: u64,
    pub stash_epoch: u64,
    pub stash_revision: u64,
    pub stash_list_stamp: Option<u64>,
    stash_stamp: Option<u64>,
    pub conflict_refresh_error: Option<String>,
    conflict_refresh_busy: bool,
    conflict_refresh_at: Instant,
    operation_request: Option<git::operations::Request>,
    operations_initialized: bool,
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
    remote_diff_tx: Option<remote_diff::Worker>,
    stats_tx: Option<std::sync::mpsc::Sender<StatsReq>>,
    stats_latest: Arc<AtomicU32>,
    stats_sent_for: Option<u32>,
    cache_fp: Option<u64>,
    cache_seen: bool,
    repo_seen: bool,
    refreshing: bool,
    refresh_mode: Option<RefreshMode>,
    next_remote_mode: Option<RefreshMode>,
    remote_explicit: bool,
    preview_cached_only: bool,
    walker: Option<Walker>,
    finishing: Vec<Walker>,
    next_walk_id: u32,
    first_frame: bool,
    remote_generation: u32,
    remote_cancel: Arc<AtomicBool>,
    preview_epoch: u32,
    preview_cancel: Arc<AtomicBool>,
    preview_busy: bool,
    preview_running_epoch: Option<u32>,
    // Includes obsolete workers waiting for their bounded tag HTTP response.
    preview_workers: usize,
    preview_wanted: bool,
    preview_force: bool,
    activity_busy: bool,
    local_activity_done: Option<u64>,
    auto_remote: bool,
    remote_scoped_request: bool,
    first_frame_at: Option<Instant>,
}
impl Engine {
    /// Queued/in-flight refresh work, excluding periodic worktree status probes.
    pub fn refresh_in_progress(&self) -> bool {
        self.app.refresh_request.is_some() || self.refresh_work_busy()
    }

    pub fn full_refresh_pending(&self) -> bool {
        self.app.refresh_request == Some(RefreshMode::Full)
            || (self.refresh_mode == Some(RefreshMode::Full) && self.refresh_work_busy())
    }

    fn refresh_work_busy(&self) -> bool {
        self.app.want_refresh
            || self.refreshing
            || self.app.want_rewalk
            || self.app.is_walking()
            || self.app.branches.want_refresh.is_some()
            || self.app.branches.loading
            || self.preview_busy
            || self.preview_workers != 0
            || self.preview_wanted
            || (self.refresh_mode.is_some()
                && (self.app.pending.loading || self.app.pending.want_refresh))
    }

    pub fn stash_status_stamp(&self) -> Option<u64> {
        self.stash_stamp
    }
    pub fn no_cache(&self) -> bool {
        self.no_cache
    }
    pub fn request_operation(&mut self, request: git::operations::Request) -> bool {
        if !self.repository_edit_busy
            && !self.operation_busy
            && self.operation_request.is_none()
            && !self.app.pending.busy
            && !self.app.branches.busy
            && !self.app.settings.saving
        {
            self.operation_request = Some(request);
            true
        } else {
            false
        }
    }
    pub fn begin_repository_edit(&mut self) -> bool {
        if self.repository_edit_busy
            || self.operation_busy
            || self.operation_request.is_some()
            || self.app.pending.busy
            || self.app.pending.request.is_some()
            || self.app.branches.busy
            || self.app.checkout_request.is_some()
            || self.app.settings.saving
            || self.repo.is_none()
        {
            return false;
        }
        self.repository_edit_busy = true;
        true
    }

    pub fn end_repository_edit(&mut self) {
        self.repository_edit_busy = false;
        // Never replay a shortcut queued against the pre-editor repository state.
        self.app.pending.request = None;
        self.app.checkout_request = None;
        self.app.want_refresh = true;
        self.app.pending.want_refresh = true;
        self.operation_request
            .get_or_insert(git::operations::Request::Inspect);
    }
    pub fn repository_path(&self) -> String {
        self.repository_root()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    }
    pub fn repository_root(&self) -> Option<&Path> {
        self.repo.as_ref().map(|r| r.workdir().unwrap_or(r.path()))
    }
    pub fn reference_storage_reftable(&self) -> bool {
        self.repo.as_ref().is_some_and(git::refs::is_reftable)
    }
    pub fn landing(&self) -> bool {
        self.repo_seen && self.repo.is_none()
    }
    pub fn open_directory(&self, path: PathBuf) -> Self {
        // Release the desktop task from this engine's receiver immediately.
        let _ = self.tx.try_send(Msg::Wake);
        Self::new(path, self.no_cache, Instant::now())
    }
    pub fn forget_repository(&mut self, path: PathBuf) {
        if self.recents_loading {
            return;
        }
        self.recents_loading = true;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = crate::recent::file()
                .and_then(|file| crate::recent::update(&file, crate::recent::Change::Forget(path)))
                .map_err(|e| e.to_string());
            let _ = tx.send_blocking(Msg::Recents(result));
        });
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
            let opened = result.is_ok();
            let recent_path = result
                .as_ref()
                .ok()
                .map(|(r, _)| r.workdir().unwrap_or(r.path()).to_path_buf());
            let _ = repo_tx.send_blocking(Msg::Repo(Box::new(result)));
            if !cfg!(test) {
                let result = crate::recent::file()
                    .and_then(|file| {
                        crate::recent::update(
                            &file,
                            recent_path
                                .map_or(crate::recent::Change::Load, crate::recent::Change::Open),
                        )
                    })
                    .map_err(|e| e.to_string());
                let _ = repo_tx.send_blocking(Msg::Recents(result));
            }
            if opened {
                // Git for Windows config and attributes path discovery may
                // launch Git. Warm both shared caches on this worker before a
                // pending file diff needs the full Git filter configuration.
                let _ = gix::path::env::installation_config();
                let _ = gix::path::env::system_prefix();
            }
        });
        let mut app = App::new();
        app.pending.known = false;
        app.pending.autofocus = true;
        app.pending.want_refresh = true;
        Self {
            recents: vec![],
            recents_loading: !cfg!(test),
            recents_error: String::new(),
            operations: Default::default(),
            operation_status: String::new(),
            operation_error: false,
            operation_busy: false,
            repository_edit_busy: false,
            operation_epoch: 0,
            stash_epoch: 0,
            stash_revision: 0,
            stash_list_stamp: None,
            stash_stamp: None,
            conflict_refresh_error: None,
            conflict_refresh_busy: false,
            conflict_refresh_at: Instant::now(),
            operation_request: None,
            operations_initialized: false,
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
            refresh_mode: None,
            next_remote_mode: None,
            remote_explicit: false,
            preview_cached_only: false,
            walker: None,
            finishing: Vec::new(),
            next_walk_id: 1,
            first_frame: false,
            remote_generation: 0,
            remote_cancel: Arc::new(AtomicBool::new(false)),
            preview_epoch: 0,
            preview_cancel: Arc::new(AtomicBool::new(false)),
            preview_busy: false,
            preview_running_epoch: None,
            preview_workers: 0,
            preview_wanted: false,
            preview_force: false,
            activity_busy: false,
            local_activity_done: None,
            auto_remote: false,
            remote_scoped_request: false,
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

    pub fn set_provider_search_labels(
        &mut self,
        labels: std::collections::HashMap<gix::ObjectId, Vec<String>>,
    ) -> bool {
        self.app.set_provider_search_labels(labels)
    }
    fn advance(&mut self, message: Option<Msg>) {
        if self.refresh_mode.is_some() && !self.refresh_work_busy() {
            self.refresh_mode = None;
        }
        if let Some(mode) = self.app.refresh_request {
            if self.refresh_mode.is_some_and(|active| active >= mode) {
                self.app.refresh_request = None;
            } else if self.refresh_mode.is_none()
                && !self.refresh_work_busy()
                && !self.operation_busy
                && !self.repository_edit_busy
                && !self.app.pending.busy
                && !self.app.branches.busy
                && !self.app.settings.saving
                && self.repo.is_some()
            {
                self.app.refresh_request = None;
                self.refresh_mode = Some(mode);
                self.next_remote_mode = Some(mode);
                self.preview_cached_only = true;
                self.app.want_refresh = true;
                self.app.pending.want_refresh = true;
                self.timing.mark("explicit local refresh requested");
            }
        }
        if !self.operation_busy
            && self.operation_request.is_none()
            && !self.app.pending.busy
            && !self.app.branches.busy
            && !self.conflict_refresh_busy
            && self.conflict_refresh_at.elapsed() >= Duration::from_millis(500)
            && let (Some(conflict), Some(repo)) = (&self.operations.conflict, &self.repo)
        {
            let previous = conflict.clone();
            let identity = previous.identity.clone();
            let epoch = self.operation_epoch;
            let selected = self.operations.preview.as_ref().map(|p| p.path.clone());
            let root = repo.workdir().unwrap_or(repo.git_dir()).to_path_buf();
            let tx = self.tx.clone();
            self.conflict_refresh_busy = true;
            self.conflict_refresh_at = Instant::now();
            thread::spawn(move || {
                let result = (|| {
                    use git::operations::conflicts::{self, Refresh};
                    let refreshed = conflicts::refresh(&root, &previous)?;
                    let preview = if let (Refresh::Changed(Some(current)), Some(path)) =
                        (&refreshed, selected)
                        && current.identity == identity
                        && current.files.contains(&path)
                    {
                        conflicts::read_preview(&root, &path, &identity).ok()
                    } else {
                        None
                    };
                    Ok((refreshed, preview))
                })();
                let _ = tx.send_blocking(Msg::ConflictRefresh(epoch, identity, result));
            });
        }
        if self.first_frame && self.repo.is_some() && !self.operations_initialized {
            self.operations_initialized = true;
            self.operation_request
                .get_or_insert(git::operations::Request::Inspect);
        }
        if !self.repository_edit_busy
            && !self.operation_busy
            && !self.app.pending.busy
            && !self.app.branches.busy
            && !self.app.settings.saving
            && let Some(repo) = &self.repo
            && let Some(request) = self.operation_request.take()
        {
            self.operation_busy = true;
            self.operation_status = "Working… You can keep browsing while Git finishes.".into();
            self.operation_error = false;
            if request.changes_repository() {
                self.app.pending.loading = false;
                self.pending_generation = self.pending_generation.wrapping_add(1);
            }
            self.app.needs_redraw = true;
            let path = repo.workdir().unwrap_or(repo.git_dir()).to_path_buf();
            let tx = self.tx.clone();
            self.timing.mark("git operation requested");
            thread::spawn(move || {
                let _ = tx.send_blocking(Msg::Operation(Box::new(git::operations::execute(
                    &path, request,
                ))));
            });
        }
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
            if !self.repository_edit_busy
                && !self.operation_busy
                && !app.settings.saving
                && !app.pending.busy
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
                    let outcome =
                        git::operations::execute(&path, git::operations::Request::Commit(request));
                    let _ = tx.send_blocking(Msg::OperationsView(Box::new(outcome.view)));
                    let _ = tx.send_blocking(Msg::Committed(outcome.result));
                });
            }
            if self.first_frame
                && !self.repository_edit_busy
                && !self.operation_busy
                && !app.pending.busy
                && !app.branches.busy
                && !app.pending.loading
                && (matches!(app.mode, app::Mode::Graph) || self.refresh_mode.is_some())
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
            && self.next_remote_mode.is_none()
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
                let updates = if self.preview_cached_only || self.preview_force {
                    graph_updates(app)
                } else {
                    app.branches.remotes.values().cloned().collect::<Vec<_>>()
                };
                let requested = graph_requests(app);
                let seed = remote::history::RetainedPreview {
                    ghosts: app.ghosts.clone(),
                    sources: app.ghost_sources.clone(),
                    branches: app.ghost_branches.clone(),
                    tags: app.ghost_tags.clone(),
                };
                let r = r.clone();
                let tx = tx.clone();
                let epoch = self.preview_epoch;
                self.preview_running_epoch = Some(epoch);
                self.preview_workers += 1;
                let force = std::mem::take(&mut self.preview_force);
                let policy = if self.preview_cached_only {
                    remote::history::TagPolicy::Cached
                } else if force {
                    remote::history::TagPolicy::Force
                } else {
                    remote::history::TagPolicy::IfStale
                };
                self.preview_cancel = Arc::new(AtomicBool::new(false));
                let cancel = self.preview_cancel.clone();
                app.ghost_status = if self.preview_cached_only {
                    "Checking remote commits…"
                } else {
                    "Checking remote commits and tags…"
                }
                .into();
                app.needs_redraw = true;
                timing.mark("ghost preview requested");
                thread::spawn(move || {
                    let preview = remote::history::load_stream_policy(
                        &r,
                        &local,
                        &refs,
                        &updates,
                        &requested,
                        policy,
                        &seed,
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
        if !self.repository_edit_busy
            && !self.operation_busy
            && !app.pending.busy
            && !app.branches.busy
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
        if !self.repository_edit_busy
            && !app.settings.saving
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
                let mode = self.next_remote_mode.take();
                self.remote_explicit = mode.is_some();
                // Startup, browser discovery and Smart Refresh hydrate commits
                // with cached tags. Only explicit Full Refresh loads remote tag
                // pages, after its branch discovery has completed.
                self.preview_cached_only = true;
                if mode != Some(RefreshMode::Smart) {
                    self.preview_epoch = self.preview_epoch.wrapping_add(1);
                    self.preview_cancel.store(true, Ordering::Relaxed);
                    self.preview_wanted |= app.branches.pending_jump.is_some() || self.preview_busy;
                }
                self.remote_generation = self.remote_generation.wrapping_add(1);
                let generation = self.remote_generation;
                self.preview_force |= force && mode != Some(RefreshMode::Smart);
                app.branches.loading = true;
                app.branches.errors.clear();
                app.needs_redraw = true;
                let git_dir = git::git_dir(r);
                let tx = tx.clone();
                let path = path.clone();
                let scope = match mode {
                    Some(RefreshMode::Smart) if !app.branches.open => remote::DiscoveryScope::Graph,
                    Some(RefreshMode::Full | RefreshMode::Smart) => remote::DiscoveryScope::All,
                    None if app.branches.open => remote::DiscoveryScope::All,
                    // Include fetched tips even without a local checkout.
                    None => remote::DiscoveryScope::Graph,
                };
                let local_only = scope != remote::DiscoveryScope::All;
                // Reuse the local worker snapshot instead of enumerating refs again.
                let refs = app.refs.clone();
                let mut requested = app.branches.preview_targets.clone();
                for (id, label) in &app.ghost_branches {
                    if let Some(source) = app.ghost_sources.get(id)
                        && let Some(name) = label.strip_prefix(&format!("{}/", source.name))
                    {
                        requested.push(crate::branches::Target {
                            name: name.into(),
                            remote: Some(source.name.clone()),
                            url: Some(source.url.clone()),
                            sha: id.to_string(),
                        });
                    }
                }
                self.remote_cancel.store(true, Ordering::Relaxed);
                self.remote_cancel = Arc::new(AtomicBool::new(false));
                let cancel = self.remote_cancel.clone();
                self.remote_scoped_request = local_only;
                timing.mark("remote discovery requested");
                thread::spawn(move || {
                    remote::discover_scoped(
                        &path,
                        &git_dir,
                        force,
                        no_cache,
                        scope,
                        refs.as_ref(),
                        &requested,
                        &cancel,
                        |event| {
                            let _ = tx.send_blocking(Msg::Remote(generation, event));
                        },
                    )
                });
            }
            if !self.operation_busy
                && !app.branches.busy
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
                    let remote = target.is_remote();
                    let outcome =
                        git::operations::execute(&path, git::operations::Request::Checkout(target));
                    let _ = tx.send_blocking(Msg::OperationsView(Box::new(outcome.view)));
                    let _ = tx.send_blocking(Msg::Checkout(outcome.result, remote));
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
                    .filter(|e| e.activity.is_none())
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
        if let (Some(r), Some(id)) = (self.repo.as_ref(), app.details_id())
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
            let mut remote_reqs = Vec::new();
            if !reqs.is_empty() {
                for req in reqs {
                    if matches!(req, DiffReq::Tree { .. }) {
                        timing.mark("diff requested");
                    } else {
                        timing.mark("file diff requested");
                    }
                    let remote_request = match &req {
                        DiffReq::Tree { remote, .. } => remote.is_some(),
                        DiffReq::File { change, .. } => change.remote.is_some(),
                    };
                    if remote_request {
                        remote_reqs.push(req);
                    } else {
                        let dtx = self.diff_tx.get_or_insert_with(|| {
                            spawn_diff_worker(r.clone(), tx.clone(), no_cache)
                        });
                        let _ = dtx.send(req);
                    }
                }
            }
            if !remote_reqs.is_empty() && self.remote_diff_tx.is_none() {
                self.remote_diff_tx =
                    Some(remote_diff::Worker::new(r.clone(), tx.clone(), no_cache));
            }
            if let Some(worker) = &mut self.remote_diff_tx {
                worker.update(app, remote_reqs);
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
                Msg::Operation(outcome) => {
                    self.operation_busy = false;
                    self.operation_error = outcome.result.is_err();
                    self.operation_status = outcome.result.unwrap_or_else(|e| e);
                    let plan = self.operations.plan.take();
                    let preview = self.operations.preview.take();
                    if outcome.view.stashes.is_some() {
                        self.stash_list_stamp = outcome.view.stash_stamp;
                        self.stash_epoch = self.stash_epoch.wrapping_add(1);
                    }
                    let stashes = self.operations.stashes.take();
                    self.operations = outcome.view;
                    if self.operations.stashes.is_none() {
                        self.operations.stashes = stashes;
                    }
                    if self.operations.plan.is_none() {
                        self.operations.plan = plan;
                    }
                    if self.operations.preview.is_none() {
                        self.operations.preview = preview;
                    }
                    if self.operations.preview.as_ref().is_some_and(|p| {
                        !self
                            .operations
                            .conflict
                            .as_ref()
                            .is_some_and(|c| c.files.contains(&p.path))
                    }) {
                        self.operations.preview = None;
                    }
                    self.operation_epoch = self.operation_epoch.wrapping_add(1);
                    if outcome.refresh_repository {
                        app.pending.want_refresh = true;
                        app.want_refresh = true;
                    }
                    if outcome.refresh_remotes {
                        // API branch tips override local tracking labels. Bypass their
                        // cache after publishing, just as a manual remote refresh does.
                        app.branches.want_refresh = Some(true);
                    }
                    app.needs_redraw = true;
                    timing.mark("git operation done");
                }
                Msg::OperationsView(view) => {
                    let stashes = self.operations.stashes.take();
                    self.operations = *view;
                    if self.operations.stashes.is_none() {
                        self.operations.stashes = stashes;
                    }
                    self.operation_epoch = self.operation_epoch.wrapping_add(1);
                }
                Msg::ConflictRefresh(epoch, identity, result) => {
                    self.conflict_refresh_busy = false;
                    if epoch != self.operation_epoch
                        || self.operation_busy
                        || self.operation_request.is_some()
                        || self
                            .operations
                            .conflict
                            .as_ref()
                            .is_none_or(|c| c.identity != identity)
                    {
                        continue;
                    }
                    match result {
                        Ok((git::operations::conflicts::Refresh::Unchanged, _)) => {
                            if self.conflict_refresh_error.take().is_some() {
                                app.needs_redraw = true;
                            }
                        }
                        Ok((git::operations::conflicts::Refresh::Changed(conflict), preview)) => {
                            if conflict.is_none() {
                                self.operation_request
                                    .get_or_insert(git::operations::Request::Inspect);
                            }
                            self.operations.conflict = conflict.map(|conflict| *conflict);
                            self.operations.preview = preview;
                            self.conflict_refresh_error = None;
                            self.operation_epoch = self.operation_epoch.wrapping_add(1);
                            app.want_refresh = true;
                            app.pending.want_refresh = true;
                            app.needs_redraw = true;
                            timing.mark("conflict external changes ready");
                        }
                        Err(error) => {
                            if self.conflict_refresh_error.as_ref() != Some(&error) {
                                self.conflict_refresh_error = Some(error);
                                app.needs_redraw = true;
                            }
                        }
                    }
                }
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
                            if self.stash_stamp != snapshot.stash_stamp {
                                self.stash_stamp = snapshot.stash_stamp;
                                self.stash_revision = self.stash_revision.wrapping_add(1);
                            }
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
                        if result.as_ref().is_ok_and(|s| s.conflicts)
                            && self.operations.conflict.is_none()
                        {
                            self.operation_request
                                .get_or_insert(git::operations::Request::Inspect);
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
                    if self.preview_running_epoch == Some(epoch) {
                        self.preview_busy = false;
                        self.preview_running_epoch = None;
                    }
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
                    self.preview_workers = self.preview_workers.saturating_sub(1);
                    if self.preview_running_epoch == Some(epoch) {
                        self.preview_busy = false;
                        self.preview_running_epoch = None;
                    }
                    if epoch == self.preview_epoch
                        && !app.branches.preview_requested
                        && !app.is_walking()
                        && !app.want_rewalk
                    {
                        let tags_changed = app.ghost_tags != tags;
                        app.ghost_tags = tags;
                        if !errors.is_empty() {
                            app.ghost_status =
                                format!("Remote preview incomplete: {}", errors.join(" | "));
                        }
                        if tags_changed && let Some(refs) = app.refs.clone() {
                            app.set_refs(refs);
                        }
                    }
                }
                Msg::Settings(session, outcome) => {
                    if matches!(
                        &outcome,
                        settings::Outcome::Saved(Ok(_)) | settings::Outcome::SavedPage(Ok(_))
                    ) {
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
                        self.remote_cancel.store(true, Ordering::Relaxed);
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
                    // Partial API batches never revoke identities that have not
                    // answered yet. Scope prevents a prior full branch-browser
                    // listing from broadening graph hydration to unrelated heads.
                    let event = match event {
                        remote::Event::HeadsCached { mut update, scope } => {
                            if let Some(previous) = app
                                .branches
                                .remotes
                                .get(&update.remote.name)
                                .filter(|old| old.remote == update.remote)
                            {
                                // Memory also knows authoritative absences. Disk
                                // only bootstraps a remote with no current snapshot;
                                // it cannot resurrect a name omitted by a newer scan.
                                update.branches = previous
                                    .branches
                                    .iter()
                                    .filter(|b| scope.contains(&b.name))
                                    .cloned()
                                    .collect();
                                update.branches.sort_by(|a, b| a.name.cmp(&b.name));
                            }
                            let update = app.branches.merge_scoped(update, &scope);
                            remote::Event::Branches(update)
                        }
                        #[cfg(test)]
                        remote::Event::BranchesPartial { mut update, scope } => {
                            if let Some(previous) = app
                                .branches
                                .remotes
                                .get(&update.remote.name)
                                .filter(|old| old.remote == update.remote)
                            {
                                let seen: std::collections::HashSet<_> =
                                    update.branches.iter().map(|b| b.name.clone()).collect();
                                update.branches.extend(
                                    previous
                                        .branches
                                        .iter()
                                        .filter(|b| {
                                            scope.contains(&b.name) && !seen.contains(&b.name)
                                        })
                                        .cloned(),
                                );
                                update.branches.sort_by(|a, b| a.name.cmp(&b.name));
                            }
                            let update = app.branches.merge_scoped(update, &scope);
                            remote::Event::Branches(update)
                        }
                        remote::Event::HeadsVerified { update, scope } => {
                            remote::Event::Branches(app.branches.merge_scoped(update, &scope))
                        }
                        other => other,
                    };
                    match event {
                        remote::Event::Configured(remotes) => {
                            if remotes.is_empty() && app.local_history.is_some() {
                                self.preview_wanted = true;
                            }
                            let previous_count = app.branches.remotes.len();
                            app.branches.remotes.retain(|name, update| {
                                remotes
                                    .iter()
                                    .any(|(n, url)| n == name && url == &update.remote.url)
                            });
                            if app.branches.remotes.len() != previous_count {
                                self.preview_epoch = self.preview_epoch.wrapping_add(1);
                                self.preview_cancel.store(true, Ordering::Relaxed);
                                self.preview_wanted = true;
                            }
                            if app.branches.remotes.len() != previous_count
                                && let Some(refs) = app.refs.clone()
                            {
                                app.set_refs(refs);
                            }
                        }
                        remote::Event::Branches(update) => {
                            let previous_graph = graph_updates(app);
                            let changed =
                                app.branches
                                    .remotes
                                    .get(&update.remote.name)
                                    .is_none_or(|old| {
                                        old.remote != update.remote
                                            || old.branches != update.branches
                                    });
                            let live = !update.cached;
                            timing.mark(format!(
                                "remote {} branches ({})",
                                if update.cached { "cached" } else { "live" },
                                update.branches.len()
                            ));
                            if changed {
                                // Newly listed browser tips can already exist locally even
                                // though they were absent from the initial graph scope.
                                self.local_activity_done = None;
                                app.branches.update(update, app.refs.as_ref());
                                let next_graph = graph_updates(app);
                                if !same_graph_updates(&previous_graph, &next_graph) {
                                    self.preview_epoch = self.preview_epoch.wrapping_add(1);
                                    self.preview_cancel.store(true, Ordering::Relaxed);
                                    self.preview_wanted = true;
                                    app.remote_labels();
                                }
                            } else {
                                app.branches
                                    .remotes
                                    .insert(update.remote.name.clone(), update);
                            }
                            if live {
                                timing.mark("remote tips verified");
                            }
                        }
                        #[cfg(test)]
                        remote::Event::BranchesPartial { .. } => {
                            unreachable!("legacy partial normalized")
                        }
                        remote::Event::HeadsCached { .. } | remote::Event::HeadsVerified { .. } => {
                            unreachable!("non-authoritative heads normalized before dispatch")
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
                                && self.remote_scoped_request
                                && !self.remote_explicit
                            {
                                app.branches.want_refresh = Some(false);
                            }
                            if self.remote_explicit && self.refresh_mode == Some(RefreshMode::Full)
                            {
                                self.preview_cached_only = false;
                                self.preview_wanted = true;
                                self.preview_force = true;
                            }
                            if self.remote_explicit
                                && self.remote_scoped_request
                                && app.branches.errors.is_empty()
                            {
                                if app.ghost_status.starts_with("Remote preview incomplete:") {
                                    self.preview_wanted = true;
                                } else if app.ghost_status.starts_with("Remote:") {
                                    app.ghost_status = if app.ghosts.is_empty() {
                                        String::new()
                                    } else {
                                        format!("{} ghost commits (remote only)", app.ghosts.len())
                                    };
                                }
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
                            if changed {
                                if app.branches.loading && self.remote_explicit {
                                    self.remote_cancel.store(true, Ordering::Relaxed);
                                    self.remote_generation = self.remote_generation.wrapping_add(1);
                                    app.branches.loading = false;
                                    self.next_remote_mode = self.refresh_mode;
                                }
                                app.set_refs(snap);
                            }
                            self.repo = Some(r);
                            if self.next_remote_mode.is_some() {
                                app.branches.want_refresh = Some(true);
                            }
                            timing.mark("explicit local snapshot ready");
                            if changed {
                                self.preview_epoch = self.preview_epoch.wrapping_add(1);
                                self.preview_cancel.store(true, Ordering::Relaxed);
                                self.preview_wanted = true;
                                app.want_rewalk = true;
                            }
                        }
                        Err(error) => {
                            self.next_remote_mode = None;
                            app.set_error(error.to_string());
                        }
                    }
                }
                Msg::Details(id, result) => {
                    // A completed read must not suppress a later read of the same
                    // commit after Pending changes has cleared the details view.
                    if self.details_requested == Some(id)
                        && (result.is_ok() || app.details_id() != Some(id))
                    {
                        self.details_requested = None;
                    }
                    if app.details_id() == Some(id) {
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
                Msg::Wake => {}
                Msg::Recents(result) => {
                    self.recents_loading = false;
                    match result {
                        Ok(entries) => {
                            self.recents = entries;
                            self.recents_error.clear();
                        }
                        Err(error) => self.recents_error = format!("Recent repositories: {error}"),
                    }
                    timing.mark("recent repositories ready");
                    app.needs_redraw = true;
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
                Msg::DiffSyntax(view_id, seq, file, syntax) => {
                    if let Some(view) = app.diff_view_mut()
                        && view.id == view_id
                    {
                        let count = syntax.span_count();
                        if view.set_syntax(seq, file, syntax) {
                            timing.mark(format!("diff syntax ready ({count} spans)"));
                            app.needs_redraw = true;
                        }
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
    pub(super) fn local_refresh_ready(&self) -> bool {
        self.app.refresh_request.is_none()
            && !self.app.want_refresh
            && !self.refreshing
            && self.next_remote_mode.is_none()
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
    /// Optional application setup waits until the initial repository refresh is idle.
    pub fn external_tools_idle(&self) -> bool {
        self.autoquit_ready(Duration::ZERO)
            && !self.operation_busy
            && !self.repository_edit_busy
            && self.operation_request.is_none()
            && !self.app.pending.busy
            && !self.app.pending.loading
            && !self.app.branches.busy
            && !self.app.settings.saving
            && !self.app.search.busy
            && !self.refreshing
            && !self.app.want_refresh
            && !self.app.want_rewalk
            && !self.app.branches.loading
            && self.app.branches.want_refresh.is_none()
            && !self.preview_busy
            && self.preview_workers == 0
            && !self.preview_wanted
            && !self.activity_busy
    }

    pub fn autoquit_ready(&self, delay: Duration) -> bool {
        if self.recents_loading {
            return false;
        }
        if self.first_frame && matches!(self.app.load, LoadState::Error(_)) {
            return self.first_frame_at.is_some_and(|t| t.elapsed() >= delay);
        }
        self.first_frame
            && self.first_frame_at.is_some_and(|t| t.elapsed() >= delay)
            && !self.operation_busy
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
        self.remote_cancel.store(true, Ordering::Relaxed);
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
    #[test]
    fn non_repository_lands_and_can_open_repository_after_failure() {
        let empty = tempfile::tempdir().unwrap();
        let mut e = Engine::new(empty.path().into(), true, Instant::now());
        until(&mut e, Engine::landing);
        assert!(matches!(e.app.load, LoadState::Error(_)));
        let repo = fixture();
        let old_messages = e.messages();
        e = e.open_directory(repo.path().join("does-not-exist"));
        until(&mut e, Engine::landing);
        e = e.open_directory(repo.path().into());
        assert!(!old_messages.same_channel(&e.messages()));
        until(&mut e, |e| e.app.history.len() == 2 && !e.app.is_walking());
        assert!(!e.landing());
        assert_eq!(
            e.app.repo_name,
            repo.path().file_name().unwrap().to_string_lossy()
        );
    }

    #[test]
    fn empty_repository_opens_graph_instead_of_landing() {
        let repo = tempfile::tempdir().unwrap();
        crate::git::command::checked(repo.path(), &["init", "-q"]).unwrap();
        let nested = repo.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let mut e = Engine::new(nested, true, Instant::now());
        until(&mut e, |e| e.repo_seen);
        assert!(!e.landing());
        assert!(!e.repository_path().is_empty());
    }
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
    fn external_tool_setup_waits_for_initial_and_queued_repository_refreshes() {
        let dir = fixture();
        let mut e = Engine::new(dir.path().into(), true, Instant::now());
        assert!(!e.external_tools_idle());
        until(&mut e, Engine::external_tools_idle);

        e.app.want_refresh = true;
        assert!(!e.external_tools_idle());
        e.tick(None);
        assert!(!e.external_tools_idle());
        until(&mut e, Engine::external_tools_idle);

        e.app.branches.want_refresh = Some(true);
        assert!(!e.external_tools_idle());
        e.tick(None);
        assert!(!e.external_tools_idle());
        until(&mut e, Engine::external_tools_idle);
    }

    #[test]
    fn external_tool_setup_can_run_on_landing_without_worktree_status() {
        let dir = fixture();
        let mut e = Engine::new(dir.path().join("does-not-exist"), true, Instant::now());
        until(&mut e, |e| e.landing() && e.external_tools_idle());
        assert!(!e.app.pending.known);
        e.operation_busy = true;
        assert!(!e.external_tools_idle());
        e.operation_busy = false;
        assert!(e.external_tools_idle());
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

    fn quiet_engine(path: &Path) -> Engine {
        let mut engine = Engine::new(path.into(), true, Instant::now());
        until(&mut engine, |e| {
            e.app.history.complete
                && e.app.pending.known
                && !e.app.pending.loading
                && e.operations_initialized
                && !e.operation_busy
                && e.operation_request.is_none()
                && e.app.branches.last_check.is_some()
                && !e.app.branches.loading
                && e.app.branches.want_refresh.is_none()
                && !e.refreshing
        });
        engine
    }

    #[test]
    fn branches_smart_refresh_lists_all_heads_without_refreshing_tags() {
        let dir = fixture();
        let mut e = quiet_engine(dir.path());
        e.app.branches.last_check = None;
        e.app.branches.show(e.app.refs.as_ref());
        until(&mut e, |e| !e.refresh_in_progress());
        assert!(!e.remote_scoped_request);
        assert!(e.preview_cached_only);
        e.app.request_refresh(RefreshMode::Smart);
        until(&mut e, |e| !e.refresh_in_progress());
        assert!(
            !e.remote_scoped_request,
            "browser Smart must check all server branches"
        );
        assert!(
            e.preview_cached_only,
            "browser Smart must not paginate tags"
        );
        e.app.request_refresh(RefreshMode::Full);
        until(&mut e, |e| !e.refresh_in_progress());
        assert!(!e.preview_cached_only, "explicit Full still refreshes tags");
        e.app.branches.open = false;
        e.app.request_refresh(RefreshMode::Smart);
        until(&mut e, |e| !e.refresh_in_progress());
        assert!(e.remote_scoped_request, "graph Smart stays scoped");
        assert!(e.preview_cached_only);
    }

    #[test]
    fn smart_refresh_retains_unchanged_graph_search_details_and_checks_local_state() {
        let dir = fixture();
        let mut e = quiet_engine(dir.path());
        e.app.search.text.value = "m: First".into();
        e.app.search.changed();
        until(&mut e, |e| {
            e.app.search.result.as_ref().is_some_and(|r| r.done)
        });
        let generation = e.app.search.generation;
        let labels = e.app.labels_revision();
        let history = e.app.history.commits.as_ptr();
        let selected = e.app.selected_id();
        let input = e.app.search.input.clone().unwrap();
        let preview = e.preview_epoch;
        e.app.request_refresh(RefreshMode::Smart);
        until(&mut e, |e| !e.refresh_in_progress());
        assert_eq!(e.app.history.commits.as_ptr(), history);
        assert_eq!(e.app.selected_id(), selected);
        assert_eq!(e.app.labels_revision(), labels);
        assert_eq!(e.preview_epoch, preview);
        assert_eq!(e.app.search.generation, generation);
        assert!(Arc::ptr_eq(&input, e.app.search.input.as_ref().unwrap()));
        // A new local branch and worktree edit are visible without fetching.
        git::command::checked(dir.path(), &["branch", "external"]).unwrap();
        std::fs::write(dir.path().join("untracked.txt"), "retained local edit").unwrap();
        e.app.request_refresh(RefreshMode::Smart);
        until(&mut e, |e| !e.refresh_in_progress());
        assert!(
            e.app
                .refs
                .as_ref()
                .unwrap()
                .refs
                .iter()
                .any(|r| r.name == "external")
        );
        assert!(
            e.app
                .pending
                .snapshot
                .files
                .iter()
                .any(|f| f.path == "untracked.txt")
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("untracked.txt")).unwrap(),
            "retained local edit"
        );
        assert!(!dir.path().join(".git/FETCH_HEAD").exists());
    }

    #[test]
    fn smart_refresh_coalesces_and_full_upgrades_after_the_current_check() {
        let dir = fixture();
        let mut e = quiet_engine(dir.path());
        e.refresh_mode = Some(RefreshMode::Smart);
        e.app.branches.loading = true;
        e.remote_explicit = true;
        e.app.request_refresh(RefreshMode::Smart);
        e.tick(None);
        assert!(e.app.refresh_request.is_none());
        let generation = e.remote_generation;
        e.app.request_refresh(RefreshMode::Full);
        e.tick(None);
        assert_eq!(e.app.refresh_request, Some(RefreshMode::Full));
        assert_eq!(e.remote_generation, generation);
        e.repository_edit_busy = true;
        e.tick(Some(Msg::Remote(generation, remote::Event::Done)));
        assert_eq!(e.app.refresh_request, Some(RefreshMode::Full));
        e.repository_edit_busy = false;
        until(&mut e, |e| !e.refresh_in_progress());
        assert_eq!(e.remote_generation, generation + 1);
        assert!(!e.remote_scoped_request);
    }

    #[test]
    fn verified_scope_preserves_browser_heads_and_only_removes_confirmed_absences() {
        let dir = fixture();
        let mut e = quiet_engine(dir.path());
        e.app.branches.busy = true;
        let remote = remote::Remote::parse(
            "origin",
            "https://example.invalid/team/repo",
            Some("github"),
        )
        .unwrap();
        let head = e.app.head_id().unwrap().to_string();
        let branch = |name: &str| remote::Branch {
            name: name.into(),
            sha: head.clone(),
        };
        let full = remote::Update {
            remote: remote.clone(),
            branches: vec![
                branch("main"),
                branch("failed"),
                branch("deleted"),
                branch("browser-only"),
            ],
            scoped: false,
            cached: false,
            stale: false,
        };
        e.app.branches.update(full, e.app.refs.as_ref());
        let labels = e.app.labels_revision();
        let epoch = e.preview_epoch;
        e.tick(Some(Msg::Remote(
            e.remote_generation,
            remote::Event::HeadsVerified {
                update: remote::Update {
                    remote: remote.clone(),
                    branches: vec![branch("main")],
                    scoped: true,
                    cached: false,
                    stale: true,
                },
                scope: ["main".into(), "deleted".into()].into_iter().collect(),
            },
        )));
        let update = &e.app.branches.remotes["origin"];
        assert_eq!(
            update
                .branches
                .iter()
                .map(|b| b.name.as_str())
                .collect::<Vec<_>>(),
            ["browser-only", "failed", "main"]
        );
        assert!(!update.scoped);
        assert_eq!(e.app.labels_revision(), labels);
        assert_eq!(e.preview_epoch, epoch);
        assert_eq!(graph_updates(&e.app)[0].branches.len(), 1);
        e.tick(Some(Msg::Remote(
            e.remote_generation.wrapping_sub(1),
            remote::Event::HeadsVerified {
                update: remote::Update {
                    remote,
                    branches: vec![],
                    scoped: true,
                    cached: false,
                    stale: false,
                },
                scope: ["main".into()].into_iter().collect(),
            },
        )));
        assert_eq!(e.app.branches.remotes["origin"].branches.len(), 3);
    }

    #[test]
    fn refresh_remains_busy_until_obsolete_tag_worker_finishes_without_blocking_new_overlay() {
        let dir = fixture();
        let mut e = quiet_engine(dir.path());
        e.app.branches.busy = true;
        e.preview_epoch = 7;
        e.preview_running_epoch = Some(7);
        e.preview_busy = true;
        e.preview_workers = 2;
        let preview = remote::history::Preview {
            history: None,
            ghosts: Default::default(),
            sources: Default::default(),
            branches: vec![],
            tags: vec![],
            errors: vec![],
        };
        e.tick(Some(Msg::Preview(7, preview)));
        assert!(!e.preview_busy);
        assert!(e.refresh_in_progress());
        e.tick(Some(Msg::PreviewTags(7, vec![], vec![])));
        assert_eq!(e.preview_workers, 1);
        assert!(e.refresh_in_progress());
        e.tick(Some(Msg::PreviewTags(6, vec![], vec![])));
        assert_eq!(e.preview_workers, 0);
        assert!(!e.refresh_in_progress());
    }

    #[test]
    fn disk_heads_bootstrap_empty_memory_but_never_resurrect_authoritatively_absent_names() {
        let dir = fixture();
        let mut e = quiet_engine(dir.path());
        e.app.branches.busy = true;
        let remote = remote::Remote::parse(
            "origin",
            "https://example.invalid/team/repo",
            Some("github"),
        )
        .unwrap();
        let branch = |name: &str, sha: &str| remote::Branch {
            name: name.into(),
            sha: sha.repeat(40),
        };
        let scope: std::collections::HashSet<String> =
            ["a".into(), "b".into()].into_iter().collect();
        let cached = remote::Update {
            remote: remote.clone(),
            branches: vec![branch("a", "a"), branch("b", "b")],
            scoped: true,
            cached: true,
            stale: false,
        };
        assert!(!e.app.branches.remotes.contains_key("origin"));
        e.tick(Some(Msg::Remote(
            e.remote_generation,
            remote::Event::HeadsCached {
                update: cached.clone(),
                scope: scope.clone(),
            },
        )));
        assert_eq!(
            e.app.branches.remotes["origin"].branches.len(),
            2,
            "cold cache still seeds missing heads"
        );
        // A complete newer branch-browser listing proves b is gone.
        e.tick(Some(Msg::Remote(
            e.remote_generation,
            remote::Event::Branches(remote::Update {
                remote: remote.clone(),
                branches: vec![branch("a", "c")],
                scoped: false,
                cached: false,
                stale: false,
            }),
        )));
        let epoch = e.preview_epoch;
        e.tick(Some(Msg::Remote(
            e.remote_generation,
            remote::Event::HeadsCached {
                update: cached,
                scope,
            },
        )));
        let heads = &e.app.branches.remotes["origin"].branches;
        assert_eq!(heads.len(), 1);
        assert_eq!(heads[0].name, "a");
        assert_eq!(heads[0].sha, "c".repeat(40));
        assert_eq!(e.preview_epoch, epoch);
        e.tick(Some(Msg::Remote(
            e.remote_generation,
            remote::Event::Status("fixture offline after cache replay".into()),
        )));
        assert!(
            !e.app.branches.remotes["origin"]
                .branches
                .iter()
                .any(|b| b.name == "b")
        );
    }
    #[test]
    fn old_disk_heads_cannot_rewind_verified_partial_or_add_unverified_missing_names() {
        let dir = fixture();
        let mut e = quiet_engine(dir.path());
        e.app.branches.busy = true;
        let remote = remote::Remote::parse(
            "origin",
            "https://example.invalid/team/repo",
            Some("github"),
        )
        .unwrap();
        let branch = |name: &str, sha: &str| remote::Branch {
            name: name.into(),
            sha: sha.repeat(40),
        };
        let scope: std::collections::HashSet<String> =
            ["a".into(), "b".into()].into_iter().collect();
        // A verified network batch has a newer a; the following request later
        // failed, so disk still has an older a plus previously unseen b.
        e.tick(Some(Msg::Remote(
            e.remote_generation,
            remote::Event::BranchesPartial {
                update: remote::Update {
                    remote: remote.clone(),
                    branches: vec![branch("a", "c")],
                    scoped: true,
                    cached: true,
                    stale: true,
                },
                scope: scope.clone(),
            },
        )));
        let epoch = e.preview_epoch;
        e.tick(Some(Msg::Remote(
            e.remote_generation,
            remote::Event::HeadsCached {
                update: remote::Update {
                    remote: remote.clone(),
                    branches: vec![branch("a", "a"), branch("b", "b")],
                    scoped: true,
                    cached: true,
                    stale: true,
                },
                scope: scope.clone(),
            },
        )));
        let heads = &e.app.branches.remotes["origin"].branches;
        assert_eq!(heads.len(), 1);
        assert!(
            heads
                .iter()
                .any(|b| b.name == "a" && b.sha == "c".repeat(40))
        );
        assert!(!heads.iter().any(|b| b.name == "b"));
        assert_eq!(e.preview_epoch, epoch);
        e.tick(Some(Msg::Remote(
            e.remote_generation,
            remote::Event::Status("fixture offline".into()),
        )));
        assert!(
            e.app.branches.remotes["origin"]
                .branches
                .iter()
                .any(|b| b.name == "a" && b.sha == "c".repeat(40))
        );
        // A batch checking only b must not resurrect cache-only a as if verified.
        e.tick(Some(Msg::Remote(
            e.remote_generation,
            remote::Event::BranchesPartial {
                update: remote::Update {
                    remote: remote.clone(),
                    branches: vec![branch("b", "e")],
                    scoped: true,
                    cached: true,
                    stale: true,
                },
                scope: scope.clone(),
            },
        )));
        assert!(
            e.app.branches.remotes["origin"]
                .branches
                .iter()
                .any(|b| b.name == "a" && b.sha == "c".repeat(40))
        );
        // A subsequent verified batch remains authoritative for its completed head.
        e.tick(Some(Msg::Remote(
            e.remote_generation,
            remote::Event::BranchesPartial {
                update: remote::Update {
                    remote,
                    branches: vec![branch("a", "d")],
                    scoped: true,
                    cached: true,
                    stale: true,
                },
                scope,
            },
        )));
        let heads = &e.app.branches.remotes["origin"].branches;
        assert_eq!(heads.len(), 2);
        assert!(
            heads
                .iter()
                .any(|b| b.name == "a" && b.sha == "d".repeat(40))
        );
    }
    #[test]
    fn partial_heads_preserve_unanswered_in_memory_scope_and_failures_never_revoke_it() {
        let dir = fixture();
        let mut e = quiet_engine(dir.path());
        e.app.branches.busy = true;
        let remote = remote::Remote::parse(
            "origin",
            "https://example.invalid/team/repo",
            Some("github"),
        )
        .unwrap();
        let branch = |name: &str, sha: &str| remote::Branch {
            name: name.into(),
            sha: sha.repeat(40),
        };
        e.app.branches.update(
            remote::Update {
                remote: remote.clone(),
                branches: vec![branch("a", "a"), branch("b", "b"), branch("unrelated", "e")],
                scoped: false,
                cached: false,
                stale: false,
            },
            e.app.refs.as_ref(),
        );
        let partial = remote::Update {
            remote: remote.clone(),
            branches: vec![branch("a", "c")],
            scoped: true,
            cached: true,
            stale: true,
        };
        e.tick(Some(Msg::Remote(
            e.remote_generation,
            remote::Event::BranchesPartial {
                update: partial,
                scope: ["a".into(), "b".into()].into_iter().collect(),
            },
        )));
        let heads = &e.app.branches.remotes["origin"].branches;
        assert_eq!(heads.len(), 3);
        assert!(
            heads
                .iter()
                .any(|b| b.name == "b" && b.sha == "b".repeat(40))
        );
        assert!(
            heads
                .iter()
                .any(|b| b.name == "a" && b.sha == "c".repeat(40))
        );
        e.tick(Some(Msg::Remote(
            e.remote_generation,
            remote::Event::Status("fixture timeout".into()),
        )));
        assert_eq!(e.app.branches.remotes["origin"].branches.len(), 3);
        // Only a completed authoritative set establishes that b is absent.
        e.tick(Some(Msg::Remote(
            e.remote_generation,
            remote::Event::Branches(remote::Update {
                remote,
                branches: vec![branch("a", "c")],
                scoped: true,
                cached: false,
                stale: false,
            }),
        )));
        assert_eq!(e.app.branches.remotes["origin"].branches.len(), 1);
    }
    #[test]
    fn removing_one_of_multiple_remotes_invalidates_its_preview_even_if_other_heads_unchanged() {
        let dir = fixture();
        let mut e = quiet_engine(dir.path());
        e.app.branches.busy = true;
        for name in ["origin", "other"] {
            let remote = remote::Remote::parse(
                name,
                &format!("https://example.invalid/team/{name}"),
                Some("github"),
            )
            .unwrap();
            e.app.branches.update(
                remote::Update {
                    remote,
                    branches: vec![remote::Branch {
                        name: "main".into(),
                        sha: "a".repeat(40),
                    }],
                    scoped: true,
                    cached: false,
                    stale: false,
                },
                e.app.refs.as_ref(),
            );
        }
        let other = e.app.branches.remotes["other"].remote.clone();
        let epoch = e.preview_epoch;
        let cancel = e.preview_cancel.clone();
        e.tick(Some(Msg::Remote(
            e.remote_generation,
            remote::Event::Configured(vec![(other.name.clone(), other.url.clone())]),
        )));
        assert_eq!(e.app.branches.remotes.len(), 1);
        assert!(e.preview_wanted);
        assert!(e.preview_epoch > epoch);
        assert!(cancel.load(Ordering::Relaxed));
    }
    #[test]
    fn unchanged_head_updates_preserve_preview_epoch_and_activity_cache() {
        let dir = fixture();
        let mut e = quiet_engine(dir.path());
        e.app.branches.busy = true;
        let id = e.app.history.commits[0].id.to_string();
        let remote = remote::Remote::parse(
            "origin",
            "https://example.invalid/team/repo",
            Some("github"),
        )
        .unwrap();
        let update = remote::Update {
            remote,
            branches: vec![remote::Branch {
                name: "main".into(),
                sha: id,
            }],
            scoped: true,
            cached: true,
            stale: false,
        };
        e.app.branches.update(update.clone(), e.app.refs.as_ref());
        let epoch = e.preview_epoch;
        e.preview_force = true;
        let mut live = update;
        live.cached = false;
        e.tick(Some(Msg::Remote(
            e.remote_generation,
            remote::Event::Branches(live),
        )));
        assert_eq!(e.preview_epoch, epoch);
        assert!(!e.preview_wanted);
    }
    #[test]
    fn obsolete_preview_releases_its_worker_but_cannot_replace_graph_or_end_new_worker() {
        let dir = fixture();
        let mut e = quiet_engine(dir.path());
        e.app.branches.busy = true;
        e.preview_epoch = 7;
        e.preview_busy = true;
        e.preview_running_epoch = Some(6);
        let count = e.app.history.len();
        let preview = remote::history::Preview {
            history: None,
            ghosts: Default::default(),
            sources: Default::default(),
            branches: vec![],
            tags: vec![],
            errors: vec![],
        };
        e.tick(Some(Msg::Preview(6, preview)));
        assert!(!e.preview_busy);
        assert_eq!(e.app.history.len(), count);
        e.preview_busy = true;
        e.preview_running_epoch = Some(7);
        e.tick(Some(Msg::PreviewTags(6, vec![], vec![])));
        assert!(e.preview_busy);
        assert_eq!(e.preview_running_epoch, Some(7));
    }
    #[test]
    fn scoped_discovery_upgrades_after_partial_failure_once_and_ignores_stale_done() {
        let dir = fixture();
        let mut e = quiet_engine(dir.path());
        e.app.branches.open = true;
        e.app.branches.remotes.clear();
        e.remote_generation += 1;
        let generation = e.remote_generation;
        e.remote_scoped_request = true;
        e.app.branches.loading = true;
        // Hold dispatch briefly to inspect the actual event handler's intent.
        e.repository_edit_busy = true;
        e.tick(Some(Msg::Remote(
            generation,
            remote::Event::Status("fixture HTTP 502".into()),
        )));
        e.tick(Some(Msg::Remote(generation - 1, remote::Event::Done)));
        assert!(
            e.app.branches.loading,
            "stale Done cannot end the current discovery"
        );
        assert!(e.app.branches.want_refresh.is_none());
        e.tick(Some(Msg::Remote(generation, remote::Event::Done)));
        assert!(!e.app.branches.errors.is_empty() && e.app.branches.remotes.is_empty());
        assert_eq!(e.app.branches.want_refresh, Some(false));
        e.repository_edit_busy = false;
        e.tick(None);
        assert_eq!(e.remote_generation, generation + 1);
        assert!(
            !e.remote_scoped_request,
            "the upgrade must request the full listing"
        );
        until(&mut e, |e| {
            !e.app.branches.loading && e.app.branches.want_refresh.is_none()
        });
        assert_eq!(e.remote_generation, generation + 1);
        // A failed full request remains available for explicit retry, without
        // an automatic loop while the Branches tab stays open.
        let generation = e.remote_generation;
        e.repository_edit_busy = true;
        e.app.branches.loading = true;
        e.tick(Some(Msg::Remote(
            generation,
            remote::Event::Status("fixture full-list failure".into()),
        )));
        e.tick(Some(Msg::Remote(generation, remote::Event::Done)));
        assert!(e.app.branches.want_refresh.is_none());
        e.repository_edit_busy = false;
        e.tick(None);
        assert_eq!(e.remote_generation, generation);
        assert!(!e.app.branches.loading);
    }

    #[test]
    fn repository_edit_holds_writes_and_discards_stale_shortcuts_before_refresh() {
        let dir = fixture();
        let mut e = quiet_engine(dir.path());
        let head = e.app.head_id().unwrap();
        let epoch = e.operation_epoch;
        assert!(e.begin_repository_edit());
        assert!(!e.begin_repository_edit());
        assert!(!e.request_operation(git::operations::Request::Create {
            name: "must-not-exist".into(),
            target: head.to_string()
        }));
        e.app.pending.request = Some(crate::pending::Request {
            head: Some(head),
            branch: Some("main".into()),
            new_branch: "must-not-exist".into(),
            description: "stale commit intent".into(),
            files: Vec::new(),
        });
        e.app.checkout_request = Some(crate::checkout::Request::Commit {
            id: e.app.history.commits[1].id,
            remote: None,
        });
        e.app.want_refresh = true;
        for _ in 0..4 {
            e.tick(None);
        }
        assert!(!e.operation_busy && !e.app.pending.busy && !e.app.branches.busy && !e.refreshing);
        assert_eq!(e.operation_epoch, epoch);
        assert_eq!(e.app.head_id(), Some(head));
        assert!(e.app.pending.request.is_some() && e.app.checkout_request.is_some());
        e.end_repository_edit();
        assert!(e.app.pending.request.is_none() && e.app.checkout_request.is_none());
        assert!(matches!(
            e.operation_request,
            Some(git::operations::Request::Inspect)
        ));
        until(&mut e, |e| {
            e.operation_epoch != epoch
                && !e.operation_busy
                && !e.refreshing
                && !e.app.want_refresh
                && !e.app.pending.loading
                && !e.app.pending.want_refresh
        });
        assert_eq!(e.app.head_id(), Some(head));
        assert_eq!(
            git::refs::head(e.repo.as_ref().unwrap())
                .unwrap()
                .branch
                .as_deref(),
            Some("main")
        );
        assert!(
            !git::refs::raw_refs(e.repo.as_ref().unwrap())
                .unwrap()
                .iter()
                .any(|r| r.name == "refs/heads/must-not-exist")
        );
    }

    #[test]
    fn reference_conversion_refreshes_cached_engine_backend_without_restart() {
        let dir = fixture();
        let mut e = quiet_engine(dir.path());
        let head = e.app.head_id();
        assert!(!e.reference_storage_reftable());
        assert!(e.begin_repository_edit());
        git::refs::migrate_reftable(dir.path()).unwrap();
        assert!(
            !e.reference_storage_reftable(),
            "old gix handle remains cached until refresh"
        );
        e.end_repository_edit();
        until(&mut e, |e| {
            e.reference_storage_reftable()
                && !e.refreshing
                && !e.app.want_refresh
                && !e.operation_busy
        });
        assert_eq!(e.app.head_id(), head);
        assert_eq!(e.app.history.len(), 2);
        assert!(e.operations.labels.is_empty());
    }

    #[test]
    fn slow_ghost_pair_does_not_block_later_single_selections() {
        use remote::files::fixture::{Api, id};
        for provider in [remote::Provider::Github, remote::Provider::Gitlab] {
            let dir = fixture();
            let mut e = quiet_engine(dir.path());
            let head = e.app.head_id().unwrap();
            e.app.select(e.app.history.index_of(&head).unwrap());
            until(&mut e, |e| {
                e.app.details.as_ref().is_some_and(|d| d.id == head)
            });
            let api = Api::start(provider);
            remote::files::fixture::install(&mut e.app, &api.remote);
            let before = git::command::checked(dir.path(), &["show-ref"]).unwrap();
            e.app.select(e.app.history.index_of(&id(2)).unwrap());
            e.app.set_details_selection(&[id(1), id(2)]);
            until(&mut e, |_| api.blocked.load(Ordering::Acquire));
            e.app.set_details_selection(&[]);
            e.app.select(e.app.history.index_of(&id(3)).unwrap());
            // A burst must remain bounded and finish the newest selection.
            for n in 0..24 {
                let target = if n % 2 == 0 { id(2) } else { id(3) };
                e.app.select(e.app.history.index_of(&target).unwrap());
                e.tick(None);
                e.presented();
            }
            let start = Instant::now();
            let deadline = start + Duration::from_millis(800);
            while Instant::now() < deadline
                && !e
                    .app
                    .details_changes
                    .as_ref()
                    .is_some_and(|c| c.loaded && c.new == id(3))
            {
                e.tick(None);
                e.presented();
                thread::sleep(Duration::from_millis(2));
            }
            let recovered = e.app.details_changes.as_ref().is_some_and(|c| {
                c.loaded && c.error.is_none() && c.new == id(3) && c.files.len() == 1
            });
            api.unblock();
            assert!(
                recovered,
                "new ghost selection waited behind an obsolete pair ({provider:?})"
            );
            e.app.select(e.app.history.index_of(&id(4)).unwrap());
            until(&mut e, |e| {
                e.app
                    .details_changes
                    .as_ref()
                    .is_some_and(|c| c.loaded && c.new == id(4))
            });
            assert!(e.app.details_changes.as_ref().unwrap().error.is_some());
            e.app.select(e.app.history.index_of(&id(2)).unwrap());
            until(&mut e, |e| {
                e.app
                    .details_changes
                    .as_ref()
                    .is_some_and(|c| c.loaded && c.new == id(2))
            });
            assert!(e.app.details_changes.as_ref().unwrap().error.is_none());
            assert!(e.repo.as_ref().unwrap().find_object(id(2)).is_err());
            assert_eq!(
                git::command::checked(dir.path(), &["show-ref"]).unwrap(),
                before
            );
        }
    }

    #[test]
    fn parked_remote_diff_and_new_ghost_details_both_finish() {
        use remote::files::fixture::{Api, id};
        let dir = fixture();
        let mut e = quiet_engine(dir.path());
        let head = e.app.head_id().unwrap();
        e.app.select(e.app.history.index_of(&head).unwrap());
        until(&mut e, |e| {
            e.app.details.as_ref().is_some_and(|d| d.id == head)
        });
        let api = Api::start(remote::Provider::Gitlab);
        remote::files::fixture::install(&mut e.app, &api.remote);
        e.app.open_parent_diff(id(2));
        e.app.park_diff();
        e.app.select(e.app.history.index_of(&id(3)).unwrap());
        until(&mut e, |e| {
            e.app.parked_diff.as_ref().is_some_and(|v| v.changes.loaded)
                && e.app
                    .details_changes
                    .as_ref()
                    .is_some_and(|c| c.loaded && c.new == id(3))
        });
        assert!(e.app.parked_diff.as_ref().unwrap().changes.error.is_none());
        assert!(e.app.details_changes.as_ref().unwrap().error.is_none());
        e.app.resume_diff();
        until(
            &mut e,
            |e| matches!(&e.app.mode,app::Mode::Diff(v) if v.diff.as_ref().is_some_and(|d|d.syntax.is_some())),
        );
        assert!(e.repo.as_ref().unwrap().find_object(id(2)).is_err());
    }

    fn remote_diff_after_local_refresh(pair: bool) -> f64 {
        let dir = fixture();
        let mut e = Engine::new(dir.path().into(), false, Instant::now());
        until(&mut e, |e| {
            e.app.history.complete && e.app.pending.known && !e.app.is_walking()
        });
        let local = e.app.history.clone();
        let head = e.app.head_id().unwrap();
        let remote = remote::Remote::parse(
            "origin",
            "https://remote-diff-fixture.invalid/team/repo",
            Some("github"),
        )
        .unwrap();
        let mut ids = Vec::new();
        for (sha, offset) in [("f".repeat(40), 1), ("a".repeat(40), 2)] {
            remote::files::seed_native_fixture(e.repo.as_ref().unwrap(), &remote, &sha);
            let id = gix::ObjectId::from_hex(sha.as_bytes()).unwrap();
            let mut details = git::commit_details(e.repo.as_ref().unwrap(), &head).unwrap();
            details.id = id;
            details.parents = vec![head];
            details.commit_time += offset;
            let mut row = e.app.history.commits[0];
            row.id = id;
            row.parents = e.app.history.push_parents(&[head]);
            row.commit_time = details.commit_time;
            e.app.history.commits.insert(0, row);
            e.app.ghosts.insert(id, details);
            e.app.ghost_sources.insert(id, remote.clone());
            ids.push(id);
        }
        e.app
            .open_selected_diff(if pair { &ids } else { &ids[1..] });
        // A local walk completes after opening the tab but before its first request.
        // The remote commit is deliberately never fetched into the object database.
        e.app.set_cached(local, true);
        let started = Instant::now();
        until(
            &mut e,
            |e| matches!(&e.app.mode, app::Mode::Diff(v) if v.diff.is_some() || v.changes.error.is_some()),
        );
        let ms = started.elapsed().as_secs_f64() * 1000.;
        let app::Mode::Diff(view) = &e.app.mode else {
            unreachable!()
        };
        assert!(view.changes.error.is_none(), "{:?}", view.changes.error);
        assert_eq!(view.changes.files[0].path, "remote-example.txt");
        assert!(view.diff.as_ref().is_some_and(|d| d.added > 0));
        assert!(e.repo.as_ref().unwrap().find_object(ids[1]).is_err());
        assert_eq!(
            git::snapshot_refs(e.repo.as_ref().unwrap())
                .unwrap()
                .head_id,
            Some(head)
        );
        ms
    }

    #[test]
    fn remote_parent_and_pair_diffs_survive_a_local_history_refresh() {
        remote_diff_after_local_refresh(false);
        remote_diff_after_local_refresh(true);
    }

    #[test]
    #[ignore = "cached remote diff worker recovery: one warm-up and 15 samples"]
    fn benchmark_remote_diff_refresh() {
        for pair in [false, true] {
            let mut samples = Vec::new();
            for sample in 0..16 {
                let ms = remote_diff_after_local_refresh(pair);
                if sample > 0 {
                    samples.push(ms);
                }
            }
            println!(
                "remote_diff_refresh_pair_{pair}_ms={}",
                serde_json::to_string(&samples).unwrap()
            );
        }
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
    fn push_refreshes_graph_labels_over_stale_remote_metadata() {
        push_graph_refresh();
    }

    /// Exercise the real push worker and result delivery with API metadata that
    /// still points at the previous commit. All Git writes stay in these fixtures.
    fn push_graph_refresh() -> (f64, f64) {
        let dir = fixture();
        let remote_dir = tempfile::tempdir().unwrap();
        let remote_path = remote_dir.path().to_str().unwrap();
        let git = |args: &[&str]| {
            String::from_utf8(crate::git::command::checked(dir.path(), args).unwrap()).unwrap()
        };
        git(&["init", "--bare", "-q", remote_path]);
        git(&["remote", "add", "origin", remote_path]);
        git(&["push", "origin", "HEAD~1:refs/heads/main"]);
        git(&["branch", "--set-upstream-to=origin/main", "main"]);
        let mut e = Engine::new(dir.path().into(), true, Instant::now());
        until(&mut e, |e| {
            e.app.history.complete
                && e.app.pending.known
                && e.auto_remote
                && !e.app.branches.loading
                && e.operations_initialized
                && !e.operation_busy
        });
        let head = e.app.head_id().unwrap();
        // Equal-time commits can occupy either row when both are ref tips.
        // Freeze the remote's actual pre-push identity rather than its row.
        let old =
            gix::ObjectId::from_hex(git(&["rev-parse", "origin/main"]).trim().as_bytes()).unwrap();
        assert_ne!(old, head);
        let remote = remote::Remote {
            name: "origin".into(),
            url: remote_path.into(),
            provider: remote::Provider::Github,
            endpoint: "http://127.0.0.1:9/branches".into(),
            project: "test/repository".into(),
        };
        e.app.branches.update(
            remote::Update {
                remote,
                branches: vec![remote::Branch {
                    name: "main".into(),
                    sha: old.to_string(),
                }],
                scoped: true,
                cached: true,
                stale: false,
            },
            e.app.refs.as_ref(),
        );
        e.app.remote_labels();
        assert!(e.app.labels[&old].iter().any(|l| l.name == "origin/main"));
        let epoch = e.operation_epoch;
        e.request_operation(git::operations::Request::PreviewPush);
        until(&mut e, |e| e.operation_epoch != epoch);
        assert!(!e.operation_error, "{}", e.operation_status);
        assert!(e.app.branches.want_refresh.is_none());
        let review = e.operations.push.clone().unwrap();
        let epoch = e.operation_epoch;
        let generation = e.remote_generation;
        let start = Instant::now();
        e.request_operation(git::operations::Request::PushReviewed(review));
        until(&mut e, |e| e.operation_epoch != epoch);
        let push_ms = start.elapsed().as_secs_f64() * 1000.;
        assert!(!e.operation_error, "{}", e.operation_status);
        // A normal local ref refresh alone leaves the old API label authoritative.
        // tick() advances twice, so discovery may already have consumed the flag.
        assert!(
            e.app.branches.want_refresh == Some(true)
                || (e.remote_generation != generation && e.preview_force)
        );
        until(&mut e, |e| {
            e.remote_generation != generation
                && !e.app.branches.loading
                && e.app.branches.want_refresh.is_none()
                && !e.refreshing
                && !e.app.want_refresh
                && !e.app.is_walking()
                && e.app
                    .labels
                    .get(&head)
                    .is_some_and(|labels| labels.iter().any(|l| l.name == "origin/main"))
        });
        let graph_ms = start.elapsed().as_secs_f64() * 1000.;
        assert!(
            !e.app
                .labels
                .get(&old)
                .is_some_and(|labels| { labels.iter().any(|l| l.name == "origin/main") })
        );
        assert_eq!(git(&["rev-parse", "origin/main"]).trim(), head.to_string());
        assert_eq!(
            String::from_utf8(
                crate::git::command::checked(remote_dir.path(), &["rev-parse", "main"]).unwrap()
            )
            .unwrap()
            .trim(),
            head.to_string()
        );
        (push_ms, graph_ms)
    }

    #[test]
    #[ignore = "15 local push-to-graph samples with one discarded warm-up"]
    fn benchmark_desktop_push_refresh() {
        for sample in -1..15 {
            let (push_ms, graph_ms) = push_graph_refresh();
            if sample >= 0 {
                println!(
                    "{}",
                    serde_json::json!({"sample":sample,"pushMs":push_ms,"pushToGraphMs":graph_ms})
                );
            }
        }
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

    #[test]
    fn conflict_commit_inspector_reads_outside_graph_and_search_without_moving_pending_selection() {
        let dir = fixture();
        let git = |args: &[&str]| {
            String::from_utf8(crate::git::command::checked(dir.path(), args).unwrap())
                .unwrap()
                .trim()
                .to_string()
        };
        let parent = gix::ObjectId::from_hex(git(&["rev-parse", "HEAD"]).as_bytes()).unwrap();
        git(&["switch", "-c", "outside"]);
        std::fs::write(dir.path().join("file.txt"), "outside inspector contents\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "Outside graph"]);
        let outside = gix::ObjectId::from_hex(git(&["rev-parse", "HEAD"]).as_bytes()).unwrap();
        git(&["switch", "main"]);
        git(&["branch", "-D", "outside"]);
        std::fs::write(dir.path().join("file.txt"), "pending work preserved\n").unwrap();
        let mut e = quiet_engine(dir.path());
        until(&mut e, |e| {
            e.app.pending.selected
                && e.app
                    .details_changes
                    .as_ref()
                    .is_some_and(|changes| changes.loaded)
        });
        assert!(e.app.history.index_of(&outside).is_none());
        e.app.search.text.value = "m: definitely-no-match-inspector".into();
        e.app.search.changed();
        until(&mut e, |e| {
            e.app
                .search
                .result
                .as_ref()
                .is_some_and(|result| result.done)
                && e.app.graph_len() == 0
        });
        // Search navigation intentionally leaves Pending changes. Select it
        // again so inspection exercises both states at the same time.
        e.app.pending.selected = true;
        e.app.sync_details_changes();
        assert!(e.app.pending.selected);
        let original_selected = e.app.selected;
        let search = e.app.search.text.value.clone();
        e.app.inspect_commit(Some((outside, Some(parent))));
        until(&mut e, |e| {
            e.app
                .details
                .as_ref()
                .is_some_and(|details| details.id == outside)
                && e.app
                    .details_changes
                    .as_ref()
                    .is_some_and(|changes| changes.loaded)
        });
        assert!(e.app.pending.selected);
        assert_eq!(e.app.selected, original_selected);
        assert_eq!(e.app.graph_len(), 0);
        assert_eq!(
            e.app.details.as_ref().unwrap().message.trim(),
            "Outside graph"
        );
        let changes = e.app.details_changes.as_ref().unwrap();
        assert_eq!(changes.old, Some(parent));
        assert_eq!(changes.new, outside);
        assert_eq!(changes.files.len(), 1);
        assert_eq!(changes.files[0].path, "file.txt");
        let changes_key = changes.key;
        let stale = git::commit_details(e.repo.as_ref().unwrap(), &parent).unwrap();
        e.tick(Some(Msg::Details(parent, Ok(stale))));
        assert_eq!(e.app.details.as_ref().unwrap().id, outside);
        e.app.open_diff();
        assert!(matches!(&e.app.mode, app::Mode::Diff(view) if view.changes.key == changes_key));
        until(
            &mut e,
            |e| matches!(&e.app.mode, app::Mode::Diff(view) if view.diff.is_some()),
        );
        if let app::Mode::Diff(view) = &e.app.mode {
            let diff = view.diff.as_ref().unwrap();
            assert_eq!(diff.source[0].as_ref(), b"second\n");
            assert_eq!(diff.source[1].as_ref(), b"outside inspector contents\n");
        }
        e.app.close_diff();
        e.app.inspect_commit(None);
        let stale = git::commit_details(e.repo.as_ref().unwrap(), &outside).unwrap();
        e.tick(Some(Msg::Details(outside, Ok(stale))));
        assert!(e.app.details.is_none());
        assert!(e.app.inspected_id().is_none());
        assert!(e.app.pending.selected);
        assert_eq!(e.app.selected, original_selected);
        assert_eq!(e.app.search.text.value, search);
        assert_eq!(e.app.graph_len(), 0);
        assert!(matches!(e.app.mode, app::Mode::Graph));
        e.app.clear_search();
        until(&mut e, |e| !e.app.search.active());
        until(&mut e, |e| {
            e.app
                .details_changes
                .as_ref()
                .is_some_and(|changes| changes.loaded && changes.checked.is_some())
        });
        assert_eq!(
            std::fs::read(dir.path().join("file.txt")).unwrap(),
            b"pending work preserved\n"
        );
        assert_eq!(e.app.history.len(), 2);
        assert!(e.app.pending.selected);
    }

    #[test]
    fn conflict_monitor_rejects_stale_epoch_and_operation_identity_replies() {
        use git::operations::{Conflict, ConflictPreview, conflicts::Refresh};
        let dir = fixture();
        let mut e = quiet_engine(dir.path());
        e.repository_edit_busy = true;
        e.conflict_refresh_at = Instant::now();
        e.operations.conflict = Some(Conflict {
            kind: "Merge".into(),
            identity: "current-operation".into(),
            files: vec!["file.txt".into()],
            ..Default::default()
        });
        e.operations.preview = Some(ConflictPreview {
            path: "file.txt".into(),
            identity: "current-operation".into(),
            ours: "local".into(),
            theirs: "incoming".into(),
            original: Some("draft baseline".into()),
        });
        let epoch = e.operation_epoch;
        e.tick(Some(Msg::ConflictRefresh(
            epoch.wrapping_sub(1),
            "current-operation".into(),
            Ok((Refresh::Changed(None), None)),
        )));
        assert_eq!(e.operation_epoch, epoch);
        assert!(e.operations.conflict.is_some());
        assert_eq!(
            e.operations.preview.as_ref().unwrap().original.as_deref(),
            Some("draft baseline")
        );
        assert!(e.operation_request.is_none());
        e.tick(Some(Msg::ConflictRefresh(
            epoch,
            "previous-operation".into(),
            Ok((Refresh::Changed(None), None)),
        )));
        assert_eq!(e.operation_epoch, epoch);
        assert_eq!(
            e.operations.conflict.as_ref().unwrap().identity,
            "current-operation"
        );
        assert!(e.operations.preview.is_some());
        assert!(e.operation_request.is_none());
    }

    #[test]
    fn external_conflict_abort_automatically_reconciles_the_journal_once() {
        use git::operations::{Request, conflicts::Refresh};
        let dir = fixture();
        let git = |args: &[&str]| crate::git::command::checked(dir.path(), args).unwrap();
        git(&["switch", "-c", "topic"]);
        std::fs::write(dir.path().join("file.txt"), "incoming conflict\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "Incoming conflict"]);
        git(&["switch", "main"]);
        std::fs::write(dir.path().join("file.txt"), "local conflict\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "Local conflict"]);
        let mut e = quiet_engine(dir.path());
        assert!(e.request_operation(Request::Merge("topic".into())));
        until(&mut e, |e| {
            e.operations.conflict.is_some()
                && !e.operation_busy
                && !e.app.pending.loading
                && !e.app.pending.want_refresh
                && !e.refreshing
                && !e.app.want_refresh
                && !e.app.is_walking()
                && !e.conflict_refresh_busy
        });
        let identity = e.operations.conflict.as_ref().unwrap().identity.clone();
        let epoch = e.operation_epoch;
        e.timing.enabled = true;
        e.timing.marks.clear();
        git(&["merge", "--abort"]);
        e.conflict_refresh_at = Instant::now() - Duration::from_secs(1);
        until(&mut e, |e| {
            e.operations.conflict.is_none()
                && !e.operation_busy
                && e.operation_request.is_none()
                && !e.conflict_refresh_busy
                && e.operation_epoch > epoch
        });
        let count = |e: &Engine| {
            e.timing
                .marks
                .iter()
                .filter(|(label, _)| label == "git operation requested")
                .count()
        };
        assert_eq!(
            count(&e),
            1,
            "external end must enqueue one recovery inspection"
        );
        let finished_epoch = e.operation_epoch;
        e.tick(Some(Msg::ConflictRefresh(
            epoch,
            identity,
            Ok((Refresh::Changed(None), None)),
        )));
        assert_eq!(e.operation_epoch, finished_epoch);
        assert_eq!(count(&e), 1);
        assert!(e.operation_request.is_none());
        let journal: serde_json::Value = serde_json::from_slice(
            &std::fs::read(dir.path().join(".git/rustle-operations/journal.json")).unwrap(),
        )
        .unwrap();
        assert!(journal["pending"].is_null());
        e.timing.enabled = false;
    }
    #[test]
    fn stash_list_survives_unrelated_recovery_view_and_failed_operations() {
        let dir = fixture();
        std::fs::write(dir.path().join("file.txt"), "saved\n").unwrap();
        crate::git::command::checked(dir.path(), &["stash", "push", "-m", "named stash"]).unwrap();
        let mut e = quiet_engine(dir.path());
        assert!(e.request_operation(git::operations::Request::ListStashes));
        let epoch = e.stash_epoch;
        until(&mut e, |e| e.stash_epoch != epoch);
        let entries = e.operations.stashes.clone().unwrap();
        assert_eq!(entries.len(), 1);
        let epoch = e.stash_epoch;
        e.tick(Some(Msg::OperationsView(Box::default())));
        assert_eq!(
            e.stash_epoch, epoch,
            "recovery metadata is not a stash response"
        );
        assert_eq!(e.operations.stashes.as_ref(), Some(&entries));
        assert!(e.request_operation(git::operations::Request::CreateStash(None)));
        until(&mut e, |e| {
            !e.operation_busy && e.operation_request.is_none()
        });
        assert!(e.operation_error, "empty stash creation must fail visibly");
        assert_eq!(e.stash_epoch, epoch);
        assert_eq!(e.operations.stashes.as_ref(), Some(&entries));
    }
    #[test]
    fn unchanged_unknown_stash_metadata_does_not_repeat_invalidation() {
        let dir = fixture();
        let mut e = quiet_engine(dir.path());
        let mut snapshot = e.app.pending.snapshot.clone();
        snapshot.stash_stamp = None;
        e.tick(Some(Msg::Pending(
            e.pending_generation,
            Ok(snapshot.clone()),
        )));
        let revision = e.stash_revision;
        e.tick(Some(Msg::Pending(
            e.pending_generation,
            Ok(snapshot.clone()),
        )));
        assert_eq!(e.stash_revision, revision);
        snapshot.stash_stamp = Some(7);
        e.tick(Some(Msg::Pending(
            e.pending_generation,
            Ok(snapshot.clone()),
        )));
        assert_eq!(e.stash_revision, revision + 1);
        e.tick(Some(Msg::Pending(e.pending_generation, Ok(snapshot))));
        assert_eq!(e.stash_revision, revision + 1);
    }
}
