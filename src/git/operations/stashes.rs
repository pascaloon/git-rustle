//! Frozen stash actions and reflog recovery; listing is explicitly requested.
use super::*;

const MAX_LOG: u64 = 4 * 1024 * 1024;
const MAX_STASHES: usize = 10000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stash {
    pub oid: String,
    pub selector: String,
    pub subject: String,
    pub time: String,
}

pub fn list(root: &Path) -> Result<Vec<Stash>> {
    let text = output(root, &["stash", "list", "--format=%H%x00%gd%x00%gs%x00%ct"])?;
    let mut entries = Vec::new();
    for line in text.lines().filter(|line| !line.is_empty()) {
        let fields: Vec<_> = line.split('\0').collect();
        if fields.len() != 4 || gix::ObjectId::from_hex(fields[0].as_bytes()).is_err() {
            return Err("Invalid stash listing; refresh using Git before continuing.".into());
        }
        entries.push(Stash {
            oid: fields[0].into(),
            selector: fields[1].into(),
            subject: command::clean(fields[2]),
            time: fields[3].into(),
        });
        if entries.len() > MAX_STASHES {
            return Err("Stash list exceeds 10,000 entries.".into());
        }
    }
    Ok(entries)
}

/// Stash worktree versus its original HEAD, plus the optional untracked tree.
/// Immutable blob IDs let the ordinary diff/search workers inspect the result.
#[derive(Clone, Debug)]
pub struct Preview {
    pub base: gix::ObjectId,
    pub oid: gix::ObjectId,
    pub changes: crate::diff::TreeDiff,
    pub details: crate::git::CommitDetails,
}
pub fn preview(root: &Path, frozen: &Stash) -> Result<Preview> {
    let stash = current(root, frozen)?;
    let repo = super::super::open(root).map_err(err)?;
    let oid = gix::ObjectId::from_hex(stash.oid.as_bytes()).map_err(err)?;
    let details = super::super::commit_details(&repo, &oid).map_err(err)?;
    let base = details
        .parents
        .first()
        .copied()
        .ok_or("Stash has no base commit")?;
    let mut changes = crate::diff::tree_diff(&repo, Some(&base), &oid).map_err(err)?;
    if let Some(untracked) = details.parents.get(2) {
        let extra = crate::diff::tree_diff(&repo, None, untracked).map_err(err)?;
        changes.files.extend(extra.files);
        changes.files.sort_by(|a, b| a.path.cmp(&b.path));
        changes.files.dedup_by(|a, b| a.path == b.path);
    }
    Ok(Preview {
        base,
        oid,
        changes,
        details,
    })
}

pub(super) fn create(context: &Context, message: Option<&str>) -> Result<()> {
    if command::checked(
        &context.root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err("There are no pending changes to stash.".into());
    }
    let mut args = vec!["stash", "push", "--include-untracked"];
    if let Some(message) = message.filter(|message| !message.trim().is_empty()) {
        args.extend(["--message", message]);
    }
    command::checked(&context.root, &args).map(|_| ())
}
fn current(root: &Path, frozen: &Stash) -> Result<Stash> {
    list(root)?
        .into_iter()
        .find(|s| s.oid == frozen.oid && s.subject == frozen.subject && s.time == frozen.time)
        .ok_or_else(|| {
            "This stash changed or was removed. Refresh the stash list and try again.".into()
        })
}

pub(super) fn marker(context: &Context) -> PathBuf {
    context.directory.join("stash-apply")
}

pub(super) fn apply(context: &Context, frozen: &Stash) -> Result<()> {
    let stash = current(&context.root, frozen)?;
    if !command::checked(
        &context.root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err("Commit or stash pending changes before applying a stash.".into());
    }
    fs::write(marker(context), stash.oid.as_bytes()).map_err(err)?;
    let result = command::checked(&context.root, &["stash", "apply", &stash.oid]).map(|_| ());
    if unresolved(context)?.is_empty() {
        fs::remove_file(marker(context)).map_err(err)?;
    }
    result
}

pub(super) fn drop_stash(context: &Context, frozen: &Stash) -> Result<()> {
    let stash = current(&context.root, frozen)?;
    command::checked(&context.root, &["stash", "drop", &stash.selector]).map(|_| ())
}

pub(super) fn unresolved(context: &Context) -> Result<Vec<String>> {
    let path = context.git.join("index");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let index = gix::index::File::at(path, gix::hash::Kind::Sha1, false, Default::default())
        .map_err(err)?;
    let mut paths = BTreeSet::new();
    for entry in index.entries().iter().filter(|e| e.stage_raw() != 0) {
        paths.insert(entry.path(&index).to_str().map_err(err)?.to_string());
    }
    Ok(paths.into_iter().collect())
}

