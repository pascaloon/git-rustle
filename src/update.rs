//! GitHub release checks and verified executable replacement. Never runs on the UI thread.
use self_update::backends::github::{Update, UpdateBuilder};
use self_update::{Checksum, Release};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const ASSET: &str = "git-rustle-x86_64-pc-windows-msvc.exe";
pub const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const API: &str = "https://api.github.com";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Available {
    pub version: String,
    pub checksum: String,
}

fn updater(api: &str, current: &str) -> UpdateBuilder {
    let mut builder = Update::configure();
    builder
        .repo_owner("pascaloon")
        .repo_name("git-rustle")
        .api_base_url(api)
        .bin_name("git-rustle.exe")
        .current_version(current)
        .target("x86_64-pc-windows-msvc")
        .asset_matcher(|assets| assets.iter().find(|asset| asset.name() == ASSET).cloned())
        .timeout(Duration::from_secs(20))
        .retries(0)
        .unattended();
    builder
}

fn available(release: &Release, current: &str) -> Result<Option<Available>, String> {
    if !self_update::version::bump_is_greater(current, release.version())
        .map_err(|e| e.to_string())?
        || release.version().contains('-')
    {
        return Ok(None);
    }
    let Some(asset) = release.assets().iter().find(|asset| asset.name() == ASSET) else {
        return Ok(None);
    };
    let checksum = asset
        .digest()
        .and_then(|s| s.strip_prefix("sha256:"))
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("The release executable has no valid SHA-256 digest")?;
    Ok(Some(Available {
        version: release.version().to_string(),
        checksum: checksum.to_string(),
    }))
}

fn check_at(api: &str, current: &str) -> Result<Option<Available>, String> {
    let releases = updater(api, current)
        .build()
        .map_err(|e| e.to_string())?
        .get_latest_release()
        .map_err(|e| e.to_string())?;
    releases
        .latest()
        .map_or(Ok(None), |release| available(release, current))
}

#[derive(Deserialize, Serialize)]
struct LastCheck {
    checked: u64,
    available: Option<Available>,
}

pub struct Checked {
    pub available: Option<Available>,
    pub next_check_in: Duration,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn directory() -> Result<PathBuf, String> {
    std::env::var_os("LOCALAPPDATA")
        .map(|p| PathBuf::from(p).join("git-rustle"))
        .ok_or_else(|| "Cannot locate the user's settings directory".into())
}

/// Every launch and scheduled check refreshes GitHub on the background executor.
/// The UI owns the six-hour timer; saved state must not suppress a startup check.
pub fn check() -> Result<Checked, String> {
    let path = directory()?.join("updates.json");
    check_and_record_at(API, VERSION, &path, now())
}

fn check_and_record_at(
    api: &str,
    current: &str,
    path: &Path,
    time: u64,
) -> Result<Checked, String> {
    let result = check_at(api, current);
    if let Ok(available) = &result {
        // Retain the last successful result for diagnostics, never to skip checks.
        // A state write failure must never hide an available update.
        let _ = fs::create_dir_all(path.parent().unwrap());
        if let Ok(bytes) = serde_json::to_vec(&LastCheck {
            checked: time,
            available: available.clone(),
        }) {
            let _ = fs::write(path, bytes);
        }
    }
    result.map(|available| Checked {
        available,
        next_check_in: CHECK_INTERVAL,
    })
}

fn install_at(api: &str, current: &str, candidate: &Available, exe: &Path) -> Result<(), String> {
    // Pin the exact offered release AND digest: a changed release cannot silently
    // substitute a different binary after the user accepted the original offer.
    let status = updater(api, current)
        .release_tag(format!("v{}", candidate.version))
        .bin_install_path(exe)
        .check_install_path_writable(true)
        .timeout(Duration::from_secs(180))
        .verify_checksum(Checksum::Sha256(candidate.checksum.clone()))
        .build()
        .map_err(|e| e.to_string())?
        .update()
        .map_err(|e| e.to_string())?;
    if !status.is_updated() {
        return Err("This version is already installed. Close and reopen Git Rustle.".into());
    }
    Ok(())
}

pub fn command(exe: &Path) -> Command {
    let mut command = Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    command
}

pub fn install(candidate: &Available, exe: &Path) -> Result<(), String> {
    let directory = directory()?;
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("update.lock"))
        .map_err(|e| e.to_string())?;
    lock.try_lock()
        .map_err(|_| "Another Git Rustle instance is updating. Try again shortly.".to_string())?;
    // Another instance may already have replaced our original path. Never downgrade it.
    let output = command(exe)
        .arg("--version")
        .output()
        .map_err(|e| e.to_string())?;
    let version = std::str::from_utf8(&output.stdout)
        .map_err(|e| e.to_string())?
        .trim()
        .strip_prefix("git-rustle ")
        .ok_or("Cannot identify the installed executable")?;
    if !output.status.success()
        || !self_update::version::bump_is_greater(version, &candidate.version)
            .map_err(|e| e.to_string())?
    {
        return Err("This update is already installed. Close and reopen Git Rustle.".into());
    }
    install_at(API, version, candidate, exe)
}

/// Only the commit form is transferred; tokens and provider settings never enter
/// the child environment. Other unsaved forms require explicit restart consent.
#[derive(Default, Serialize, Deserialize)]
pub struct CommitDraft {
    pub description: String,
    pub branch: String,
    pub checks: std::collections::HashMap<String, bool>,
}

impl CommitDraft {
    pub fn capture(app: &crate::app::App) -> Self {
        let mut checks = app.pending.checks.clone();
        let changes = match &app.mode {
            crate::app::Mode::Diff(view) => Some(&view.changes),
            crate::app::Mode::Graph => app.details_changes.as_ref(),
        };
        if let Some(changes) = changes
            && let Some(selected) = &changes.checked
        {
            checks.extend(
                changes
                    .files
                    .iter()
                    .zip(selected)
                    .map(|(file, checked)| (file.path.clone(), *checked)),
            );
        }
        Self {
            description: app.pending.description.value.clone(),
            branch: app.pending.branch.value.clone(),
            checks,
        }
    }
}

pub const RESUME: &str = "GIT_RUSTLE_UPDATE_COMMIT_DRAFT";

pub fn take_draft() -> Option<CommitDraft> {
    let text = std::env::var(RESUME).ok();
    // SAFETY: called at the very start of main, before engine/GPUI workers exist.
    // Do not let Git commands or subsequent launches inherit the transfer data.
    unsafe { std::env::remove_var(RESUME) };
    text.and_then(|text| serde_json::from_str(&text).ok())
}

pub fn restore_draft(engine: &mut crate::engine::Engine, draft: Option<CommitDraft>) {
    if let Some(draft) = draft {
        engine.app.pending.description.cursor = draft.description.len();
        engine.app.pending.description.value = draft.description;
        engine.app.pending.branch.cursor = draft.branch.len();
        engine.app.pending.branch.value = draft.branch;
        engine.app.pending.checks = draft.checks;
    }
}

#[cfg(test)]
mod tests;
