//! File discard, guarded by the complete status snapshot reviewed in the menu.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
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
    discard_files(root, std::slice::from_ref(request))
}

/// Validate the entire reviewed selection before changing any file. The outer
/// operation journal captures the batch as one recoverable action.
pub fn discard_files(root: &Path, requests: &[DiscardRequest]) -> Result<(), String> {
    if requests.is_empty() {
        return Err("Select at least one file to discard.".into());
    }
    let repository = super::super::open(root).map_err(|e| e.to_string())?;
    let current = snapshot(&repository)?;
    if current.conflicts {
        return Err("Resolve or abort the active conflict before discarding files.".into());
    }
    let current_files: std::collections::HashMap<_, _> = current
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    let mut paths = std::collections::HashSet::new();
    for request in requests {
        if !paths.insert(&request.file.path) {
            return Err("Duplicate discard path.".into());
        }
        if current.fingerprint != request.fingerprint
            || current.head != request.head
            || current.branch != request.branch
            || current_files.get(request.file.path.as_str()).copied() != Some(&request.file)
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
            if old != &file.path && current_files.contains_key(old.as_str()) {
                return Err(
                    "The original rename path has separate changes; discard them separately."
                        .into(),
                );
            }
        }
    }
    let mut restore = std::collections::BTreeSet::new();
    let mut remove = std::collections::BTreeSet::new();
    for request in requests {
        let file = &request.file;
        if file.old.is_some() {
            let original = file.old_path.as_deref().unwrap_or(&file.path);
            restore.insert(original);
            if original == file.path {
                continue;
            }
        }
        // Added files and rename destinations have no HEAD content to restore.
        remove.insert(file.path.as_str());
    }
    if !restore.is_empty() {
        discard_paths(
            root,
            &[
                "restore",
                "--source=HEAD",
                "--staged",
                "--worktree",
                "--pathspec-from-file=-",
                "--pathspec-file-nul",
            ],
            &restore,
        )?;
    }
    if !remove.is_empty() {
        discard_paths(
            root,
            &[
                "rm",
                "--cached",
                "--force",
                "--ignore-unmatch",
                "--pathspec-from-file=-",
                "--pathspec-file-nul",
            ],
            &remove,
        )?;
        for path in remove {
            match std::fs::remove_file(root.join(path)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("Could not discard {path}: {e}")),
            }
        }
    }
    Ok(())
}

fn discard_paths(
    root: &Path,
    args: &[&str],
    paths: &std::collections::BTreeSet<&str>,
) -> Result<(), String> {
    // NUL-delimited literal pathspecs preserve spaces, glob characters and newlines
    // without growing the process command line with the selected file count.
    let mut input = Vec::new();
    for path in paths {
        input.extend_from_slice(b":(literal)");
        input.extend_from_slice(path.as_bytes());
        input.push(0);
    }
    let output = command::run(root, args, Some(&input), Duration::from_secs(300))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command::clean(&String::from_utf8_lossy(&output.stderr)))
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

    fn batch_fixture() -> (Fixture, Vec<DiscardRequest>) {
        let f = Fixture::new(true);
        for index in 0..50 {
            f.write(
                &format!("batch/tracked [{index}].txt"),
                &format!("base {index}\n"),
            );
        }
        f.git(&["add", "batch"]);
        f.git(&["commit", "-qm", "Batch discard baseline"]);
        for index in 2..50 {
            f.write(
                &format!("batch/tracked [{index}].txt"),
                &format!("staged {index}\n"),
            );
        }
        for index in 0..20 {
            f.write(&format!("batch/added [{index}].txt"), "staged addition\n");
        }
        f.write("b.txt", "preserved staged b\n");
        f.git(&["add", "."]);
        f.git(&["mv", "batch/tracked [0].txt", "batch/renamed [0].txt"]);
        f.git(&["--literal-pathspecs", "rm", "batch/tracked [1].txt"]);
        for index in 2..12 {
            f.write(
                &format!("batch/tracked [{index}].txt"),
                &format!("unstaged {index}\n"),
            );
        }
        for index in 0..30 {
            f.write(
                &format!("batch/untracked [{index}].txt"),
                "untracked addition\n",
            );
        }
        f.write("a.txt", "preserved unstaged a\n");
        f.write("b.txt", "preserved unstaged b\n");
        f.write("preserve-untracked.txt", "preserved untracked bytes\n");
        let snapshot = f.snapshot();
        let requests: Vec<_> = snapshot
            .files
            .iter()
            .filter(|file| file.path.starts_with("batch/"))
            .map(|file| DiscardRequest::new(&snapshot, file))
            .collect();
        assert_eq!(requests.len(), 100);
        (f, requests)
    }

    fn assert_batch_discarded(f: &Fixture) {
        for index in 0..50 {
            assert_eq!(
                std::fs::read_to_string(f.root().join(format!("batch/tracked [{index}].txt")))
                    .unwrap(),
                format!("base {index}\n")
            );
        }
        assert!(!f.root().join("batch/renamed [0].txt").exists());
        for (kind, count) in [("added", 20), ("untracked", 30)] {
            for index in 0..count {
                assert!(
                    !f.root()
                        .join(format!("batch/{kind} [{index}].txt"))
                        .exists()
                );
            }
        }
        assert_eq!(f.git(&["diff", "--cached", "--name-only"]), "b.txt");
        assert_eq!(f.git(&["show", ":b.txt"]), "preserved staged b");
        for (path, expected) in [
            ("a.txt", "preserved unstaged a\n"),
            ("b.txt", "preserved unstaged b\n"),
            ("preserve-untracked.txt", "preserved untracked bytes\n"),
        ] {
            assert_eq!(
                std::fs::read_to_string(f.root().join(path)).unwrap(),
                expected
            );
        }
        assert!(
            !f.snapshot()
                .files
                .iter()
                .any(|file| file.path.starts_with("batch/"))
        );
    }

    #[test]
    fn discard_hundred_mixed_files_preserves_unrelated_index_and_worktree() {
        let (f, requests) = batch_fixture();
        discard_files(f.root(), &requests).unwrap();
        assert_batch_discarded(&f);
    }

    #[test]
    #[ignore = "one warmup and fifteen 100-file tracked/untracked batch discard samples"]
    fn benchmark_discard_batch() {
        for sample in -1..15 {
            let (f, requests) = batch_fixture();
            let started = std::time::Instant::now();
            discard_files(f.root(), &requests).unwrap();
            let worker_ms = started.elapsed().as_secs_f64() * 1000.;
            assert_batch_discarded(&f);
            println!(
                "{}",
                serde_json::json!({"sample": sample, "files": requests.len(), "workerMs": worker_ms})
            );
        }
    }

    #[test]
    fn discard_batch_validates_every_request_before_mutating() {
        let f = Fixture::new(true);
        f.write("a.txt", "changed a\n");
        f.write("b.txt", "changed b\n");
        let a = request(&f, "a.txt");
        let mut b = request(&f, "b.txt");
        b.fingerprint ^= 1;
        assert!(discard_files(f.root(), &[a.clone(), b]).is_err());
        assert_eq!(
            std::fs::read_to_string(f.root().join("a.txt")).unwrap(),
            "changed a\n"
        );
        assert!(discard_files(f.root(), &[a.clone(), a]).is_err());
        assert_eq!(
            std::fs::read_to_string(f.root().join("b.txt")).unwrap(),
            "changed b\n"
        );
        assert!(discard_files(f.root(), &[]).is_err());
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