pub(super) fn finish(context: &Context, pending: &Pending, abort: bool) -> Result<()> {
    if abort {
        // A fresh handle observes externally changed symbolic/packed or
        // detached HEAD. Native peeling retains HEAD^{commit} semantics while
        // avoiding a subprocess on the common files reference backend.
        let repo = super::super::open(&context.root).map_err(err)?;
        let head = if super::super::refs::is_reftable(&repo) {
            revision(&context.root, "HEAD")?
        } else {
            let commit = repo.head_commit().map_err(err)?;
            commit.decode().map_err(err)?;
            commit.id.to_string()
        };
        if head != pending.before.head {
            return Err("HEAD changed outside the stash apply. Restore it before aborting.".into());
        }
        // Git's merge reset refuses overlapping edits; no hard-reset fallback.
        command::checked(
            &context.root,
            &["reset", "--merge", "--quiet", &pending.before.head],
        )?;
    } else if !unresolved(context)?.is_empty() {
        return Err(
            "Resolve and stage every conflicted file before finishing the stash apply.".into(),
        );
    }
    fs::remove_file(marker(context)).map_err(err)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(super) enum Log {
    Files(Vec<u8>),
    Reftable(Vec<LogEntry>),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct LogEntry {
    oid: String,
    name: String,
    email: String,
    date: String,
    message: String,
}

fn has_log(context: &Context) -> Result<bool> {
    let result = command::run(
        &context.root,
        &["reflog", "exists", "refs/stash"],
        None,
        Duration::from_secs(30),
    )?;
    match result.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(command::clean(&String::from_utf8_lossy(&result.stderr))),
    }
}

pub(super) fn read_log(context: &Context, repo: &gix::Repository) -> Result<Option<Log>> {
    if super::super::refs::is_reftable(repo) {
        if !has_log(context)? {
            return Ok(None);
        }
        let text = output(
            &context.root,
            &[
                "reflog",
                "show",
                "--date=raw",
                "--format=%H%x00%gn%x00%ge%x00%gD%x00%gs",
                "refs/stash",
            ],
        )?;
        let mut entries = Vec::new();
        for line in text.lines().filter(|s| !s.is_empty()) {
            let f: Vec<_> = line.split('\0').collect();
            if f.len() != 5 {
                return Err("Invalid stash reflog.".into());
            }
            let date = f[3]
                .split_once("@{")
                .and_then(|(_, d)| d.strip_suffix('}'))
                .ok_or("Invalid stash reflog date.")?;
            entries.push(LogEntry {
                oid: f[0].into(),
                name: f[1].into(),
                email: f[2].into(),
                date: format!("@{date}"),
                message: f[4].into(),
            });
            if entries.len() > MAX_STASHES {
                return Err("Stash reflog exceeds 10,000 entries.".into());
            }
        }
        entries.reverse();
        return Ok((!entries.is_empty()).then_some(Log::Reftable(entries)));
    }
    let path = context.common.join("logs/refs/stash");
    let meta = match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        result => result.map_err(err)?,
    };
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_LOG {
        return Err("Stash reflog is not a bounded regular file.".into());
    }
    Ok(Some(Log::Files(fs::read(path).map_err(err)?)))
}

impl Log {
    pub(super) fn ids(&self) -> Result<Vec<String>> {
        match self {
            Self::Reftable(entries) => Ok(entries.iter().map(|e| e.oid.clone()).collect()),
            Self::Files(bytes) => bytes
                .split(|b| *b == b'\n')
                .filter(|line| !line.is_empty())
                .map(|line| {
                    let oid = line
                        .split(|b| *b == b' ')
                        .nth(1)
                        .ok_or("Invalid stash reflog.")?;
                    gix::ObjectId::from_hex(oid)
                        .map(|id| id.to_string())
                        .map_err(err)
                })
                .collect(),
        }
    }
}

pub(super) fn restore_log(
    context: &Context,
    old: &Option<Log>,
    target: &Option<Log>,
) -> Result<()> {
    if old == target {
        return Ok(());
    }
    if matches!(old, Some(Log::Reftable(_))) || matches!(target, Some(Log::Reftable(_))) {
        // Recreating refs/stash after deleting its last entry does not create
        // a reflog with Git's default logAllRefUpdates policy. Writing the first
        // restored entry creates it; expiring a missing log is an error.
        if has_log(context)? {
            command::checked(
                &context.root,
                &[
                    "reflog",
                    "expire",
                    "--expire=all",
                    "--expire-unreachable=all",
                    "refs/stash",
                ],
            )?;
        }
        if let Some(Log::Reftable(entries)) = target {
            let mut previous = "0".repeat(40);
            for entry in entries {
                let env = [
                    ("GIT_COMMITTER_NAME", OsStr::new(&entry.name)),
                    ("GIT_COMMITTER_EMAIL", OsStr::new(&entry.email)),
                    ("GIT_COMMITTER_DATE", OsStr::new(&entry.date)),
                ];
                run(
                    &context.root,
                    &[
                        "reflog",
                        "write",
                        "refs/stash",
                        &previous,
                        &entry.oid,
                        &entry.message,
                    ],
                    None,
                    &env,
                )?;
                previous = entry.oid.clone();
            }
        }
        return Ok(());
    }
    let path = context.common.join("logs/refs/stash");
    if let Some(Log::Files(bytes)) = target {
        fs::create_dir_all(path.parent().unwrap()).map_err(err)?;
        Lock::new(path.with_extension("lock"))?.install(&path, bytes)?;
    } else {
        match fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(err(e)),
            _ => {}
        }
    }
    Ok(())
}
