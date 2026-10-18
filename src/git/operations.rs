//! Serialized worker-side mutations and guarded, persistent recovery checkpoints.
use super::command;
use gix::bstr::ByteSlice;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug)]
pub enum Request {
    Inspect,
    PreviewPush,
    OnBranch {
        branch: String,
        head: String,
        request: Box<Request>,
    },
    PushReviewed(PushPreview),
    Create {
        name: String,
        target: String,
    },
    Delete {
        name: String,
        force: bool,
    },
    Rename {
        old: String,
        new: String,
    },
    Pull,
    Merge(String),
    Rebase(String),
    Plan(String),
    Interactive {
        plan: Plan,
        text: String,
    },
    Continue,
    Skip,
    Abort,
    Resolve {
        path: String,
        choice: Resolution,
    },
    ReadConflict(String),
    SaveConflict {
        path: String,
        original: String,
        edited: String,
    },
    MoveTo(usize),
    Checkout(crate::checkout::Request),
    Commit(crate::pending::Request),
}
impl Request {
    pub fn is_read_only(&self) -> bool {
        if let Self::OnBranch { request, .. } = self {
            return request.is_read_only();
        }
        matches!(
            self,
            Self::Inspect | Self::PreviewPush | Self::Plan(_) | Self::ReadConflict(_)
        )
    }
}
#[derive(Clone, Debug)]
pub enum Resolution {
    Ours,
    Theirs,
    Mark,
}
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub branch: String,
    pub base: String,
    pub head: String,
    pub commits: Vec<String>,
    pub text: String,
}
#[derive(Clone, Debug, Default)]
pub struct Conflict {
    pub kind: String,
    pub files: Vec<String>,
}
#[derive(Clone, Debug)]
pub struct ConflictPreview {
    pub path: String,
    pub ours: String,
    pub theirs: String,
    pub original: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PushRisk {
    Trivial,
    Rewrite,
    Blocked,
}
#[derive(Clone, Debug)]
pub struct PushPreview {
    pub local_branch: String,
    pub head: String,
    pub remote: String,
    pub branch: String,
    pub url: String,
    pub expected: Option<String>,
    pub risk: PushRisk,
    pub explanation: String,
}

#[derive(Clone, Debug, Default)]
pub struct View {
    pub labels: Vec<String>,
    pub cursor: usize,
    pub boundary: String,
    pub conflict: Option<Conflict>,
    pub plan: Option<Plan>,
    pub push: Option<PushPreview>,
    pub interactive_rebase: bool,
    pub preview: Option<ConflictPreview>,
}
pub struct Outcome {
    pub view: View,
    pub result: Result<String>,
    pub refresh_repository: bool,
    pub refresh_remotes: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Snapshot {
    head: String,
    symbolic: Option<String>,
    refs: BTreeMap<String, String>,
    symbolic_refs: BTreeMap<String, String>,
    config: Vec<u8>,
    worktree_config: Option<Vec<u8>>,
    index_entries: String,
    index: Option<String>,
    index_tree: String,
    tree: String,
}
impl Snapshot {
    fn equivalent(&self, other: &Self) -> bool {
        self.head == other.head
            && self.symbolic == other.symbolic
            && self.refs == other.refs
            && self.config == other.config
            && self.worktree_config == other.worktree_config
            && self.symbolic_refs == other.symbolic_refs
            && self.index_entries == other.index_entries
            && self.index == other.index
            && self.tree == other.tree
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    label: String,
    before: Snapshot,
    after: Snapshot,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Pending {
    label: String,
    before: Snapshot,
}
#[derive(Default, Serialize, Deserialize)]
struct Journal {
    entries: Vec<Entry>,
    cursor: usize,
    boundary: String,
    pending: Option<Pending>,
}
struct Context {
    root: PathBuf,
    git: PathBuf,
    common: PathBuf,
    directory: PathBuf,
}
struct Lock {
    path: PathBuf,
    file: Option<fs::File>,
}
struct RepositoryLock {
    _file: fs::File,
}
impl RepositoryLock {
    fn new(path: &Path) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        Ok(Self {
            _file: options.open(path).map_err(|e| {
                format!("Another Git Rustle operation holds the repository lock: {e}")
            })?,
        })
    }
}
impl Lock {
    fn new(path: PathBuf) -> Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| {
                format!(
                    "Cannot lock {}: {e}. Finish other Git operations first.",
                    path.display()
                )
            })?;
        Ok(Self {
            path,
            file: Some(file),
        })
    }
    fn install(mut self, destination: &Path, bytes: &[u8]) -> Result<()> {
        let file = self.file.as_mut().unwrap();
        file.write_all(bytes).map_err(err)?;
        file.sync_all().map_err(err)?;
        self.file.take();
        fs::rename(&self.path, destination).map_err(err)
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        self.file.take();
        let _ = fs::remove_file(&self.path);
    }
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn output(root: &Path, args: &[&str]) -> Result<String> {
    String::from_utf8(command::checked(root, args)?)
        .map(|s| s.trim().to_string())
        .map_err(err)
}
fn run(
    root: &Path,
    args: &[&str],
    input: Option<&[u8]>,
    env: &[(&str, &OsStr)],
) -> Result<Vec<u8>> {
    let o = command::run_env(root, args, input, Duration::from_secs(300), env)?;
    if o.status.success() {
        Ok(o.stdout)
    } else {
        Err(command::clean(&format!(
            "{} {}",
            String::from_utf8_lossy(&o.stderr),
            String::from_utf8_lossy(&o.stdout)
        )))
    }
}
fn editor_env() -> [(&'static str, &'static OsStr); 2] {
    [
        ("GIT_EDITOR", OsStr::new("true")),
        ("GIT_MERGE_AUTOEDIT", OsStr::new("no")),
    ]
}
impl Context {
    fn new(root: &Path) -> Result<Self> {
        let repo = super::open(root).map_err(err)?;
        let root = repo
            .workdir()
            .ok_or("This operation needs a working tree")?
            .to_path_buf();
        // Git for Windows rejects verbatim (\\?\) paths in GIT_INDEX_FILE.
        let git = repo.git_dir().to_path_buf();
        let common = repo.common_dir().to_path_buf();
        let directory = git.join("rustle-operations");
        fs::create_dir_all(&directory).map_err(err)?;
        Ok(Self {
            root,
            git,
            common,
            directory,
        })
    }
    fn load(&self) -> Result<Journal> {
        match fs::read(self.directory.join("journal.json")) {
            Ok(bytes) => {
                let j: Journal = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("Recovery journal is unreadable: {e}"))?;
                if j.cursor > j.entries.len() {
                    return Err("Invalid recovery cursor".into());
                }
                Ok(j)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Journal::default()),
            Err(e) => Err(err(e)),
        }
    }
    fn save(&self, journal: &Journal) -> Result<()> {
        // This scratch file is exclusively owned under RepositoryLock; a crash
        // may leave it behind, but must not prevent reopening the durable journal.
        let scratch = self.directory.join("journal.json.lock");
        if scratch.exists() {
            fs::remove_file(&scratch).map_err(err)?;
        }
        Lock::new(self.directory.join("journal.json.lock"))?.install(
            &self.directory.join("journal.json"),
            &serde_json::to_vec(journal).map_err(err)?,
        )
    }
    fn snapshot(&self) -> Result<Snapshot> {
        if self.conflict()?.is_some() {
            return Err("Finish or abort the active Git operation first".into());
        }
        let repo = super::open(&self.root).map_err(err)?;
        let h = repo.head().map_err(err)?;
        let head = h.id().map(|x| x.to_string()).unwrap_or_default();
        let symbolic = h.referent_name().map(|n| n.to_string());
        let mut refs = BTreeMap::new();
        let mut symbolic_refs = BTreeMap::new();
        let platform = repo.references().map_err(err)?;
        for reference in platform.all().map_err(err)? {
            let reference = reference.map_err(err)?;
            let name = reference.name().as_bstr().to_str().map_err(err)?;
            if name.starts_with("refs/rustle/") {
                continue;
            }
            if let Some(symbolic) = reference.target().try_name() {
                symbolic_refs.insert(
                    name.into(),
                    symbolic.as_bstr().to_str().map_err(err)?.into(),
                );
            } else if let Some(id) = reference.target().try_id() {
                refs.insert(name.into(), id.to_string());
            }
        }
        let config = fs::read(self.common.join("config")).map_err(err)?;
        let worktree_config = match fs::read(self.git.join("config.worktree")) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(err(e)),
        };
        if output(&self.root, &["config", "--bool", "core.sparseCheckout"]).unwrap_or_default()
            == "true"
        {
            return Err("Recovery checkpoints do not yet support sparse checkouts.".into());
        }
        let mut capture_index = None;
        let index = match fs::metadata(self.git.join("index")) {
            Ok(metadata) => {
                let mut index = gix::index::File::at(
                    self.git.join("index"),
                    gix::hash::Kind::Sha1,
                    false,
                    Default::default(),
                )
                .map_err(err)?;
                if index
                    .entries()
                    .iter()
                    .any(|e| e.mode == gix::index::entry::Mode::COMMIT)
                {
                    return Err("Recovery checkpoints do not yet support submodules. Use Git outside Rustle for this operation.".into());
                }
                if index.entries().iter().any(|entry| {
                    entry.flags.intersects(
                        gix::index::entry::Flags::ASSUME_VALID
                            | gix::index::entry::Flags::SKIP_WORKTREE,
                    )
                }) {
                    return Err("Recovery cannot safely capture files marked assume-unchanged or skip-worktree. Clear these index flags using Git before this operation.".into());
                }
                // Capture with valid stat data so `git add` need not reopen every
                // unchanged file. Preserve the original index timestamp too:
                // making it newer would hide Git's racily-clean detection.
                let mut capture = Vec::new();
                index
                    .write_to(
                        &mut capture,
                        gix::index::write::Options {
                            extensions: gix::index::write::Extensions::None,
                            ..Default::default()
                        },
                    )
                    .map_err(err)?;
                capture_index = Some((capture, metadata.modified().map_err(err)?));
                // Stat caches from an old checkout can falsely report a CRLF
                // conversion as dirty. Preserve staging/flags, not stale stats
                // or filesystem-monitor/tree-cache extensions. Also expands a
                // split index while its shared index is still available.
                for entry in index.entries_mut() {
                    entry.stat = Default::default();
                }
                let mut bytes = Vec::new();
                index
                    .write_to(
                        &mut bytes,
                        gix::index::write::Options {
                            extensions: gix::index::write::Extensions::None,
                            ..Default::default()
                        },
                    )
                    .map_err(err)?;
                Some(bytes)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(err(e)),
        };
        let dirty = !command::checked(
            &self.root,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        )?
        .is_empty();
        let (tree, index_tree) = if !dirty && !head.is_empty() {
            let tree = repo.head_tree_id().map_err(err)?.to_string();
            (tree.clone(), tree)
        } else {
            let temp = self.directory.join("snapshot.index");
            if let Some((bytes, timestamp)) = &capture_index {
                fs::write(&temp, bytes).map_err(err)?;
                OpenOptions::new()
                    .write(true)
                    .open(&temp)
                    .map_err(err)?
                    .set_modified(*timestamp)
                    .map_err(err)?;
            } else {
                let _ = fs::remove_file(&temp);
            }
            let env = [("GIT_INDEX_FILE", temp.as_os_str())];
            let result: Result<(String, String)> = (|| {
                if index.is_none() {
                    run(&self.root, &["read-tree", "--empty"], None, &env)?;
                }
                let staged = run(&self.root, &["write-tree"], None, &env)?;
                run(&self.root, &["add", "-A", "--", "."], None, &env)?;
                let bytes = run(&self.root, &["write-tree"], None, &env)?;
                Ok((
                    String::from_utf8_lossy(&bytes).trim().to_string(),
                    String::from_utf8_lossy(&staged).trim().to_string(),
                ))
            })();
            let _ = fs::remove_file(&temp);
            result?
        };
        let index = index
            .map(|bytes| -> Result<String> {
                let id =
                    gix::objs::compute_hash(gix::hash::Kind::Sha1, gix::objs::Kind::Blob, &bytes)
                        .map_err(err)?
                        .to_string();
                let file = self.directory.join(format!("index-{id}"));
                if !file.exists() {
                    fs::write(file, bytes).map_err(err)?;
                }
                Ok(id)
            })
            .transpose()?;
        // The normalized index already includes every path, object, mode and
        // staging flag; a second `git ls-files` process adds no validation.
        let index_entries = index.clone().unwrap_or_default();
        Ok(Snapshot {
            head,
            symbolic,
            refs,
            symbolic_refs,
            config,
            worktree_config,
            index_entries,
            index,
            tree,
            index_tree,
        })
    }
    fn pin(&self, snapshot: &Snapshot) -> Result<()> {
        let repo = super::open(&self.root).map_err(err)?;
        // Pin working tree plus local commit tips, protecting deleted/rewritten commits from GC.
        let mut parents = BTreeSet::new();
        for (name, oid) in &snapshot.refs {
            if name.starts_with("refs/heads/") || name.starts_with("refs/remotes/") {
                parents.insert(oid.clone());
            }
        }
        if !snapshot.head.is_empty() {
            parents.insert(snapshot.head.clone());
        }
        let identity = "author Git Rustle Recovery <recovery@rustle.invalid> 0 +0000\ncommitter Git Rustle Recovery <recovery@rustle.invalid> 0 +0000\n\nRecovery checkpoint\n";
        let staged = format!("tree {}\n{identity}", snapshot.index_tree);
        let index_commit = repo
            .write_object(
                gix::objs::CommitRef::from_bytes(staged.as_bytes(), gix::hash::Kind::Sha1)
                    .map_err(err)?,
            )
            .map_err(err)?;
        parents.insert(index_commit.to_string());
        let mut body = format!("tree {}\n", snapshot.tree);
        for parent in parents {
            body += &format!("parent {parent}\n");
        }
        body += identity;
        let oid = repo
            .write_object(
                gix::objs::CommitRef::from_bytes(body.as_bytes(), gix::hash::Kind::Sha1)
                    .map_err(err)?,
            )
            .map_err(err)?;
        let edit = gix::refs::transaction::RefEdit::update(
            format!("refs/rustle/checkpoints/{oid}")
                .try_into()
                .map_err(err)?,
            oid.detach(),
            gix::refs::transaction::PreviousValue::Any,
            "Git Rustle recovery checkpoint",
        );
        repo.edit_references_as(
            [edit],
            Some(gix::actor::SignatureRef {
                name: b"Git Rustle Recovery".as_bstr(),
                email: b"recovery@rustle.invalid".as_bstr(),
                time: "0 +0000",
            }),
        )
        .map_err(err)?;
        Ok(())
    }
    fn conflict(&self) -> Result<Option<Conflict>> {
        let kind =
            if self.git.join("rebase-merge").exists() || self.git.join("rebase-apply").exists() {
                "Rebase"
            } else if self.git.join("MERGE_HEAD").exists() {
                "Merge"
            } else if self.git.join("CHERRY_PICK_HEAD").exists() {
                "Cherry-pick"
            } else if self.git.join("REVERT_HEAD").exists() {
                "Revert"
            } else if self.git.join("sequencer").exists() {
                "Sequencer"
            } else {
                ""
            };
        let mut files = BTreeSet::new();
        match fs::metadata(self.git.join("index")) {
            Ok(_) => {
                let index = gix::index::File::at(
                    self.git.join("index"),
                    gix::hash::Kind::Sha1,
                    false,
                    Default::default(),
                )
                .map_err(err)?;
                for entry in index.entries().iter().filter(|e| e.stage_raw() != 0) {
                    files.insert(entry.path(&index).to_str().map_err(err)?.to_owned());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(err(e)),
        }
        if kind.is_empty() && files.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Conflict {
                kind: if kind.is_empty() {
                    "Unmerged index"
                } else {
                    kind
                }
                .into(),
                files: files.into_iter().collect(),
            }))
        }
    }
    fn restore(&self, expected: &Snapshot, target: &Snapshot) -> Result<()> {
        // Validate durable index data before changing any repository state.
        let target_index = target
            .index
            .as_ref()
            .map(|index| -> Result<Vec<u8>> {
                let id = gix::ObjectId::from_hex(index.as_bytes()).map_err(err)?;
                let bytes = fs::read(self.directory.join(format!("index-{id}"))).map_err(err)?;
                let actual =
                    gix::objs::compute_hash(gix::hash::Kind::Sha1, gix::objs::Kind::Blob, &bytes)
                        .map_err(err)?;
                if actual != id {
                    return Err("Recovery index checksum mismatch; nothing was restored.".into());
                }
                Ok(bytes)
            })
            .transpose()?;
        let current = self.snapshot()?;
        if !current.equivalent(expected) {
            return Err("Repository changed outside this history. Undo/redo is invalid; no files were restored.".into());
        }
        if current.symbolic_refs != target.symbolic_refs {
            return Err(
                "Symbolic references changed; this history transition is no longer valid.".into(),
            );
        }
        let worktrees = output(&self.root, &["worktree", "list", "--porcelain"])?;
        let our_root = fs::canonicalize(&self.root).map_err(err)?;
        for block in worktrees.split("\n\n") {
            let location = block.lines().find_map(|l| l.strip_prefix("worktree "));
            let branch = block.lines().find_map(|l| l.strip_prefix("branch "));
            if let (Some(location), Some(branch)) = (location, branch)
                && fs::canonicalize(location).map_err(err)? != our_root
                && (current.refs.get(branch) != target.refs.get(branch)
                    || target.symbolic.as_deref() == Some(branch))
            {
                return Err(format!(
                    "Branch {branch} is checked out in another worktree."
                ));
            }
        }
        let index_lock = Lock::new(self.git.join("index.lock"))?;
        let config_lock = if current.config != target.config {
            Some(Lock::new(self.common.join("config.lock"))?)
        } else {
            None
        };
        let worktree_config_lock = if current.worktree_config != target.worktree_config {
            Some(Lock::new(self.git.join("config.worktree.lock"))?)
        } else {
            None
        };
        let temp = self.directory.join("restore.index");
        let env = [("GIT_INDEX_FILE", temp.as_os_str())];
        let change_tree = current.tree != target.tree;
        if change_tree {
            let added = command::checked(
                &self.root,
                &[
                    "diff-tree",
                    "--no-commit-id",
                    "--name-only",
                    "--diff-filter=A",
                    "--no-renames",
                    "-r",
                    "-z",
                    &current.tree,
                    &target.tree,
                ],
            )?;
            for path in added.split(|b| *b == 0).filter(|p| !p.is_empty()) {
                let path = std::str::from_utf8(path).map_err(err)?;
                let destination = self.root.join(path);
                match fs::symlink_metadata(&destination) {
                    Ok(_) => {
                        return Err(format!(
                            "An ignored or untracked file would be overwritten: {path}. Move it aside before restoring."
                        ));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(err(e)),
                }
                for parent in destination
                    .ancestors()
                    .skip(1)
                    .take_while(|p| *p != self.root)
                {
                    if let Ok(metadata) = fs::symlink_metadata(parent)
                        && (!metadata.is_dir() || metadata.file_type().is_symlink())
                    {
                        return Err(format!(
                            "A file or symlink blocks the restore path: {path}."
                        ));
                    }
                }
            }
            let _ = fs::remove_file(&temp);
            run(&self.root, &["read-tree", &current.tree], None, &env)?;
            run(&self.root, &["update-index", "--refresh"], None, &env)?;
            // Two-tree checkout refuses collisions, including ignored files. Never reset --hard.
            run(
                &self.root,
                &["read-tree", "-m", "-u", &current.tree, &target.tree],
                None,
                &env,
            )?;
        }
        let mut transaction = String::from("start\n");
        for name in current
            .refs
            .keys()
            .chain(target.refs.keys())
            .collect::<BTreeSet<_>>()
        {
            match (current.refs.get(name), target.refs.get(name)) {
                (Some(old), Some(new)) if old != new => {
                    transaction += &format!("update {name} {new} {old}\n")
                }
                (Some(old), None) => transaction += &format!("delete {name} {old}\n"),
                (None, Some(new)) => transaction += &format!("create {name} {new}\n"),
                (Some(old), Some(_)) => transaction += &format!("verify {name} {old}\n"),
                _ => {}
            }
        }
        // Git forbids updating HEAD and its old referent in the same transaction.
        // Reattach after refs commit; the real index lock blocks concurrent checkouts.
        transaction += "prepare\ncommit\n";
        if let Err(error) = run(
            &self.root,
            &["update-ref", "-m", "Git Rustle undo/redo", "--stdin"],
            Some(transaction.as_bytes()),
            &[],
        ) {
            if change_tree
                && let Err(rollback) = run(
                    &self.root,
                    &["read-tree", "-m", "-u", &target.tree, &current.tree],
                    None,
                    &env,
                )
            {
                return Err(format!(
                    "{error}. Worktree rollback failed: {rollback}. Recovery checkpoints retained."
                ));
            }
            return Err(error);
        }
        if current.symbolic != target.symbolic || target.symbolic.is_none() {
            if let Some(reference) = &target.symbolic {
                output(
                    &self.root,
                    &[
                        "symbolic-ref",
                        "-m",
                        "Git Rustle undo/redo",
                        "HEAD",
                        reference,
                    ],
                )?;
            } else {
                output(
                    &self.root,
                    &["update-ref", "--no-deref", "HEAD", &target.head],
                )?;
            }
        }
        let _ = fs::remove_file(&temp);
        if let Some(bytes) = &target_index {
            index_lock.install(&self.git.join("index"), bytes)?;
        } else {
            fs::remove_file(self.git.join("index"))
                .or_else(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        Ok(())
                    } else {
                        Err(e)
                    }
                })
                .map_err(err)?;
        }
        if let Some(lock) = config_lock {
            lock.install(&self.common.join("config"), &target.config)?;
        }
        if let Some(lock) = worktree_config_lock {
            if let Some(bytes) = &target.worktree_config {
                lock.install(&self.git.join("config.worktree"), bytes)?;
            } else {
                fs::remove_file(self.git.join("config.worktree")).map_err(err)?;
            }
        }
        // Restored index stat data describes the checkpoint's old files. Refresh
        // it after checkout/filter conversion without changing the staged blobs.
        // Exit 1 means an intentionally unstaged modification still needs update.
        if target_index.is_some() {
            let refresh = command::run(
                &self.root,
                &["update-index", "--refresh"],
                None,
                Duration::from_secs(30),
            )?;
            if !matches!(refresh.status.code(), Some(0 | 1)) {
                return Err("Restored state, but Git could not refresh index metadata. Refresh the repository before continuing.".into());
            }
        }
        Ok(())
    }
}

