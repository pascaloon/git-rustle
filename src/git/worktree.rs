//! Read-only status snapshots and selected-file commits with an isolated index.
use super::command;
use crate::diff::{ChangeKind, FileChange, Side};
use crate::pending::{Request, Snapshot};
use gix::ObjectId;
use gix::objs::tree::EntryKind;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

fn text(bytes: &[u8]) -> Result<&str, String> {
    std::str::from_utf8(bytes)
        .map_err(|_| "A path is not valid UTF-8 and cannot be committed here".into())
}
fn mode(value: &[u8]) -> Option<EntryKind> {
    match value {
        b"100644" => Some(EntryKind::Blob),
        b"100755" => Some(EntryKind::BlobExecutable),
        b"120000" => Some(EntryKind::Link),
        b"160000" => Some(EntryKind::Commit),
        _ => None,
    }
}
pub fn snapshot(repo: &gix::Repository) -> Result<Snapshot, String> {
    let Some(root) = repo.workdir() else {
        return Ok(Snapshot::default());
    };
    let bytes = command::checked(
        root,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "--no-ahead-behind",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    )?;
    parse_status(root, &bytes)
}
fn parse_status(root: &Path, bytes: &[u8]) -> Result<Snapshot, String> {
    use std::hash::{Hash, Hasher};
    let mut snapshot = Snapshot::default();
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hash);
    let mut paths = std::collections::HashMap::<String, usize>::new();
    let mut records = bytes.split(|&b| b == 0).filter(|r| !r.is_empty());
    while let Some(record) = records.next() {
        if let Some(oid) = record.strip_prefix(b"# branch.oid ") {
            if oid != b"(initial)" {
                snapshot.head = Some(ObjectId::from_hex(oid).map_err(|e| e.to_string())?);
            }
            continue;
        }
        if let Some(name) = record.strip_prefix(b"# branch.head ") {
            if name != b"(detached)" {
                snapshot.branch = Some(text(name)?.into());
            }
            continue;
        }
        let (path, old, old_path, conflict) = match record.first() {
            Some(b'1' | b'2') => {
                let rename = record[0] == b'2';
                let fields: Vec<_> = record
                    .splitn(if rename { 10 } else { 9 }, |&b| b == b' ')
                    .collect();
                if fields.len() != if rename { 10 } else { 9 } {
                    return Err("Invalid Git status record".into());
                }
                let copy = fields[1].contains(&b'C');
                let old = if copy {
                    None
                } else {
                    mode(fields[3])
                        .map(|kind| ObjectId::from_hex(fields[6]).map(|id| Side { id, kind }))
                        .transpose()
                        .map_err(|e| e.to_string())?
                };
                let original = if rename {
                    Some(text(records.next().ok_or("Missing rename path")?)?.to_owned())
                } else {
                    None
                };
                let old_path = if copy { None } else { original };
                (
                    text(fields[if rename { 9 } else { 8 }])?.to_owned(),
                    old,
                    old_path,
                    false,
                )
            }
            Some(b'?') => (text(&record[2..])?.to_owned(), None, None, false),
            Some(b'u') => {
                let fields: Vec<_> = record.splitn(11, |&b| b == b' ').collect();
                if fields.len() != 11 {
                    return Err("Invalid unmerged status record".into());
                }
                (text(fields[10])?.to_owned(), None, None, true)
            }
            _ => continue,
        };
        snapshot.conflicts |= conflict;
        let exists = std::fs::symlink_metadata(root.join(&path)).is_ok();
        if let Ok(metadata) = std::fs::symlink_metadata(root.join(&path)) {
            metadata.len().hash(&mut hash);
            metadata.modified().ok().hash(&mut hash);
        }
        let kind = if old_path.is_some() {
            ChangeKind::Renamed
        } else if old.is_none() {
            ChangeKind::Added
        } else if !exists {
            ChangeKind::Deleted
        } else {
            ChangeKind::Modified
        };
        if let Some(index) = paths.get(&path) {
            let previous = &mut snapshot.files[*index];
            // A staged deletion can also be reported as a newly untracked file.
            if previous.old.is_none() {
                previous.old = old;
            }
            previous.kind = if previous.old.is_some() && exists {
                ChangeKind::Modified
            } else {
                previous.kind
            };
        } else {
            paths.insert(path.clone(), snapshot.files.len());
            snapshot.files.push(FileChange {
                worktree: true,
                remote: None,
                path,
                old_path,
                kind,
                old,
                new: None,
            });
        }
    }
    snapshot
        .files
        .sort_by(|a, b| crate::diff::display_order(&a.path, &b.path));
    snapshot.fingerprint = hash.finish();
    Ok(snapshot)
}

