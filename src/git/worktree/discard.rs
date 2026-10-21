//! A single-file discard, guarded by the status snapshot reviewed in the menu.
use super::*;

#[derive(Clone, Debug)]
pub struct DiscardRequest {
    pub fingerprint: u64,
    pub head: Option<ObjectId>,
    pub branch: Option<String>,
    pub file: FileChange,
}

impl DiscardRequest {
    pub fn new(snapshot: &Snapshot, file: &FileChange) -> Self {
        Self {
            fingerprint: snapshot.fingerprint,
            head: snapshot.head,
            branch: snapshot.branch.clone(),
            file: file.clone(),
        }
    }
}

fn safe_path(root: &Path, path: &str) -> Result<(), String> {
    use std::path::Component;
    let mut absolute = root.to_path_buf();
    let components = Path::new(path).components().collect::<Vec<_>>();
    if components.is_empty() {
        return Err("Cannot discard an empty path.".into());
    }
    for (i, component) in components.iter().enumerate() {
        let Component::Normal(part) = component else {
            return Err("Cannot discard a path outside the repository.".into());
        };
        if part.to_string_lossy().eq_ignore_ascii_case(".git") {
            return Err("Cannot discard Git metadata.".into());
        }
        absolute.push(part);
        if let Ok(metadata) = std::fs::symlink_metadata(&absolute) {
            if i + 1 < components.len() && (!metadata.is_dir() || metadata.is_symlink()) {
                return Err("Cannot discard through a symbolic link or non-directory.".into());
            }
            if i + 1 == components.len() && metadata.is_dir() {
                return Err("Discard file does not remove directories or submodules.".into());
            }
        }
    }
    Ok(())
}