fn validate_name(root: &Path, name: &str) -> Result<()> {
    if name.is_empty() || name.starts_with('-') || name == "HEAD" {
        return Err("Enter a valid branch name".into());
    }
    output(root, &["check-ref-format", &format!("refs/heads/{name}")]).map(|_| ())
}
fn revision(root: &Path, target: &str) -> Result<String> {
    if target.trim().is_empty() {
        return Err("Enter a branch, tag or commit".into());
    }
    output(
        root,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{target}^{{commit}}"),
        ],
    )
}
fn clean_worktree(root: &Path) -> Result<()> {
    if !command::checked(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err(
            "Commit or stash your pending changes before merging, pulling or rebasing.".into(),
        );
    }
    if output(root, &["symbolic-ref", "--quiet", "HEAD"]).is_err() {
        return Err("Switch to a local branch first.".into());
    }
    Ok(())
}
fn plan(root: &Path, base: &str) -> Result<Plan> {
    let base = revision(root, base)?;
    let head = revision(root, "HEAD")?;
    let range = format!("{base}..{head}");
    if !output(root, &["rev-list", "--merges", &range])?.is_empty() {
        return Err("This range contains merge commits. Choose a linear range; interactive merge topology is not supported yet.".into());
    }
    let lines = output(root, &["log", "--reverse", "--format=%H%x09%s", &range])?;
    let mut result = Plan {
        branch: output(root, &["symbolic-ref", "--short", "HEAD"])?,
        base,
        head,
        ..Plan::default()
    };
    for line in lines.lines() {
        if let Some((id, subject)) = line.split_once('\t') {
            result.commits.push(id.into());
            result.text += &format!("pick {id} {subject}\n");
        }
    }
    if result.commits.is_empty() {
        return Err("There are no commits to replay above that base.".into());
    }
    Ok(result)
}
fn shell_quote(path: &Path) -> String {
    format!(
        "'{}'",
        path.to_string_lossy()
            .replace('\\', "/")
            .replace('\'', "'\\''")
    )
}
fn interactive(context: &Context, plan: &Plan, text: &str) -> Result<()> {
    if revision(&context.root, "HEAD")? != plan.head
        || output(&context.root, &["symbolic-ref", "--short", "HEAD"])? != plan.branch
    {
        return Err("HEAD changed. Load the rebase plan again.".into());
    }
    let expected: BTreeSet<_> = plan.commits.iter().cloned().collect();
    let mut seen = BTreeSet::new();
    let mut todo = String::new();
    let mut has_previous = false;
    for (i, line) in text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .enumerate()
    {
        let mut fields = line.splitn(3, ' ');
        let action = fields.next().unwrap_or("");
        let id = fields.next().unwrap_or("");
        let message = fields.next().unwrap_or("").trim();
        if !expected.contains(id) || !seen.insert(id.to_string()) {
            return Err("Every original full commit ID must appear exactly once. Use drop to remove a commit.".into());
        }
        if !matches!(
            action,
            "pick" | "reword" | "edit" | "squash" | "fixup" | "drop"
        ) {
            return Err("Allowed actions: pick, reword, edit, squash, fixup, drop. Shell commands are not allowed.".into());
        }
        if matches!(action, "squash" | "fixup") && !has_previous {
            return Err("Squash/fixup needs a preceding retained commit.".into());
        }
        if action != "drop" {
            has_previous = true;
        }
        todo += &format!(
            "{} {id}\n",
            if action == "reword" { "pick" } else { action }
        );
        if action == "reword" {
            if message.is_empty() {
                return Err("Reword needs a new commit message after the ID.".into());
            }
            let file = context.directory.join(format!("message-{i}.txt"));
            fs::write(&file, format!("{message}\n")).map_err(err)?;
            todo += &format!("exec git commit --amend -F {}\n", shell_quote(&file));
        }
    }
    if seen != expected {
        return Err(
            "Every original commit must appear exactly once. Use drop to remove it.".into(),
        );
    }
    let file = context.directory.join("sequence.txt");
    fs::write(&file, todo).map_err(err)?;
    // Git for Windows supplies sh/cat for its editor commands. Only a quoted,
    // app-generated file path reaches this shell; plan text is never executed.
    let editor = format!("cat {} >", shell_quote(&file));
    run(
        &context.root,
        &[
            "-c",
            "rebase.updateRefs=false",
            "rebase",
            "--interactive",
            "--no-autostash",
            "--no-autosquash",
            "--no-fork-point",
            "--no-rebase-merges",
            &plan.base,
        ],
        None,
        &[
            ("GIT_SEQUENCE_EDITOR", OsStr::new(&editor)),
            ("GIT_EDITOR", OsStr::new("true")),
        ],
    )?;
    Ok(())
}