static SERIAL: AtomicU64 = AtomicU64::new(0);
struct IndexTransaction {
    index: PathBuf,
    lock: PathBuf,
    temporary: PathBuf,
    reconcile: PathBuf,
    message: PathBuf,
    hooks: PathBuf,
    hook_files: Vec<PathBuf>,
    handle: Option<File>,
}
impl IndexTransaction {
    fn new(repo: &gix::Repository) -> Result<Self, String> {
        let index = repo.git_dir().join("index");
        let lock = repo.git_dir().join("index.lock");
        let handle = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .map_err(|e| {
                format!("Cannot lock Git index: {e}. Wait for other Git operations to finish.")
            })?;
        let name = format!(
            "rustle-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        );
        Ok(Self {
            index,
            lock,
            temporary: repo.git_dir().join(format!("{name}.index")),
            reconcile: repo.git_dir().join(format!("{name}.reconcile")),
            message: repo.git_dir().join(format!("{name}.message")),
            hooks: repo.git_dir().join(format!("{name}.hooks")),
            hook_files: Vec::new(),
            handle: Some(handle),
        })
    }
    fn install(&mut self) -> Result<(), String> {
        let bytes = std::fs::read(&self.reconcile).map_err(|e| e.to_string())?;
        let mut file = self.handle.take().unwrap();
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        drop(file);
        std::fs::rename(&self.lock, &self.index).map_err(|e| e.to_string())
    }
}
impl Drop for IndexTransaction {
    fn drop(&mut self) {
        self.handle.take();
        for file in [&self.lock, &self.temporary, &self.reconcile, &self.message] {
            let _ = std::fs::remove_file(file);
        }
        for file in &self.hook_files {
            let _ = std::fs::remove_file(file);
        }
        let _ = std::fs::remove_dir(&self.hooks);
        for file in [&self.temporary, &self.reconcile] {
            let _ = std::fs::remove_file(file.with_extension(format!(
                "{}.lock",
                file.extension().unwrap().to_string_lossy()
            )));
        }
    }
}
fn forward_other_hooks(
    root: &Path,
    transaction: &mut IndexTransaction,
) -> Result<[bool; 4], String> {
    let mut commit_hooks = [false; 4];
    let configured = command::run(
        root,
        &["config", "--path", "--get", "core.hooksPath"],
        None,
        Duration::from_secs(10),
    )?;
    let directory = if configured.status.success() {
        PathBuf::from(text(&configured.stdout)?.trim())
    } else if configured.status.code() == Some(1) {
        PathBuf::from(text(&command::checked(root, &["rev-parse", "--git-common-dir"])?)?.trim())
            .join("hooks")
    } else {
        return Err("Cannot read Git hook configuration".into());
    };
    let directory = if directory.is_absolute() {
        directory
    } else {
        root.join(directory)
    };
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(e)
            if e.kind() == std::io::ErrorKind::NotFound
                || e.kind() == std::io::ErrorKind::NotADirectory =>
        {
            return Ok(commit_hooks);
        }
        Err(e) => return Err(e.to_string()),
    };
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.path().is_file() {
            continue;
        }
        let name = entry.file_name();
        if let Some(index) = [
            "pre-commit",
            "prepare-commit-msg",
            "commit-msg",
            "post-commit",
        ]
        .iter()
        .position(|n| name == *n)
        {
            commit_hooks[index] = true;
            continue;
        }
        if name.to_string_lossy().ends_with(".sample") {
            continue;
        }
        let source = entry
            .path()
            .to_string_lossy()
            .replace('\\', "/")
            .replace('\'', "'\"'\"'");
        let target = transaction.hooks.join(name);
        std::fs::write(&target, format!("#!/bin/sh\nexec '{source}' \"$@\"\n"))
            .map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        transaction.hook_files.push(target);
    }
    Ok(commit_hooks)
}

