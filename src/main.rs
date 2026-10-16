//! git-rustle: a Windows-Terminal-native Git graph viewer.
//!
//! Startup runs three things in parallel: reading the layout cache, opening the
//! repository (refs snapshot), and setting up the console. The first frame is
//! drawn from the cache as soon as it is available; a background walk refreshes
//! the graph when the refs changed.

mod app;
mod branches;
mod cache;
mod checkout;
mod credentials;
mod diff;
mod diffview;
mod git;
mod graph;
mod model;
mod remote;
mod settings;
mod term;
mod ui;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use app::{App, LoadState};
use diffview::DiffReq;
use model::History;
use term::{Console, Event, Screen};

enum Msg {
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
    Input(Event),
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
fn spawn_diff_worker(repo: gix::Repository, tx: Sender<Msg>, no_cache: bool) -> Sender<DiffReq> {
    let (req_tx, req_rx) = mpsc::channel::<DiffReq>();
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
                    if tx.send(Msg::TreeDiff(key, result)).is_err() {
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
                    if tx.send(Msg::FileDiff(view, seq, file, result)).is_err() {
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
) -> Sender<StatsReq> {
    let (req_tx, req_rx) = mpsc::channel::<StatsReq>();
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
                            .send(Msg::Stats(req.key, std::mem::take(&mut chunk)))
                            .is_err()
                        {
                            return;
                        }
                        last_send = Instant::now();
                    }
                }
                if !chunk.is_empty() && tx.send(Msg::Stats(req.key, chunk)).is_err() {
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
            self.marks.push((what.into(), self.start.elapsed()));
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

/// Debug aid: `GIT_RUSTLE_KEYS` is a script of key presses replayed after the
/// first complete frame, e.g. `" <down>d<tab><pgdn>"`. Plain characters are
/// typed as-is; named keys go in angle brackets (`<down>`, `<esc>`, `<c-c>`).
fn parse_key_script(script: &str) -> Vec<term::KeyEvent> {
    use term::KeyCode::*;
    let plain = |code| term::KeyEvent {
        code,
        ctrl: false,
        shift: false,
        alt: false,
    };
    let mut out = Vec::new();
    let mut chars = script.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '<' {
            out.push(plain(Char(c)));
            continue;
        }
        let name: String = chars.by_ref().take_while(|&c| c != '>').collect();
        let (ctrl, name) = match name.strip_prefix("c-") {
            Some(rest) => (true, rest),
            None => (false, name.as_str()),
        };
        let code = match name {
            "f1" => F(1),
            "f2" => F(2),
            "f5" => F(5),
            "up" => Up,
            "down" => Down,
            "left" => Left,
            "right" => Right,
            "pgup" => PageUp,
            "pgdn" => PageDown,
            "home" => Home,
            "end" => End,
            "enter" => Enter,
            "esc" => Esc,
            "tab" => Tab,
            "space" => Char(' '),
            "del" => Delete,
            "bs" => Backspace,
            other => match other.chars().next() {
                Some(ch) if other.chars().count() == 1 => Char(ch),
                _ => continue,
            },
        };
        let mut ev = plain(code);
        ev.ctrl = ctrl;
        out.push(ev);
    }
    out
}

struct Options {
    path: PathBuf,
    no_cache: bool,
}

fn parse_args() -> Result<Options, String> {
    let mut opts = Options {
        path: PathBuf::from("."),
        no_cache: false,
    };
    for arg in std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("-h") | Some("--help") => {
                println!(
                    "git-rustle {}\n\nUsage: git-rustle [OPTIONS] [PATH]\n\n  PATH          repository or any directory inside one (default: .)\n  --no-cache    ignore and do not write disk caches\n  -h, --help    show this help\n  -V, --version show the version\n\nEnvironment:\n  GIT_RUSTLE_TIMING=1        print startup timings to stderr on exit\n  GIT_RUSTLE_AUTOQUIT=<ms>   quit automatically after the first frame (benchmarking)\n  GIT_RUSTLE_DUMP=<file>     write the last frame as plain text on exit (debugging)\n  GIT_RUSTLE_KEYS=<script>   replay key presses after the first frame, e.g. \"<down>d\" (debugging)",
                    env!("CARGO_PKG_VERSION")
                );
                std::process::exit(0);
            }
            Some("-V") | Some("--version") => {
                println!("git-rustle {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            Some("--no-cache") => opts.no_cache = true,
            Some(s) if s.starts_with('-') => return Err(format!("unknown option: {s}")),
            _ => opts.path = PathBuf::from(arg),
        }
    }
    Ok(opts)
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
                tx.send(Msg::Batch(id, chunk)).is_ok()
            });
            let cancelled = cancel2.load(Ordering::Relaxed);
            let ok = result.is_ok() && !cancelled;
            let _ = tx.send(Msg::WalkDone(id, result));
            if ok && let (Some(full), Some((path, git_dir, fp))) = (full, cache_target) {
                let written = cache::write(&path, &git_dir, fp, &full).is_ok();
                let _ = tx.send(Msg::CacheWritten(written));
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
        let _ = tx.send(Msg::Refreshed(Box::new(result)));
    });
}