// Resolve the configured push endpoint without changing remote-tracking refs.
// A preview must never silently refresh the lease that protects someone else's work.
fn push_destination(root: &Path) -> Result<(String, String, String, String)> {
    let local = output(root, &["symbolic-ref", "--short", "HEAD"])
        .map_err(|_| "Check out a branch before pushing.".to_string())?;
    // One Git configuration read honors global/local/worktree includes without
    // starting a new process for each fallback. Git normalizes section/key case,
    // preserves subsection (branch) case, and NUL separates values containing LF.
    let configuration = output(root, &["config", "--null", "--list"])?;
    let values: BTreeMap<_, _> = configuration
        .split('\0')
        .filter_map(|entry| entry.split_once('\n'))
        .collect();
    let config = |key: &str| {
        values
            .get(key)
            .filter(|s| !s.is_empty())
            .map(|s| (*s).to_string())
    };
    let upstream = config(&format!("branch.{local}.remote"));
    let remote = config(&format!("branch.{local}.pushremote"))
        .or_else(|| config("remote.pushdefault"))
        .or_else(|| upstream.clone())
        .unwrap_or_else(|| "origin".into());
    if remote.is_empty() || remote.starts_with('-') {
        return Err("Configure a push remote for this branch first.".into());
    }
    let branch = if upstream.as_ref() == Some(&remote) {
        config(&format!("branch.{local}.merge"))
            .and_then(|s| s.strip_prefix("refs/heads/").map(str::to_string))
            .unwrap_or_else(|| local.clone())
    } else {
        local.clone()
    };
    validate_name(root, &branch)?;
    let urls = output(root, &["remote", "get-url", "--push", "--all", &remote])?;
    if urls.lines().count() != 1 || urls.starts_with('-') {
        return Err("Push review requires exactly one push URL for this remote.".into());
    }
    Ok((local, remote, branch, urls))
}
fn preview_push(root: &Path) -> Result<PushPreview> {
    let (local_branch, remote, branch, url) = push_destination(root)?;
    let head = revision(root, "HEAD")?;
    let tracking = output(
        root,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/remotes/{remote}/{branch}"),
        ],
    )
    .ok();
    let advertised = output(
        root,
        &[
            "ls-remote",
            "--refs",
            "--",
            &url,
            &format!("refs/heads/{branch}"),
        ],
    )?;
    let expected = advertised.split_whitespace().next().map(str::to_string);
    let fast_forward = expected.as_ref().is_none_or(|tip| {
        tip == &head || command::checked(root, &["merge-base", "--is-ancestor", tip, &head]).is_ok()
    });
    let (risk, explanation) = if fast_forward {
        (
            PushRisk::Trivial,
            "This is a normal push. No remote commits will be overwritten.".into(),
        )
    } else if tracking == expected {
        (PushRisk::Rewrite, "Your history has diverged. Force push with lease will replace remote history; commits absent locally will disappear from this remote branch. The lease protects the remote tip shown in this review.".into())
    } else {
        (PushRisk::Blocked, "The remote differs from your last fetched version. Force push with lease would be rejected. Overriding it could lose other people's commits. Fetch and review those changes before trying again.".into())
    };
    Ok(PushPreview {
        local_branch,
        head,
        remote,
        branch,
        url,
        expected,
        risk,
        explanation,
    })
}
fn push_reviewed(context: &Context, journal: &mut Journal, review: &PushPreview) -> Result<String> {
    if review.risk == PushRisk::Blocked {
        return Err("Fetch and review the remote before pushing.".into());
    }
    if context.conflict()?.is_some() {
        return Err("Finish the active operation before pushing.".into());
    }
    let destination = push_destination(&context.root)?;
    if destination
        != (
            review.local_branch.clone(),
            review.remote.clone(),
            review.branch.clone(),
            review.url.clone(),
        )
        || revision(&context.root, "HEAD")? != review.head
    {
        return Err("The branch or push destination changed. Open Push to review it again.".into());
    }
    let spec = format!("{}:refs/heads/{}", review.head, review.branch);
    let lease = format!(
        "--force-with-lease=refs/heads/{}:{}",
        review.branch,
        review.expected.as_deref().unwrap_or("")
    );
    // Use the revalidated configured remote so Git also updates its tracking
    // refs (including custom fetch mappings). Never inherit mirror semantics.
    let no_mirror = format!("remote.{}.mirror=false", review.remote);
    let mut args = vec![
        "-c",
        &no_mirror,
        "-c",
        "push.followTags=false",
        "-c",
        "push.useForceIfIncludes=false",
        "push",
        "--porcelain",
        "--no-follow-tags",
        "--recurse-submodules=no",
    ];
    if review.risk == PushRisk::Rewrite {
        args.push(&lease);
    }
    args.extend(["--", &review.remote, &spec]);
    invalidate(
        journal,
        "Push attempted: earlier undo/redo history is sealed.",
    );
    context.save(journal)?;
    command::checked(&context.root, &args)?;
    // Publishing a new branch also makes the toolbar's Pull action usable.
    if output(
        &context.root,
        &[
            "config",
            "--get",
            &format!("branch.{}.remote", review.local_branch),
        ],
    )
    .is_err()
    {
        command::checked(
            &context.root,
            &[
                "config",
                &format!("branch.{}.remote", review.local_branch),
                &review.remote,
            ],
        )?;
        command::checked(
            &context.root,
            &[
                "config",
                &format!("branch.{}.merge", review.local_branch),
                &format!("refs/heads/{}", review.branch),
            ],
        )?;
    }
    Ok(format!(
        "Pushed {} to {}/{}. Earlier history is sealed.",
        review.local_branch, review.remote, review.branch
    ))
}