fn git(root: &Path, index: &Path, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>, String> {
    let output = command::run_env(
        root,
        args,
        input,
        Duration::from_secs(300),
        &[
            ("GIT_INDEX_FILE", index.as_os_str()),
            ("GIT_EDITOR", std::ffi::OsStr::new("true")),
        ],
    )?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let message = if output.stderr.is_empty() {
            &output.stdout
        } else {
            &output.stderr
        };
        Err(command::clean(&String::from_utf8_lossy(message)))
    }
}
pub fn commit(root: &Path, request: &Request) -> Result<String, String> {
    if request.description.trim().is_empty() {
        return Err("Enter a commit description".into());
    }
    if request.files.is_empty() {
        return Err("Check at least one file to commit".into());
    }
    let repo = super::open(root).map_err(|e| e.to_string())?;
    if repo.workdir().is_none() {
        return Err("Committing needs a working tree".into());
    }
    for operation in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
        "sequencer",
    ] {
        if repo.git_dir().join(operation).exists() {
            return Err(
                "Finish the ongoing merge, rebase, or cherry-pick before committing here".into(),
            );
        }
    }
    let head = repo.head().map_err(|e| e.to_string())?;
    let current = head.id().map(|id| id.detach());
    let branch = head.referent_name().map(|name| name.shorten().to_string());
    if current != request.head || branch != request.branch {
        return Err("HEAD changed; refresh pending changes before committing".into());
    }
    let branch_name = request.new_branch.trim();
    if !branch_name.is_empty() {
        if branch_name.starts_with('-') || branch_name == "HEAD" {
            return Err("Invalid new branch name".into());
        }
        command::checked(
            root,
            &["check-ref-format", &format!("refs/heads/{branch_name}")],
        )?;
        let exists = command::run(
            root,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch_name}"),
            ],
            None,
            Duration::from_secs(10),
        )?;
        if exists.status.success() {
            return Err("That branch already exists; choose a new branch name".into());
        }
    }
    let mut transaction = IndexTransaction::new(&repo)?;
    if !command::checked(root, &["ls-files", "-u"])?.is_empty() {
        return Err("Resolve file conflicts before committing".into());
    }
    let mut paths = Vec::new();
    for file in &request.files {
        for path in std::iter::once(&file.path).chain(file.old_path.iter()) {
            if path.is_empty()
                || path.as_bytes().contains(&0)
                || Path::new(path).is_absolute()
                || Path::new(path)
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                return Err("Invalid selected file path".into());
            }
            paths.extend_from_slice(path.as_bytes());
            paths.push(0);
        }
    }
    if let Some(id) = current {
        git(
            root,
            &transaction.temporary,
            &["read-tree", &id.to_string()],
            None,
        )?;
    } else {
        git(
            root,
            &transaction.temporary,
            &["read-tree", "--empty"],
            None,
        )?;
    }
    git(
        root,
        &transaction.temporary,
        &[
            "--literal-pathspecs",
            "add",
            "-A",
            "--pathspec-from-file=-",
            "--pathspec-file-nul",
        ],
        Some(&paths),
    )?;
    // Preserve unchecked staged entries. Only selected paths are reconciled after success.
    if transaction.index.exists() {
        std::fs::copy(&transaction.index, &transaction.reconcile).map_err(|e| e.to_string())?;
    } else {
        git(
            root,
            &transaction.reconcile,
            &["read-tree", "--empty"],
            None,
        )?;
    }
    let mut post_commit = false;
    let result = (|| {
        if !branch_name.is_empty() {
            // Creating at the same HEAD changes no working files. Git checks name/worktree conflicts.
            git(
                root,
                &transaction.temporary,
                &["switch", "-c", branch_name],
                None,
            )?;
        }
        std::fs::write(&transaction.message, request.description.as_bytes())
            .map_err(|e| e.to_string())?;
        std::fs::create_dir(&transaction.hooks).map_err(|e| e.to_string())?;
        let hooks = forward_other_hooks(root, &mut transaction)?;
        post_commit = hooks[3];
        if hooks[0] {
            git(
                root,
                &transaction.temporary,
                &["hook", "run", "--ignore-missing", "pre-commit"],
                None,
            )?;
        }
        let message = transaction
            .message
            .to_str()
            .ok_or("Invalid commit message path")?;
        if hooks[1] {
            git(
                root,
                &transaction.temporary,
                &[
                    "hook",
                    "run",
                    "--ignore-missing",
                    "prepare-commit-msg",
                    "--",
                    message,
                    "message",
                ],
                None,
            )?;
        }
        if hooks[2] {
            git(
                root,
                &transaction.temporary,
                &[
                    "hook",
                    "run",
                    "--ignore-missing",
                    "commit-msg",
                    "--",
                    message,
                ],
                None,
            )?;
        }
        let changed = git(
            root,
            &transaction.temporary,
            &["diff", "--cached", "--name-only", "-z", "--no-renames"],
            None,
        )?;
        let allowed: std::collections::HashSet<_> =
            paths.split(|&b| b == 0).filter(|p| !p.is_empty()).collect();
        if changed.is_empty() {
            return Err("The checked files have no changes to commit".into());
        }
        if changed
            .split(|&b| b == 0)
            .filter(|p| !p.is_empty())
            .any(|p| !allowed.contains(p))
        {
            return Err("A commit hook staged unchecked files. Check those files or adjust the hook; nothing was committed.".into());
        }
        let now = super::open(root).map_err(|e| e.to_string())?;
        let now = now.head().map_err(|e| e.to_string())?;
        let expected = if branch_name.is_empty() {
            branch.clone()
        } else {
            Some(branch_name.into())
        };
        if now.id().map(|id| id.detach()) != current
            || now.referent_name().map(|n| n.shorten().to_string()) != expected
        {
            return Err("HEAD changed while preparing the commit; refresh before retrying".into());
        }
        let hooks = format!("core.hooksPath={}", transaction.hooks.display());
        git(
            root,
            &transaction.temporary,
            &["-c", &hooks, "commit", "--file", message],
            None,
        )
    })();
    if let Err(error) = result {
        if !branch_name.is_empty() {
            let after = super::open(root)
                .and_then(|r| {
                    r.head()
                        .map(|h| {
                            (
                                h.id().map(|id| id.detach()),
                                h.referent_name().map(|n| n.shorten().to_string()),
                            )
                        })
                        .map_err(Into::into)
                })
                .ok();
            if after == Some((current, Some(branch_name.into()))) {
                let restore = if let Some(original) = &branch {
                    command::checked(
                        root,
                        &["symbolic-ref", "HEAD", &format!("refs/heads/{original}")],
                    )
                } else if let Some(id) = current {
                    command::checked(root, &["update-ref", "--no-deref", "HEAD", &id.to_string()])
                } else {
                    Err("Cannot restore unborn detached HEAD".into())
                };
                if restore.is_ok()
                    && let Some(id) = current
                {
                    let _ = command::checked(
                        root,
                        &[
                            "update-ref",
                            "-d",
                            &format!("refs/heads/{branch_name}"),
                            &id.to_string(),
                        ],
                    );
                }
            }
        }
        return Err(error);
    }
    let completed = super::open(root).map_err(|e| e.to_string())?;
    let id = completed
        .head()
        .map_err(|e| e.to_string())?
        .id()
        .ok_or("Git did not create a commit")?
        .to_string();
    let id = id.as_str();
    let reconciled = git(
        root,
        &transaction.reconcile,
        &[
            "--literal-pathspecs",
            "reset",
            "-q",
            id,
            "--pathspec-from-file=-",
            "--pathspec-file-nul",
        ],
        Some(&paths),
    )
    .and_then(|_| transaction.install());
    if let Err(error) = reconciled {
        return Err(format!(
            "Commit {id} was created, but the index could not be updated: {error}"
        ));
    }
    if post_commit
        && let Err(error) = git(
            root,
            &transaction.index,
            &["hook", "run", "--ignore-missing", "post-commit"],
            None,
        )
    {
        return Ok(format!(
            "Committed {}. Post-commit hook reported: {error}",
            &id[..12.min(id.len())]
        ));
    }
    Ok(format!(
        "Committed {}{}",
        &id[..12.min(id.len())],
        if branch_name.is_empty() {
            String::new()
        } else {
            format!(" on {branch_name}")
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(tempfile::TempDir);
    impl Fixture {
        fn new(committed: bool) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let f = Self(dir);
            f.git(&["init", "-q", "-b", "main"]);
            for (key, value) in [
                ("user.name", "Pending Test"),
                ("user.email", "test@example.invalid"),
                ("commit.gpgSign", "false"),
                ("core.autocrlf", "false"),
                ("core.hooksPath", "disabled-test-hooks"),
            ] {
                f.git(&["config", key, value]);
            }
            if committed {
                f.write("a.txt", "base a\n");
                f.write("b.txt", "base b\n");
                f.git(&["add", "."]);
                f.git(&["commit", "-qm", "Initial"]);
            }
            f
        }
        fn root(&self) -> &Path {
            self.0.path()
        }
        fn git(&self, args: &[&str]) -> String {
            String::from_utf8(command::checked(self.root(), args).unwrap())
                .unwrap()
                .trim()
                .into()
        }
        fn write(&self, path: &str, value: &str) {
            let p = self.root().join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, value).unwrap();
        }
        fn snapshot(&self) -> Snapshot {
            snapshot(&super::super::open(self.root()).unwrap()).unwrap()
        }
        fn request(&self, paths: &[&str]) -> Request {
            let s = self.snapshot();
            Request {
                head: s.head,
                branch: s.branch,
                new_branch: String::new(),
                description: "Selected changes\n\nBody with é Unicode".into(),
                files: s
                    .files
                    .into_iter()
                    .filter(|f| paths.contains(&f.path.as_str()))
                    .collect(),
            }
        }
        fn hook(&self, name: &str, body: &str) {
            self.git(&["config", "core.hooksPath", ".git/hooks"]);
            self.write(
                &format!(".git/hooks/{name}"),
                &format!("#!/bin/sh\n{body}\n"),
            );
        }
    }
    #[test]
    fn status_and_diffs_include_staged_unstaged_untracked_deleted_and_binary() {
        let f = Fixture::new(true);
        f.write("a.txt", "staged a\n");
        f.git(&["add", "a.txt"]);
        f.write("a.txt", "working a\n");
        std::fs::remove_file(f.root().join("b.txt")).unwrap();
        f.write("new space [1].txt", "untracked\n");
        f.write(".gitignore", "ignored\n");
        f.write("ignored", "ignore me");
        std::fs::write(f.root().join("binary.bin"), [0, 1, 2]).unwrap();
        let s = f.snapshot();
        assert!(!s.conflicts);
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert!(!s.files.iter().any(|c| c.path == "ignored"));
        let repo = super::super::open(f.root()).unwrap();
        let a = s.files.iter().find(|c| c.path == "a.txt").unwrap();
        let diff = crate::diff::file_diff(&repo, a).unwrap();
        assert!(diff.rows.iter().any(
            |r| matches!(r,crate::diff::Row::Line{right:Some(line),..} if line.text=="working a")
        ));
        let b = s.files.iter().find(|c| c.path == "b.txt").unwrap();
        assert_eq!(b.kind, ChangeKind::Deleted);
        assert_eq!(
            crate::diff::file_stats(&repo, b).unwrap(),
            crate::diff::Stat::Lines {
                added: 0,
                removed: 1
            }
        );
        let binary = s.files.iter().find(|c| c.path == "binary.bin").unwrap();
        assert_eq!(
            crate::diff::file_stats(&repo, binary).unwrap(),
            crate::diff::Stat::Opaque
        );
    }
    #[test]
    fn worktree_diff_uses_git_line_ending_conversion() {
        let f = Fixture::new(false);
        f.write("a.txt", "first\noriginal\nthird\n");
        f.git(&["add", "a.txt"]);
        f.git(&["commit", "-qm", "Initial"]);
        f.git(&["config", "core.autocrlf", "true"]);
        f.write("a.txt", "first\r\nupdated\r\nthird\r\n");

        assert_eq!(f.git(&["diff", "--numstat", "--", "a.txt"]), "1\t1\ta.txt");
        let repo = super::super::open(f.root()).unwrap();
        let change = f
            .snapshot()
            .files
            .into_iter()
            .find(|c| c.path == "a.txt")
            .unwrap();
        let diff = crate::diff::file_diff(&repo, &change).unwrap();
        assert_eq!((diff.added, diff.removed), (1, 1));
        assert_eq!(
            crate::diff::file_stats(&repo, &change).unwrap(),
            crate::diff::Stat::Lines {
                added: 1,
                removed: 1
            }
        );
    }
    #[test]
    fn worktree_diff_honors_external_git_config() {
        if let Some(root) = std::env::var_os("GIT_RUSTLE_EOL_CHILD_REPO") {
            let repo = super::super::open(Path::new(&root)).unwrap();
            let change = snapshot(&repo)
                .unwrap()
                .files
                .into_iter()
                .find(|change| change.path == "a.txt")
                .unwrap();
            let diff = crate::diff::file_diff(&repo, &change).unwrap();
            assert_eq!((diff.added, diff.removed), (1, 1));
            assert_eq!(
                crate::diff::file_stats(&repo, &change).unwrap(),
                crate::diff::Stat::Lines {
                    added: 1,
                    removed: 1
                }
            );
            return;
        }

        let f = Fixture::new(false);
        f.write("a.txt", "first\noriginal\nthird\n");
        f.git(&["add", "a.txt"]);
        f.git(&["commit", "-qm", "Initial"]);
        f.git(&["config", "--unset", "core.autocrlf"]);
        f.write("a.txt", "first\r\nupdated\r\nthird\n");
        f.write("global.gitconfig", "[core]\n\tautocrlf = true\n");

        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "git::worktree::tests::worktree_diff_honors_external_git_config",
                "--nocapture",
            ])
            .env("GIT_RUSTLE_EOL_CHILD_REPO", f.root())
            .env("GIT_CONFIG_GLOBAL", f.root().join("global.gitconfig"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[test]
    fn worktree_diff_preserves_crlf_when_attributes_disable_conversion() {
        let f = Fixture::new(false);
        f.write(".gitattributes", "*.txt -text\n");
        f.write("a.txt", "first\noriginal\nthird\n");
        f.git(&["add", ".gitattributes", "a.txt"]);
        f.git(&["commit", "-qm", "Initial"]);
        f.git(&["config", "core.autocrlf", "true"]);
        f.write("a.txt", "first\r\nupdated\r\nthird\r\n");

        assert_eq!(f.git(&["diff", "--numstat", "--", "a.txt"]), "3\t3\ta.txt");
        let repo = super::super::open(f.root()).unwrap();
        let change = f
            .snapshot()
            .files
            .into_iter()
            .find(|c| c.path == "a.txt")
            .unwrap();
        let diff = crate::diff::file_diff(&repo, &change).unwrap();
        assert_eq!((diff.added, diff.removed), (3, 3));
        assert_eq!(
            crate::diff::file_stats(&repo, &change).unwrap(),
            crate::diff::Stat::Lines {
                added: 3,
                removed: 3
            }
        );
    }
    #[test]
    fn selected_commit_preserves_unchecked_index_and_worktree() {
        let f = Fixture::new(true);
        f.write("a.txt", "partial a\n");
        f.write("b.txt", "staged b\n");
        f.git(&["add", "a.txt", "b.txt"]);
        f.write("a.txt", "full working a\n");
        f.write("b.txt", "working b\n");
        f.write("untracked.txt", "keep\n");
        let staged = f.git(&["show", ":b.txt"]);
        let request = f.request(&["a.txt"]);
        commit(f.root(), &request).unwrap();
        assert_eq!(f.git(&["show", "HEAD:a.txt"]), "full working a");
        assert_eq!(f.git(&["show", "HEAD:b.txt"]), "base b");
        assert_eq!(f.git(&["show", ":b.txt"]), staged);
        assert_eq!(
            std::fs::read_to_string(f.root().join("b.txt")).unwrap(),
            "working b\n"
        );
        assert_eq!(f.git(&["diff", "--cached", "--name-only"]), "b.txt");
        assert!(f.root().join("untracked.txt").exists());
        assert!(!f.root().join(".git/index.lock").exists());
    }
    #[test]
    fn new_branch_commit_handles_untracked_paths_and_leaves_main_unchanged() {
        let f = Fixture::new(true);
        let main = f.git(&["rev-parse", "main"]);
        f.write("unicodé [1] space.txt", "new\n");
        f.write("other.txt", "keep\n");
        let mut request = f.request(&["unicodé [1] space.txt"]);
        request.new_branch = "feature/selected".into();
        commit(f.root(), &request).unwrap();
        assert_eq!(f.git(&["branch", "--show-current"]), "feature/selected");
        assert_eq!(f.git(&["rev-parse", "main"]), main);
        assert_eq!(f.git(&["show", "HEAD:unicodé [1] space.txt"]), "new");
        assert!(
            !f.git(&["ls-tree", "-r", "--name-only", "HEAD"])
                .contains("other.txt")
        );
    }
    #[test]
    fn deleted_and_renamed_files_are_committed_as_single_selections() {
        let f = Fixture::new(true);
        f.git(&["mv", "a.txt", "renamed.txt"]);
        std::fs::remove_file(f.root().join("b.txt")).unwrap();
        let request = f.request(&["renamed.txt", "b.txt"]);
        assert_eq!(
            request
                .files
                .iter()
                .find(|c| c.path == "renamed.txt")
                .unwrap()
                .old_path
                .as_deref(),
            Some("a.txt")
        );
        commit(f.root(), &request).unwrap();
        assert_eq!(
            f.git(&["ls-tree", "-r", "--name-only", "HEAD"]),
            "renamed.txt"
        );
        assert!(f.snapshot().files.is_empty());
    }
    #[test]
    fn hooks_validate_message_and_cannot_sweep_in_unchecked_files() {
        let f = Fixture::new(true);
        f.write("a.txt", "changed a\n");
        f.write("b.txt", "changed b\n");
        let before = std::fs::read(f.root().join(".git/index")).unwrap();
        let head = f.git(&["rev-parse", "HEAD"]);
        f.hook("pre-commit", "git add b.txt");
        let mut request = f.request(&["a.txt"]);
        request.new_branch = "hook-test".into();
        assert!(
            commit(f.root(), &request)
                .unwrap_err()
                .contains("unchecked")
        );
        assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
        assert_eq!(f.git(&["branch", "--show-current"]), "main");
        assert_eq!(std::fs::read(f.root().join(".git/index")).unwrap(), before);
        assert!(
            !f.git(&["branch", "--list", "hook-test"])
                .contains("hook-test")
        );
        f.hook("pre-commit", "exit 0");
        f.hook("commit-msg", "printf '\\nHook footer\\n' >> \"$1\"");
        request.new_branch.clear();
        commit(f.root(), &request).unwrap();
        assert!(f.git(&["log", "-1", "--format=%B"]).contains("Hook footer"));
    }
    #[test]
    fn failed_hooks_and_stale_requests_leave_index_and_head_alone() {
        let f = Fixture::new(true);
        f.write("a.txt", "changes\n");
        f.hook("pre-commit", "echo rejected >&2; exit 1");
        let request = f.request(&["a.txt"]);
        let before = std::fs::read(f.root().join(".git/index")).unwrap();
        let head = f.git(&["rev-parse", "HEAD"]);
        assert!(commit(f.root(), &request).unwrap_err().contains("rejected"));
        assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
        assert_eq!(std::fs::read(f.root().join(".git/index")).unwrap(), before);
        std::fs::write(
            f.root().join(".git/index.lock"),
            "owned by another Git operation",
        )
        .unwrap();
        assert!(commit(f.root(), &request).unwrap_err().contains("lock"));
        assert_eq!(
            std::fs::read_to_string(f.root().join(".git/index.lock")).unwrap(),
            "owned by another Git operation"
        );
        std::fs::remove_file(f.root().join(".git/index.lock")).unwrap();
        f.git(&["switch", "-c", "different"]);
        assert!(
            commit(f.root(), &request)
                .unwrap_err()
                .contains("HEAD changed")
        );
    }
    #[test]
    fn unborn_and_detached_head_can_commit_selected_files() {
        let f = Fixture::new(false);
        f.write("first.txt", "first\n");
        f.write("excluded.txt", "keep\n");
        let mut request = f.request(&["first.txt"]);
        request.new_branch = "initial-branch".into();
        commit(f.root(), &request).unwrap();
        assert_eq!(
            f.git(&["ls-tree", "-r", "--name-only", "HEAD"]),
            "first.txt"
        );
        let branch = f.git(&["rev-parse", "initial-branch"]);
        f.git(&["switch", "--detach"]);
        f.write("first.txt", "second\n");
        commit(f.root(), &f.request(&["first.txt"])).unwrap();
        assert_eq!(f.git(&["branch", "--show-current"]), "");
        assert_eq!(f.git(&["rev-parse", "initial-branch"]), branch);
    }
    #[test]
    fn linked_worktree_uses_its_own_index_and_branch() {
        let f = Fixture::new(true);
        let work = f.root().join("linked");
        let original = f.git(&["rev-parse", "main"]);
        f.git(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            work.to_str().unwrap(),
        ]);
        std::fs::write(work.join("a.txt"), "linked change\n").unwrap();
        let s = snapshot(&super::super::open(&work).unwrap()).unwrap();
        let request = Request {
            head: s.head,
            branch: s.branch,
            new_branch: String::new(),
            description: "Linked worktree".into(),
            files: s.files,
        };
        commit(&work, &request).unwrap();
        assert_eq!(f.git(&["rev-parse", "main"]), original);
        assert_eq!(
            String::from_utf8(command::checked(&work, &["show", "HEAD:a.txt"]).unwrap()).unwrap(),
            "linked change\n"
        );
    }

    #[test]
    fn unmerged_real_index_is_rejected_even_for_an_unrelated_selection() {
        let f = Fixture::new(true);
        f.write("b.txt", "selected\n");
        let blob = f.git(&["rev-parse", "HEAD:a.txt"]);
        let data = format!(
            "0 {}\ta.txt\n100644 {blob} 1\ta.txt\n100644 {blob} 2\ta.txt\n100644 {blob} 3\ta.txt\n",
            "0".repeat(40)
        );
        let out = command::run(
            f.root(),
            &["update-index", "--index-info"],
            Some(data.as_bytes()),
            Duration::from_secs(10),
        )
        .unwrap();
        assert!(out.status.success());
        assert!(f.snapshot().conflicts);
        assert!(
            commit(f.root(), &f.request(&["b.txt"]))
                .unwrap_err()
                .contains("conflicts")
        );
    }

    #[test]
    fn reference_hook_is_honored_even_with_a_quoted_custom_hooks_path() {
        let f = Fixture::new(true);
        f.write("a.txt", "changed\n");
        let dir = ".git/custom hooks' é";
        f.git(&["config", "core.hooksPath", dir]);
        f.write(&format!("{dir}/reference-transaction"),"#!/bin/sh\nif [ \"$1\" = prepared ]; then echo reference-policy-rejected >&2; exit 1; fi\n");
        let request = f.request(&["a.txt"]);
        let head = f.git(&["rev-parse", "HEAD"]);
        let index = std::fs::read(f.root().join(".git/index")).unwrap();
        assert!(
            commit(f.root(), &request)
                .unwrap_err()
                .contains("reference-policy-rejected")
        );
        assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
        assert_eq!(std::fs::read(f.root().join(".git/index")).unwrap(), index);
        assert!(!std::fs::read_dir(f.root().join(".git")).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("rustle-")
        }));
    }

    #[test]
    #[ignore = "explicit pending-change status, checkbox, drawing and diff performance measurement"]
    fn benchmark_pending() {
        use std::time::Instant;
        let f = Fixture::new(false);
        for n in 0..1000 {
            f.write(
                &format!("src/group{:02}/file{n:04}.txt", n / 50),
                "base content\n",
            );
        }
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "1000-file fixture"]);
        for n in 0..1000 {
            f.write(
                &format!("src/group{:02}/file{n:04}.txt", n / 50),
                "updated content\n",
            );
        }
        let repo = super::super::open(f.root()).unwrap();
        let refs = super::super::snapshot_refs(&repo).unwrap();
        let mut history = crate::model::History::default();
        super::super::walk(&repo, &refs.tips, &mut |chunk| {
            history.append(chunk);
            true
        })
        .unwrap();
        let mut app = crate::app::App::new();
        app.set_cached(history, true);
        app.set_refs(refs);
        app.pending.autofocus = true;
        app.install_pending(Ok(snapshot(&repo).unwrap()));
        app.sync_details_changes();
        app.focus = crate::app::Focus::Files;
        app.pending.description.value = "Benchmark pending commit".into();
        let mut samples = Vec::new();
        for n in 0..16 {
            let start = Instant::now();
            let value = snapshot(&repo).unwrap();
            let status_ms = start.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(value.files.len(), 1000);
            let changes = app.details_changes.as_mut().unwrap();
            let start = Instant::now();
            changes.set_all_checked(false);
            changes.set_all_checked(true);
            let checks_ms = start.elapsed().as_secs_f64() * 1000.0 / 2.0;
            let start = Instant::now();
            let diff = crate::diff::file_diff(&repo, &value.files[0]).unwrap();
            let file_ms = start.elapsed().as_secs_f64() * 1000.0;
            assert!(diff.added > 0);
            let mut screen = crate::term::Screen::new(120, 40);
            let areas = crate::ui::compute_areas(120, 40, 48);
            let start = Instant::now();
            for _ in 0..200 {
                let changes = app.details_changes.as_ref().unwrap();
                app.details_cursor.move_by(&changes.tree, 1);
                crate::ui::draw(&mut app, &mut screen, &areas, 1700000000);
                std::hint::black_box(screen.test_cells());
            }
            let draw_ms = start.elapsed().as_secs_f64() * 1000.0 / 200.0;
            if n > 0 {
                samples.push(serde_json::json!({"sample":n-1,"statusMs":status_ms,"bulkCheckMs":checks_ms,"fileDiffMs":file_ms,"selectDrawMs":draw_ms}));
            }
        }
        println!(
            "1000 files; one warm-up, 15 samples: {}",
            serde_json::to_string(&samples).unwrap()
        );
    }
}