fn main() {
    let start = Instant::now();
    let opts = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("git-rustle: {e}");
            std::process::exit(2);
        }
    };
    let mut timing = Timing::new(start);

    let (tx, rx) = mpsc::channel::<Msg>();

    // 1. Cache read, in parallel with everything else.
    let git_dir_guess = find_git_dir(&opts.path);
    let cache_path = if opts.no_cache {
        None
    } else {
        git_dir_guess.as_deref().and_then(cache::cache_path)
    };
    {
        let tx = tx.clone();
        let path = cache_path.clone();
        thread::Builder::new()
            .name("cache-read".into())
            .spawn(move || {
                let cached = path.and_then(|p| cache::read(&p).ok());
                let _ = tx.send(Msg::Cache(cached));
            })
            .expect("spawn cache thread");
    }

    // 2. Repository open + refs snapshot.
    {
        let tx = tx.clone();
        let path = opts.path.clone();
        thread::Builder::new()
            .name("repo-open".into())
            .spawn(move || {
                let result = git::open(&path).and_then(|repo| {
                    let snap = git::snapshot_refs(&repo)?;
                    Ok((repo, snap))
                });
                let _ = tx.send(Msg::Repo(Box::new(result)));
            })
            .expect("spawn repo thread");
    }

    // 3. Console.
    let console = match Console::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("git-rustle: {e} (run inside Windows Terminal or a console window)");
            std::process::exit(1);
        }
    };
    timing.mark("console ready");
    let (w, h) = console.size();
    let mut screen = Screen::new(w, h);
    {
        let tx = tx.clone();
        term::input::spawn_reader(console.stdin, move |ev| tx.send(Msg::Input(ev)).is_ok());
    }

    let mut app = App::new();
    app.right_width = ui::default_right_width(w);
    app.sidebar_width = ui::diff::default_sidebar_width(w);
    app.repo_name = opts
        .path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| s != ".")
        .unwrap_or_else(|| {
            std::env::current_dir()
                .ok()
                .and_then(|d| d.file_name().map(|s| s.to_string_lossy().into_owned()))
                .unwrap_or_default()
        });

    let exit = run(
        &mut app,
        &console,
        &mut screen,
        &rx,
        &tx,
        &mut timing,
        cache_path,
        opts.no_cache,
    );
    drop(console);
    timing.report();
    if let Some(path) = std::env::var_os("GIT_RUSTLE_DUMP") {
        let _ = std::fs::write(path, screen.last_frame_text());
    }
    if let Err(msg) = exit {
        eprintln!("git-rustle: {msg}");
        std::process::exit(1);
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    app: &mut App,
    console: &Console,
    screen: &mut Screen,
    rx: &Receiver<Msg>,
    tx: &Sender<Msg>,
    timing: &mut Timing,
    cache_path: Option<PathBuf>,
    no_cache: bool,
) -> Result<(), String> {
    let mut repo: Option<gix::Repository> = None;
    let mut diff_tx: Option<Sender<DiffReq>> = None;
    let mut remote_diff_tx: Option<Sender<DiffReq>> = None;
    let mut stats_tx: Option<Sender<StatsReq>> = None;
    let stats_latest = Arc::new(AtomicU32::new(0));
    let mut stats_sent_for: Option<u32> = None;
    let mut cache_fp: Option<u64> = None;
    let mut cache_seen = false;
    let mut repo_seen = false;
    let mut refreshing = false;
    let mut walker: Option<Walker> = None;
    // Walkers that finished walking but may still be writing the cache.
    let mut finishing: Vec<Walker> = Vec::new();
    let mut next_walk_id: u32 = 1;
    let mut first_frame = false;
    let mut branch_frame = false;
    let mut settings_frame = false;
    let mut checkout_frame = false;
    let mut remote_generation = 0u32;
    let mut preview_epoch = 0u32;
    let mut preview_cancel = Arc::new(AtomicBool::new(false));
    let mut preview_busy = false;
    let mut preview_wanted = false;
    let mut preview_force = false;
    let mut activity_busy = false;
    let mut local_activity_done = None;
    let mut auto_remote = false;
    let mut areas = ui::compute_areas(screen.width(), screen.height(), app.right_width);
    let mut last_tick = Instant::now();
    // Benchmark aid: GIT_RUSTLE_AUTOQUIT=<ms> quits after the first frame plus that delay.
    let autoquit: Option<Duration> = std::env::var("GIT_RUSTLE_AUTOQUIT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis);
    let mut first_frame_at: Option<Instant> = None;
    let mut key_script: Option<Vec<term::KeyEvent>> = std::env::var("GIT_RUSTLE_KEYS")
        .ok()
        .map(|s| parse_key_script(&s));

    loop {
        if let (Some(delay), Some(at)) = (autoquit, first_frame_at)
            && at.elapsed() >= delay
            && walker.is_none()
            && !app.branches.busy
            && !app.settings.saving
        {
            app.quit = true;
        }
        // Replay scripted keys once the graph is complete, spaced out so each
        // one is handled (and its async work finished) before the next.
        if let Some(keys) = key_script.take_if(|_| first_frame_at.is_some() && walker.is_none()) {
            let tx = tx.clone();
            thread::spawn(move || {
                for k in keys {
                    thread::sleep(Duration::from_millis(40));
                    if tx.send(Msg::Input(Event::Key(k))).is_err() {
                        break;
                    }
                }
            });
        }
        if app.quit {
            preview_cancel.store(true, Ordering::Relaxed);
            if let Some(wk) = walker.take() {
                wk.cancel();
                wk.join();
            }
            for wk in finishing.drain(..) {
                wk.join();
            }
            return Ok(());
        }
        // Start network work only after the local graph has rendered and finished loading.
        if first_frame && !app.is_walking() && repo.is_some() && !auto_remote {
            auto_remote = true;
            app.branches.want_refresh.get_or_insert(false);
        }
        if std::mem::take(&mut app.branches.preview_requested) {
            preview_epoch = preview_epoch.wrapping_add(1);
            preview_cancel.store(true, Ordering::Relaxed);
            preview_wanted = true;
        }
        if preview_wanted
            && !preview_busy
            && !app.is_walking()
            && !app.want_rewalk
            && !app.settings.saving
            && !app.branches.busy
            && let Some(r) = repo.as_ref()
            && let Some(refs) = app.refs.clone()
        {
            preview_wanted = false;
            {
                preview_busy = true;
                let local = app
                    .local_history
                    .get_or_insert_with(|| Arc::new(app.history.clone()))
                    .clone();
                let updates = app.branches.remotes.values().cloned().collect::<Vec<_>>();
                let requested = app.branches.preview_targets.clone();
                let r = r.clone();
                let tx = tx.clone();
                let epoch = preview_epoch;
                let force = std::mem::take(&mut preview_force);
                preview_cancel = Arc::new(AtomicBool::new(false));
                let cancel = preview_cancel.clone();
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
                            let _ = tx.send(Msg::Preview(epoch, preview));
                        }),
                    );
                    let _ = tx.send(Msg::PreviewTags(epoch, preview.tags, preview.errors));
                });
            }
        }
        if !app.branches.busy
            && let Some(r) = repo.as_ref()
            && let Some(request) = app.settings.request.take()
        {
            let path = r.workdir().unwrap_or_else(|| r.git_dir()).to_path_buf();
            let session = app.settings.session;
            let tx = tx.clone();
            timing.mark("settings requested");
            thread::spawn(move || {
                let _ = tx.send(Msg::Settings(session, settings::execute(&path, request)));
            });
        }
        if !app.settings.saving
            && (app.want_refresh
                || app.branches.want_refresh.is_some()
                || app.checkout_request.is_some())
            && let Some(r) = repo.as_ref()
        {
            let path = r.workdir().unwrap_or_else(|| r.git_dir()).to_path_buf();
            if app.want_refresh && !refreshing && !app.branches.busy {
                app.want_refresh = false;
                refreshing = true;
                refresh_repo(path.clone(), tx.clone());
            }
            if !app.branches.loading
                && !app.branches.busy
                && let Some(force) = app.branches.want_refresh.take()
            {
                preview_epoch = preview_epoch.wrapping_add(1);
                preview_cancel.store(true, Ordering::Relaxed);
                preview_wanted = app.branches.pending_jump.is_some();
                remote_generation = remote_generation.wrapping_add(1);
                let generation = remote_generation;
                preview_force |= force;
                if force {
                    local_activity_done = None;
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
                        let _ = tx.send(Msg::Remote(generation, event));
                    })
                });
                if force {
                    app.want_refresh = true;
                }
            }
            if !app.branches.busy
                && let Some(target) = app.checkout_request.take()
            {
                app.branches.busy = true;
                app.branches.status = format!("Checking out {}…", target.label());
                app.checkout_notice = Some(Ok(app.branches.status.clone()));
                app.needs_redraw = true;
                let tx = tx.clone();
                timing.mark("checkout requested");
                thread::spawn(move || {
                    let result = git::checkout::execute(&path, &target);
                    let _ = tx.send(Msg::Checkout(result, target.is_remote()));
                });
            }
        }
        // Browser metadata is lazy; neither the initial frame nor redraws walk history.
        if app.branches.open
            && !activity_busy
            && !app.is_walking()
            && !app.want_rewalk
            && let Some(r) = repo.as_ref()
            && let Some(refs) = app.refs.as_ref()
        {
            let generation = remote_generation;
            if local_activity_done != Some(refs.fingerprint) {
                local_activity_done = Some(refs.fingerprint);
                activity_busy = true;
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
                    let _ = tx.send(Msg::BranchActivity(generation, values, None));
                });
            } else if let Some((target, remote)) = app.branches.next_remote_activity() {
                activity_busy = true;
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
                    let _ = tx.send(Msg::BranchActivity(generation, values, error));
                });
            }
        }
        // Fetch details for the selected commit if they are stale.
        if let (Some(r), Some(id)) = (repo.as_ref(), app.selected_id())
            && app.details.as_ref().map(|d| d.id) != Some(id)
        {
            app.details = app
                .ghosts
                .get(&id)
                .cloned()
                .or_else(|| git::commit_details(r, &id).ok());
            app.needs_redraw = true;
        }
        // Hand new diff work to the workers.
        if let Some(r) = repo.as_ref() {
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
                    }
                    let remote_request = match &req {
                        DiffReq::Tree { remote, .. } => remote.is_some(),
                        DiffReq::File { change, .. } => change.remote.is_some(),
                    };
                    let worker = if remote_request {
                        &mut remote_diff_tx
                    } else {
                        &mut diff_tx
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
                && stats_sent_for != Some(req.key)
            {
                stats_sent_for = Some(req.key);
                stats_latest.store(req.key, Ordering::Relaxed);
                let stx = stats_tx.get_or_insert_with(|| {
                    spawn_stats_worker(r.clone(), tx.clone(), Arc::clone(&stats_latest))
                });
                let _ = stx.send(req);
            }
        }

        if app.needs_redraw {
            areas = ui::compute_areas(screen.width(), screen.height(), app.right_width);
            let now = ui::time::now_unix();
            ui::draw(app, screen, &areas, now);
            screen.flush(console);
            if app.checkout_confirmation.is_some() && !checkout_frame {
                timing.mark("checkout confirmation frame");
            }
            checkout_frame = app.checkout_confirmation.is_some();
            if app.branches.open && !branch_frame {
                timing.mark(format!(
                    "branch browser frame ({} branches)",
                    app.branches.entries.len()
                ));
            }
            branch_frame = app.branches.open;
            if app.settings.open && !settings_frame {
                timing.mark("settings popup frame");
            }
            settings_frame = app.settings.open;
            app.needs_redraw = false;
            if !first_frame
                && (!app.history.is_empty()
                    || matches!(app.load, LoadState::Cached | LoadState::Complete))
            {
                first_frame = true;
                first_frame_at = Some(Instant::now());
                timing.mark(format!("first frame ({} rows)", app.history.len()));
            }
        }

        // Start a walk when asked (or when the cache turned out stale).
        if app.want_rewalk
            && let Some(r) = repo.as_ref()
            && let Some(refs) = app.refs.as_ref()
        {
            app.want_rewalk = false;
            if let Some(wk) = walker.take() {
                wk.cancel();
                finishing.push(wk);
            }
            let id = next_walk_id;
            next_walk_id += 1;
            let target = if no_cache {
                None
            } else {
                cache_path
                    .clone()
                    .map(|p| (p, git::git_dir(r), refs.fingerprint))
            };
            walker = Some(spawn_walk(
                id,
                r.clone(),
                refs.tips.clone(),
                target,
                tx.clone(),
            ));
            app.start_walk();
            timing.mark("walk started");
        }

        let msg = match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(m) => Some(m),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        };
        let mut pending = Vec::new();
        if let Some(m) = msg {
            pending.push(m);
        }
        // Coalesce whatever else is already queued so we draw once.
        while let Ok(m) = rx.try_recv() {
            pending.push(m);
            if pending.len() > 256 {
                break;
            }
        }

        for msg in pending {
            match msg {
                Msg::BranchActivity(generation, values, error) => {
                    activity_busy = false;
                    if generation == remote_generation {
                        timing.mark(format!("branch activity ready ({} tips)", values.len()));
                        app.branches.install_activity(values, app.refs.as_ref());
                        if let Some((target, error)) = error {
                            app.branches.activity_errors.insert(target, error);
                        }
                        app.needs_redraw = true;
                    } else {
                        local_activity_done = None;
                    }
                }
                Msg::Preview(epoch, mut preview) => {
                    if epoch == preview_epoch
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
                    preview_busy = false;
                    if epoch == preview_epoch
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
                        preview_epoch = preview_epoch.wrapping_add(1);
                        preview_cancel.store(true, Ordering::Relaxed);
                        preview_wanted = false;
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
                        remote_generation = remote_generation.wrapping_add(1);
                        app.branches.loading = false;
                        app.branches.remotes.clear();
                        app.branches.preview_targets.clear();
                        app.branches.pending_jump = None;
                        app.branches.activity_cache.clear();
                        app.branches.activity_attempted.clear();
                        app.branches.activity_errors.clear();
                        app.branches.activity_requests = 0;
                        local_activity_done = None;
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
                    if generation != remote_generation {
                        continue;
                    }
                    match event {
                        remote::Event::Configured(remotes) => {
                            if remotes.is_empty() && app.local_history.is_some() {
                                preview_wanted = true;
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
                            preview_wanted |= preview_force
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
                    refreshing = false;
                    match *result {
                        Ok((r, snap)) => {
                            let changed = app
                                .refs
                                .as_ref()
                                .is_none_or(|old| old.fingerprint != snap.fingerprint);
                            app.set_refs(snap);
                            repo = Some(r);
                            if changed {
                                preview_epoch = preview_epoch.wrapping_add(1);
                                preview_cancel.store(true, Ordering::Relaxed);
                                preview_wanted = true;
                                app.want_rewalk = true;
                            }
                        }
                        Err(error) => app.set_error(error.to_string()),
                    }
                }
                Msg::Input(Event::Key(k)) => {
                    if k.code == term::KeyCode::Enter {
                        timing.mark("enter input");
                    }
                    app.handle_key(k)
                }
                Msg::Input(Event::Mouse(m)) => {
                    app.handle_mouse(m, &areas, screen.width(), screen.height())
                }
                Msg::Input(Event::Resize) => {
                    if let Some(dialog) = &mut app.checkout_confirmation {
                        dialog.ready = false;
                    }
                    let (w, h) = console.size();
                    screen.resize(w, h);
                    app.needs_redraw = true;
                }
                Msg::Cache(cached) => {
                    cache_seen = true;
                    if let Some(c) = cached {
                        timing.mark(format!("cache read ({} rows)", c.history.len()));
                        cache_fp = Some(c.fingerprint);
                        let complete = c.history.complete;
                        app.set_cached(c.history, true);
                        if !complete {
                            app.want_rewalk = true;
                        }
                    } else {
                        timing.mark("no cache");
                    }
                    if repo_seen {
                        decide(app, cache_fp);
                    }
                }
                Msg::Repo(result) => {
                    repo_seen = true;
                    match *result {
                        Ok((r, snap)) => {
                            timing.mark(format!("repo open ({} refs)", snap.refs.len()));
                            app.repo_name = git::display_name(&r);
                            app.set_refs(snap);
                            repo = Some(r);
                            if cache_seen {
                                decide(app, cache_fp);
                            }
                        }
                        Err(e) => {
                            if app.history.is_empty() {
                                return Err(e.to_string());
                            }
                            app.set_error(e.to_string());
                        }
                    }
                }
                Msg::Batch(id, batch) => {
                    if walker.as_ref().is_some_and(|w| w.id == id) {
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
                    if walker.as_ref().is_some_and(|w| w.id == id) {
                        finishing.extend(walker.take());
                        first_frame_at.get_or_insert_with(Instant::now);
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
                    finishing.retain(|w| w.handle.as_ref().is_some_and(|h| !h.is_finished()));
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

        // Periodic tick: pick up resizes the host did not report, refresh ages.
        if last_tick.elapsed() >= Duration::from_millis(250) {
            last_tick = Instant::now();
            let (w, h) = console.size();
            if (w, h) != (screen.width(), screen.height()) {
                screen.resize(w, h);
                app.needs_redraw = true;
            }
        }
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