fn pull(context: &Context) -> Result<()> {
    let branch = output(&context.root, &["symbolic-ref", "--short", "HEAD"])?;
    let remote = output(
        &context.root,
        &["config", "--get", &format!("branch.{branch}.remote")],
    )
    .map_err(|_| "The current branch has no upstream remote.".to_string())?;
    let merge = output(
        &context.root,
        &["config", "--get-all", &format!("branch.{branch}.merge")],
    )
    .map_err(|_| "The current branch has no upstream branch.".to_string())?;
    if merge.lines().count() != 1 || !merge.starts_with("refs/heads/") || remote.starts_with('-') {
        return Err("Pull needs exactly one configured upstream branch.".into());
    }
    let tracking = format!("refs/remotes/{remote}/{}", &merge["refs/heads/".len()..]);
    let target = if remote == "." {
        revision(&context.root, &merge)?
    } else {
        command::checked(
            &context.root,
            &[
                "-c",
                "fetch.writeCommitGraph=false",
                "fetch",
                "--no-tags",
                "--no-prune",
                "--no-prune-tags",
                "--no-recurse-submodules",
                "--no-write-fetch-head",
                "--refmap=",
                "--",
                &remote,
                &format!("+{merge}:{tracking}"),
            ],
        )?;
        revision(&context.root, &tracking)?
    };
    run(
        &context.root,
        &["merge", "--no-edit", "--", &target],
        None,
        &editor_env(),
    )?;
    Ok(())
}
fn resolve(context: &Context, path: &str, choice: &Resolution) -> Result<()> {
    let conflict = context.conflict()?.ok_or("There is no active conflict.")?;
    if !conflict.files.iter().any(|p| p == path) {
        return Err("That file is no longer unresolved. Refresh the conflict list.".into());
    }
    match choice {
        Resolution::Mark => {
            if let Ok(bytes) = fs::read(context.root.join(path))
                && let Ok(text) = std::str::from_utf8(&bytes)
                && text.lines().any(|l| {
                    l.starts_with("<<<<<<< ") || l.starts_with(">>>>>>> ") || l == "======="
                })
            {
                return Err("Conflict markers remain. Edit the file and remove them before marking it resolved.".into());
            }
        }
        Resolution::Ours | Resolution::Theirs => {
            let stage = if matches!(choice, Resolution::Ours) {
                "2"
            } else {
                "3"
            };
            let exists = command::run(
                &context.root,
                &["cat-file", "-e", &format!(":{stage}:{path}")],
                None,
                Duration::from_secs(30),
            )?
            .status
            .success();
            if exists {
                command::checked(
                    &context.root,
                    &[
                        "--literal-pathspecs",
                        "checkout",
                        if stage == "2" { "--ours" } else { "--theirs" },
                        "--",
                        path,
                    ],
                )?;
            } else {
                command::checked(
                    &context.root,
                    &["--literal-pathspecs", "rm", "-f", "--", path],
                )?;
                return Ok(());
            }
        }
    }
    command::checked(
        &context.root,
        &["--literal-pathspecs", "add", "-A", "--", path],
    )?;
    Ok(())
}
fn read_conflict(context: &Context, path: &str) -> Result<ConflictPreview> {
    if !context
        .conflict()?
        .is_some_and(|c| c.files.iter().any(|p| p == path))
    {
        return Err("This file is no longer unresolved.".into());
    }
    let side = |stage| -> Result<String> {
        let out = command::run(
            &context.root,
            &["show", &format!(":{stage}:{path}")],
            None,
            Duration::from_secs(30),
        )?;
        if !out.status.success() {
            return Ok("(File absent on this side)".into());
        }
        if out.stdout.len() > 2 * 1024 * 1024 || out.stdout.contains(&0) {
            return Ok("(Binary or large file: choose a whole side or use your editor)".into());
        }
        Ok(String::from_utf8(out.stdout).unwrap_or_else(|_| "(Not UTF-8: use your editor)".into()))
    };
    let file = context.root.join(path);
    let original = fs::symlink_metadata(&file)
        .ok()
        .filter(|m| m.is_file() && m.len() <= 2 * 1024 * 1024)
        .and_then(|_| fs::read(&file).ok())
        .filter(|b| !b.contains(&0))
        .and_then(|b| String::from_utf8(b).ok());
    Ok(ConflictPreview {
        path: path.into(),
        ours: side(2)?,
        theirs: side(3)?,
        original,
    })
}
fn save_conflict(context: &Context, path: &str, original: &str, edited: &str) -> Result<String> {
    if edited
        .lines()
        .any(|l| l.starts_with("<<<<<<< ") || l.starts_with(">>>>>>> ") || l == "=======")
    {
        return Err("Conflict markers remain. Remove them before saving the resolution.".into());
    }
    let current = read_conflict(context, path)?;
    if current.original.as_deref() != Some(original) {
        return Err("The file changed outside this editor. Reload it before saving.".into());
    }
    let destination = context.root.join(path);
    let parent = fs::canonicalize(destination.parent().ok_or("Invalid file path")?).map_err(err)?;
    if !parent.starts_with(fs::canonicalize(&context.root).map_err(err)?) {
        return Err("Conflict path leaves the working tree.".into());
    }
    fs::write(destination, edited).map_err(err)?;
    resolve(context, path, &Resolution::Mark)?;
    Ok(format!("Saved and resolved {path}."))
}
fn continue_operation(context: &Context, action: &str) -> Result<()> {
    let conflict = context.conflict()?.ok_or("No operation is in progress.")?;
    if action == "--continue" && !conflict.files.is_empty() {
        return Err("Resolve and stage every conflicted file before continuing.".into());
    }
    let verb = match conflict.kind.as_str() {
        "Rebase" => "rebase",
        "Merge" => "merge",
        "Cherry-pick" => "cherry-pick",
        "Revert" => "revert",
        _ => return Err("This operation must be completed using Git in the terminal.".into()),
    };
    if action == "--skip" && verb == "merge" {
        return Err("Merge cannot skip commits. Resolve the files or abort.".into());
    }
    run(&context.root, &[verb, action], None, &editor_env())?;
    Ok(())
}