pub fn discard_file(root: &Path, request: &DiscardRequest) -> Result<(), String> {
    let repository = super::super::open(root).map_err(|e| e.to_string())?;
    let current = snapshot(&repository)?;
    if current.conflicts {
        return Err("Resolve or abort the active conflict before discarding files.".into());
    }
    if current.fingerprint != request.fingerprint
        || current.head != request.head
        || current.branch != request.branch
        || !current.files.contains(&request.file)
    {
        return Err("Pending changes changed. Refresh and confirm Discard file again.".into());
    }
    let file = &request.file;
    safe_path(root, &file.path)?;
    if file.old.is_some_and(|s| s.kind == EntryKind::Commit) {
        return Err("Discard file does not modify submodules.".into());
    }
    if let Some(old) = &file.old_path {
        safe_path(root, old)?;
        if current
            .files
            .iter()
            .any(|other| other.path == *old && other.path != file.path)
        {
            return Err(
                "The original rename path has separate changes; discard them separately.".into(),
            );
        }
    }
    let pathspec = format!(":(literal){}", file.path);
    if file.old.is_some() {
        let original = file.old_path.as_deref().unwrap_or(&file.path);
        let original_spec = format!(":(literal){original}");
        // restore updates precisely these index entries and their worktree files.
        command::checked(
            root,
            &[
                "restore",
                "--source=HEAD",
                "--staged",
                "--worktree",
                "--",
                &original_spec,
            ],
        )?;
        if original == file.path {
            return Ok(());
        }
    }
    // A staged addition/rename destination has no HEAD content to restore.
    command::checked(
        root,
        &[
            "rm",
            "--cached",
            "--force",
            "--ignore-unmatch",
            "--",
            &pathspec,
        ],
    )?;
    match std::fs::remove_file(root.join(&file.path)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Could not discard {}: {e}", file.path)),
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::Fixture;
    use super::*;

    fn request(f: &Fixture, path: &str) -> DiscardRequest {
        let snapshot = f.snapshot();
        DiscardRequest::new(
            &snapshot,
            snapshot.files.iter().find(|f| f.path == path).unwrap(),
        )
    }

    #[test]
    fn discard_restores_staged_and_unstaged_content_preserving_other_index_entries() {
        let f = Fixture::new(true);
        f.write("a.txt", "staged a\n");
        f.write("b.txt", "staged b\n");
        f.git(&["add", "."]);
        f.write("a.txt", "unstaged a\n");
        let request = request(&f, "a.txt");
        discard_file(f.root(), &request).unwrap();
        assert_eq!(
            std::fs::read_to_string(f.root().join("a.txt")).unwrap(),
            "base a\n"
        );
        assert_eq!(f.git(&["show", ":b.txt"]), "staged b");
        assert_eq!(f.git(&["diff", "--cached", "--name-only"]), "b.txt");
    }

    #[test]
    fn discard_new_files_and_staged_deletions_and_renames() {
        let f = Fixture::new(true);
        for staged in [false, true] {
            f.write("new[1].txt", "new\n");
            if staged {
                f.git(&["add", "--", "new[1].txt"]);
            }
            discard_file(f.root(), &request(&f, "new[1].txt")).unwrap();
            assert!(!f.root().join("new[1].txt").exists());
        }
        f.git(&["rm", "a.txt"]);
        discard_file(f.root(), &request(&f, "a.txt")).unwrap();
        assert_eq!(
            std::fs::read_to_string(f.root().join("a.txt")).unwrap(),
            "base a\n"
        );
        f.git(&["mv", "a.txt", "renamed.txt"]);
        discard_file(f.root(), &request(&f, "renamed.txt")).unwrap();
        assert!(f.root().join("a.txt").exists());
        assert!(!f.root().join("renamed.txt").exists());
        assert!(f.snapshot().files.is_empty());
    }

    #[test]
    fn discard_rejects_stale_state_without_touching_content() {
        let f = Fixture::new(true);
        f.write("a.txt", "reviewed\n");
        let request = request(&f, "a.txt");
        f.write("a.txt", "changed after confirmation\n");
        assert!(discard_file(f.root(), &request).is_err());
        assert_eq!(
            std::fs::read_to_string(f.root().join("a.txt")).unwrap(),
            "changed after confirmation\n"
        );
    }

    #[test]
    fn discard_rejects_same_size_rewrite_with_changed_mtime() {
        let f = Fixture::new(true);
        f.write("a.txt", "first\n");
        let request = request(&f, "a.txt");
        // Explicit mtime keeps this deterministic on low-resolution filesystems.
        f.write("a.txt", "other\n");
        let metadata = std::fs::metadata(f.root().join("a.txt")).unwrap();
        let changed = metadata.modified().unwrap() + Duration::from_secs(2);
        File::options()
            .write(true)
            .open(f.root().join("a.txt"))
            .unwrap()
            .set_modified(changed)
            .unwrap();
        assert!(discard_file(f.root(), &request).is_err());
        assert_eq!(std::fs::read(f.root().join("a.txt")).unwrap(), b"other\n");
    }

    #[test]
    fn discard_unborn_staged_addition_and_rename_collision() {
        let f = Fixture::new(false);
        f.write("first.txt", "initial\n");
        f.git(&["add", "."]);
        discard_file(f.root(), &request(&f, "first.txt")).unwrap();
        assert!(f.snapshot().files.is_empty());
        let f = Fixture::new(true);
        f.git(&["mv", "a.txt", "renamed.txt"]);
        f.write("a.txt", "separate untracked content\n");
        assert!(discard_file(f.root(), &request(&f, "renamed.txt")).is_err());
        assert_eq!(
            std::fs::read_to_string(f.root().join("a.txt")).unwrap(),
            "separate untracked content\n"
        );
        assert!(f.root().join("renamed.txt").exists());
    }

    #[test]
    #[ignore = "15 single-file discard samples on fresh disposable fixtures"]
    fn benchmark_discard_file() {
        for sample in 0..16 {
            let f = Fixture::new(true);
            f.write(
                ".git/git-rustle-discard-fixture",
                "disposable discard benchmark fixture",
            );
            f.write("a.txt", "staged a\n");
            f.write("b.txt", "preserve staged b\n");
            f.git(&["add", "."]);
            f.write("a.txt", "unstaged a\n");
            f.write("untracked.txt", "preserve untracked\n");
            let request = request(&f, "a.txt");
            let start = std::time::Instant::now();
            discard_file(f.root(), &request).unwrap();
            let elapsed = start.elapsed().as_secs_f64() * 1000.;
            assert_eq!(
                std::fs::read_to_string(f.root().join("a.txt")).unwrap(),
                "base a\n"
            );
            assert_eq!(f.git(&["show", ":b.txt"]), "preserve staged b");
            assert!(f.root().join("untracked.txt").exists());
            println!(
                "discard_file sample={sample} warmup={} worker_ms={elapsed:.3}",
                sample == 0
            );
        }
    }
}
