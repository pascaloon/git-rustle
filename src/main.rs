//! Git Rustle: a Windows-native GPU-accelerated Git client.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#[cfg(not(windows))]
compile_error!("Git Rustle desktop currently supports Windows only.");
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
mod pending;
mod remote;
mod search;
mod settings;
mod term;
mod ui;

mod desktop;
mod desktop_details;
mod desktop_icons;
mod desktop_scroll;
mod desktop_selection;
mod desktop_shell;
mod desktop_text;
mod desktop_theme;
mod desktop_wrap;
mod engine;
mod selectable;
use std::path::PathBuf;
use std::time::Instant;
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
                    "git-rustle {}\n\nUsage: git-rustle [OPTIONS] [PATH]\n\n  PATH          repository or any directory inside one (default: .)\n  --no-cache    ignore and do not write disk caches\n  -h, --help    show this help\n  -V, --version show the version\n\nEnvironment:\n  GIT_RUSTLE_TIMING=1        print startup timings to stderr on exit\n  GIT_RUSTLE_AUTOQUIT=<ms>   quit automatically after the first frame (benchmarking)\n  GIT_RUSTLE_DUMP=<file>     write the final model summary on exit (debugging)\n  GIT_RUSTLE_SCENARIO=<name> scroll, branches, diff, search, settings (benchmarking)",
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

fn main() {
    let start = Instant::now();
    let opts = match parse_args() {
        Ok(opts) => opts,
        Err(error) => {
            eprintln!("git-rustle: {error}");
            std::process::exit(2);
        }
    };
    desktop::run(engine::Engine::new(opts.path, opts.no_cache, start));
}