pub fn execute(root: &Path, request: Request) -> Outcome {
    let mut view = View::default();
    let result = (|| {
        let context = Context::new(root)?;
        let _lock = RepositoryLock::new(&context.common.join("rustle-operation.lock"))?;
        let mut journal = context.load()?;
        let mut loaded_plan = None;
        let mut preview = None;
        let mut push = None;
        let result = match &request {
            Request::OnBranch {
                branch,
                head,
                request,
            } => {
                if output(&context.root, &["symbolic-ref", "--short", "HEAD"])? != *branch
                    || revision(&context.root, "HEAD")? != *head
                {
                    Err("The current branch changed. Open the action again to review its new target.".into())
                } else {
                    execute_inner(&context, &mut journal, request, &mut loaded_plan)
                }
            }
            Request::PreviewPush => preview_push(&context.root).map(|p| {
                push = Some(p);
                "Push review ready.".into()
            }),
            Request::PushReviewed(review) => push_reviewed(&context, &mut journal, review),
            Request::ReadConflict(path) => read_conflict(&context, path).map(|p| {
                preview = Some(p);
                "Conflict versions loaded.".into()
            }),
            Request::SaveConflict {
                path,
                original,
                edited,
            } => save_conflict(&context, path, original, edited),
            _ => execute_inner(&context, &mut journal, &request, &mut loaded_plan),
        };
        let save = context.save(&journal);
        view = View {
            labels: journal.entries.iter().map(|e| e.label.clone()).collect(),
            cursor: journal.cursor,
            boundary: journal.boundary.clone(),
            conflict: context.conflict()?,
            interactive_rebase: context.git.join("rebase-merge/interactive").exists(),
            plan: loaded_plan,
            push,
            preview,
        };
        save?;
        result
    })();
    Outcome {
        view,
        refresh_remotes: result.is_ok() && matches!(request, Request::PushReviewed(_)),
        result,
        refresh_repository: !request.is_read_only(),
    }
}
fn invalidate(journal: &mut Journal, reason: &str) {
    journal.entries.clear();
    journal.cursor = 0;
    journal.pending = None;
    journal.boundary = reason.into();
}
fn record(context: &Context, journal: &mut Journal, pending: Pending) -> Result<()> {
    let after = context.snapshot()?;
    if !after.equivalent(&pending.before) {
        context.pin(&after)?;
        journal.entries.truncate(journal.cursor);
        journal.entries.push(Entry {
            label: pending.label,
            before: pending.before,
            after,
        });
        if journal.entries.len() > 100 {
            journal.entries.remove(0);
            journal.boundary = "Only the latest 100 operations are navigable; older recovery objects are retained.".into();
        }
        journal.cursor = journal.entries.len();
    }
    journal.pending = None;
    Ok(())
}
fn execute_inner(
    context: &Context,
    journal: &mut Journal,
    request: &Request,
    loaded_plan: &mut Option<Plan>,
) -> Result<String> {
    match request {
        Request::Inspect => {
            if context.conflict()?.is_none() {
                if let Some(pending) = journal.pending.clone() {
                    record(context, journal, pending)?;
                }
                if let Some(expected) = expected(journal)
                    && !context.snapshot()?.equivalent(expected)
                {
                    invalidate(
                        journal,
                        "Repository changed outside Git Rustle. Earlier undo/redo is no longer valid.",
                    );
                }
            }
            return Ok("Repository state refreshed.".into());
        }
        Request::Plan(base) => {
            *loaded_plan = Some(plan(&context.root, base)?);
            return Ok("Plan loaded. Reorder lines and choose actions, then start rebase.".into());
        }
        Request::MoveTo(cursor) => {
            if journal.pending.is_some() || context.conflict()?.is_some() {
                return Err("Finish or abort the active operation before undo/redo.".into());
            }
            if *cursor > journal.entries.len() {
                return Err("Invalid history position.".into());
            }
            if *cursor == journal.cursor {
                return Ok("Already at that history position.".into());
            }
            let current = expected(journal).ok_or("History is empty.")?.clone();
            let target = if *cursor == 0 {
                &journal.entries[0].before
            } else {
                &journal.entries[cursor - 1].after
            };
            if !context.snapshot()?.equivalent(&current) {
                invalidate(
                    journal,
                    "Repository changed outside this history; undo/redo was invalidated.",
                );
                return Err(journal.boundary.clone());
            }
            context.restore(&current, target)?;
            journal.cursor = *cursor;
            return Ok("History position restored.".into());
        }
        Request::Resolve { path, choice } => {
            resolve(context, path, choice)?;
            return Ok(format!("Resolved {path}."));
        }
        Request::Continue | Request::Skip | Request::Abort => {
            let action = match request {
                Request::Continue => "--continue",
                Request::Skip => "--skip",
                _ => "--abort",
            };
            let result = continue_operation(context, action);
            if context.conflict()?.is_none() {
                if matches!(request, Request::Abort) && result.is_ok() {
                    journal.pending = None;
                } else if let Some(pending) = journal.pending.clone() {
                    record(context, journal, pending)?;
                } else {
                    invalidate(
                        journal,
                        "An operation started outside Git Rustle finished. Earlier history was invalidated.",
                    );
                }
            }
            result?;
            return Ok(if action == "--abort" {
                "Operation aborted."
            } else {
                "Operation advanced."
            }
            .into());
        }
        _ => {}
    }
    if context.conflict()?.is_some() {
        return Err("Finish or abort the active Git operation first.".into());
    }
    if journal.pending.is_some() {
        return Err("Refresh recovery state before starting another operation.".into());
    }
    let before = context.snapshot()?;
    if let Some(expected) = expected(journal)
        && !before.equivalent(expected)
    {
        invalidate(
            journal,
            "Repository changed outside Git Rustle. A new history starts here.",
        );
    }
    let label = match request {
        Request::Create { name, .. } => format!("Create branch {name}"),
        Request::Delete { name, .. } => format!("Delete branch {name}"),
        Request::Rename { old, new } => format!("Rename {old} to {new}"),
        Request::Pull => "Pull current upstream".into(),
        Request::Merge(target) => format!("Merge {target}"),
        Request::Rebase(target) => format!("Rebase onto {target}"),
        Request::Interactive { .. } => "Interactive rebase".into(),
        Request::Checkout(target) => format!("Checkout {}", target.label()),
        Request::Commit(_) => "Commit selected changes".into(),
        _ => return Err("Unsupported operation".into()),
    };
    context.pin(&before)?;
    journal.pending = Some(Pending {
        label: label.clone(),
        before,
    });
    context.save(journal)?;
    let result: Result<()> = (|| {
        match request {
            Request::Create { name, target } => {
                validate_name(&context.root, name)?;
                let target = revision(&context.root, target)?;
                command::checked(&context.root, &["branch", "--", name, &target])?;
            }
            Request::Delete { name, force } => {
                validate_name(&context.root, name)?;
                command::checked(
                    &context.root,
                    &["branch", if *force { "-D" } else { "-d" }, "--", name],
                )?;
            }
            Request::Rename { old, new } => {
                validate_name(&context.root, old)?;
                validate_name(&context.root, new)?;
                command::checked(&context.root, &["branch", "-m", "--", old, new])?;
            }
            Request::Pull => {
                clean_worktree(&context.root)?;
                pull(context)?;
            }
            Request::Merge(target) | Request::Rebase(target) => {
                clean_worktree(&context.root)?;
                let target = revision(&context.root, target)?;
                let args = if matches!(request, Request::Merge(_)) {
                    vec!["merge", "--no-edit", "--", &target]
                } else {
                    vec![
                        "-c",
                        "rebase.updateRefs=false",
                        "rebase",
                        "--no-autostash",
                        "--no-fork-point",
                        &target,
                    ]
                };
                run(&context.root, &args, None, &editor_env())?;
            }
            Request::Interactive { plan, text } => {
                clean_worktree(&context.root)?;
                interactive(context, plan, text)?;
            }
            Request::Checkout(target) => {
                super::checkout::execute(&context.root, target)?;
            }
            Request::Commit(request) => {
                super::worktree::commit(&context.root, request)?;
            }
            _ => unreachable!(),
        }
        Ok(())
    })();
    if context.conflict()?.is_none() {
        if let Some(pending) = journal.pending.clone() {
            record(context, journal, pending)?;
        }
    } else {
        return Err(format!(
            "{} paused. Resolve files in the Conflicts tab, then continue or abort. {}",
            label,
            result.err().unwrap_or_default()
        ));
    }
    result?;
    Ok(format!("{label} completed."))
}
fn expected(journal: &Journal) -> Option<&Snapshot> {
    if journal.cursor == 0 {
        journal.entries.first().map(|e| &e.before)
    } else {
        journal.entries.get(journal.cursor - 1).map(|e| &e.after)
    }
}

#[cfg(test)]
mod tests;
