//! git-rustle: a Windows-Terminal-native Git graph viewer.
//!
//! Startup runs three things in parallel: reading the layout cache, opening the
//! repository (refs snapshot), and setting up the console. The first frame is
//! drawn from the cache as soon as it is available; a background walk refreshes
//! the graph when the refs changed.

mod app;
mod cache;
mod git;
mod graph;
mod model;
mod term;
mod ui;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use app::{App, LoadState};
use model::History;
use term::{Console, Event, Screen};

enum Msg {
    Input(Event),
    Cache(Option<cache::Cached>),
    Repo(Box<git::Result<(gix::Repository, git::RefSnapshot)>>),
    Batch(u32, History),
    WalkDone(u32, git::Result<()>),
    CacheWritten(bool),
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
                    "git-rustle {}\n\nUsage: git-rustle [OPTIONS] [PATH]\n\n  PATH          repository or any directory inside one (default: .)\n  --no-cache    ignore and do not write the layout cache\n  -h, --help    show this help\n  -V, --version show the version\n\nEnvironment:\n  GIT_RUSTLE_TIMING=1        print startup timings to stderr on exit\n  GIT_RUSTLE_AUTOQUIT=<ms>   quit automatically after the first frame (benchmarking)\n  GIT_RUSTLE_DUMP=<file>     write the last frame as plain text on exit (debugging)",
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
    let mut cache_fp: Option<u64> = None;
    let mut cache_seen = false;
    let mut repo_seen = false;
    let mut walker: Option<Walker> = None;
    // Walkers that finished walking but may still be writing the cache.
    let mut finishing: Vec<Walker> = Vec::new();
    let mut next_walk_id: u32 = 1;
    let mut first_frame = false;
    let mut areas = ui::compute_areas(screen.width(), screen.height(), app.right_width);
    let mut last_tick = Instant::now();
    // Benchmark aid: GIT_RUSTLE_AUTOQUIT=<ms> quits after the first frame plus that delay.
    let autoquit: Option<Duration> = std::env::var("GIT_RUSTLE_AUTOQUIT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis);
    let mut first_frame_at: Option<Instant> = None;

    loop {
        if let (Some(delay), Some(at)) = (autoquit, first_frame_at)
            && at.elapsed() >= delay
            && walker.is_none()
        {
            app.quit = true;
        }
        if app.quit {
            if let Some(wk) = walker.take() {
                wk.cancel();
                wk.join();
            }
            for wk in finishing.drain(..) {
                wk.join();
            }
            return Ok(());
        }
        // Fetch details for the selected commit if they are stale.
        if let (Some(r), Some(id)) = (repo.as_ref(), app.selected_id())
            && app.details.as_ref().map(|d| d.id) != Some(id)
        {
            app.details = git::commit_details(r, &id).ok();
            app.needs_redraw = true;
        }

        if app.needs_redraw {
            areas = ui::compute_areas(screen.width(), screen.height(), app.right_width);
            let now = ui::time::now_unix();
            ui::draw(app, screen, &areas, now);
            screen.flush(console);
            app.needs_redraw = false;
            if !first_frame && !app.history.is_empty() {
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
            walker = Some(spawn_walk(id, r.clone(), refs.tips.clone(), target, tx.clone()));
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
                Msg::Input(Event::Key(k)) => app.handle_key(k),
                Msg::Input(Event::Mouse(m)) => app.handle_mouse(m, &areas, screen.width()),
                Msg::Input(Event::Resize) => {
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
                    timing.mark(if ok { "cache written" } else { "cache write failed" });
                    // Reap finished threads.
                    finishing.retain(|w| w.handle.as_ref().is_some_and(|h| !h.is_finished()));
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
    let Some(refs) = app.refs.as_ref() else { return };
    let fresh = cache_fp == Some(refs.fingerprint) && app.history.complete;
    if fresh {
        app.load = LoadState::Cached;
        app.needs_redraw = true;
    } else {
        app.want_rewalk = true;
    }
}
