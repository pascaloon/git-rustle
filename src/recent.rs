//! Bounded user-wide MRU history. All filesystem work runs on a worker.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

pub const LIMIT: usize = 20;

pub fn file() -> io::Result<PathBuf> {
    // Also allows benchmark launches to avoid modifying the user's history.
    if let Some(path) = std::env::var_os("GIT_RUSTLE_RECENTS_FILE") {
        return Ok(path.into());
    }
    std::env::var_os("LOCALAPPDATA")
        .map(|base| PathBuf::from(base).join("git-rustle/recent-repositories.json"))
        .ok_or_else(|| io::Error::other("Cannot locate the user's settings directory"))
}

pub enum Change {
    Load,
    Open(PathBuf),
    Forget(PathBuf),
}

fn same(a: &Path, b: &Path) -> bool {
    // Canonicalization adds the extended Windows prefix; accept ordinary paths,
    // slash aliases and case aliases from an existing history file as well.
    fn key(path: &Path) -> String {
        let text = path.to_string_lossy().replace('/', r"\");
        let text = if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{unc}")
        } else {
            text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
        };
        text.to_lowercase()
    }
    a == b || key(a) == key(b)
}

pub fn update(path: &Path, change: Change) -> io::Result<Vec<PathBuf>> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Invalid history path"))?;
    fs::create_dir_all(parent)?;
    // A separate lock survives atomic replacement and serializes multiple app instances.
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))?;
    lock.lock()?;
    let mut bytes = Vec::new();
    match File::open(path) {
        Ok(file) => {
            file.take(1024 * 1024).read_to_end(&mut bytes)?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut entries: Vec<PathBuf> = if bytes.is_empty() {
        vec![]
    } else {
        serde_json::from_slice(&bytes).map_err(io::Error::other)?
    };
    let mut unique: Vec<PathBuf> = Vec::with_capacity(LIMIT);
    for entry in entries.drain(..) {
        if !unique.iter().any(|p| same(p, &entry)) && unique.len() < LIMIT {
            unique.push(entry);
        }
    }
    entries = unique;
    let save = !matches!(change, Change::Load);
    match change {
        Change::Load => {}
        Change::Open(repo) => {
            let repo = fs::canonicalize(&repo)?;
            entries.retain(|p| !same(p, &repo));
            entries.insert(0, repo);
            entries.truncate(LIMIT);
        }
        Change::Forget(repo) => entries.retain(|p| !same(p, &repo)),
    }
    if save {
        let temporary = path.with_extension("tmp");
        let result = (|| {
            let mut file = File::create(&temporary)?;
            file.write_all(&serde_json::to_vec(&entries)?)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_path_aliases_are_one_recent_repository() {
        assert!(same(
            Path::new(r"\\?\C:\Users\Repo"),
            Path::new("c:/users/repo")
        ));
        assert!(same(
            Path::new(r"\\?\UNC\host\share\Repo"),
            Path::new(r"\\host\share\repo")
        ));
        assert!(!same(Path::new(r"C:\repo"), Path::new(r"C:\repo-other")));
    }

    #[test]
    fn bounded_persistent_mru_and_forget() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("settings/recent.json");
        assert!(update(&store, Change::Load).unwrap().is_empty());
        let mut paths = vec![];
        for i in 0..23 {
            let p = dir.path().join(format!("repo-{i}"));
            fs::create_dir(&p).unwrap();
            paths.push(fs::canonicalize(p).unwrap());
            update(&store, Change::Open(paths[i].clone())).unwrap();
        }
        let entries = update(&store, Change::Open(paths[9].clone())).unwrap();
        assert_eq!(entries.len(), LIMIT);
        assert_eq!(entries[0], paths[9]);
        assert_eq!(entries[1], paths[22]);
        assert!(!entries.contains(&paths[2]));
        assert_eq!(update(&store, Change::Load).unwrap(), entries);
        let forgotten = update(&store, Change::Forget(paths[9].clone())).unwrap();
        assert_eq!(forgotten.len(), 19);
        assert!(!update(&store, Change::Load).unwrap().contains(&paths[9]));
    }

    #[test]
    fn concurrent_instances_merge_updates() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("recent.json");
        let workers: Vec<_> = (0..8)
            .map(|i| {
                let repo = dir.path().join(format!("repo-{i}"));
                fs::create_dir(&repo).unwrap();
                let store = store.clone();
                std::thread::spawn(move || update(&store, Change::Open(repo)).unwrap())
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(update(&store, Change::Load).unwrap().len(), 8);
    }

    #[test]
    fn corrupt_history_is_preserved_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("recent.json");
        fs::write(&store, b"broken json").unwrap();
        assert!(update(&store, Change::Load).is_err());
        assert_eq!(fs::read(&store).unwrap(), b"broken json");
    }
}
