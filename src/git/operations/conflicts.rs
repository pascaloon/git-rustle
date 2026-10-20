//! Conflict metadata and change detection run on workers, independently of the
//! recovery journal. Unchanged polls only inspect bounded operation files and
//! metadata; commit ranges and worktree text are read only after a change.
use super::*;
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock};

const COMMIT_LIMIT: usize = 100;
const OP_FILES: &[&str] = &[
    "HEAD",
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "REBASE_HEAD",
    "ORIG_HEAD",
    "rebase-merge/head-name",
    "rebase-merge/onto",
    "rebase-merge/orig-head",
    "rebase-merge/stopped-sha",
    "rebase-merge/msgnum",
    "rebase-apply/head-name",
    "rebase-apply/onto",
    "rebase-apply/orig-head",
    "rebase-apply/original-commit",
    "rebase-apply/next",
    "sequencer/head",
    "sequencer/todo",
    "rustle-operations/stash-apply",
];

#[derive(Clone, Debug)]
pub struct Summary {
    pub id: gix::ObjectId,
    pub parents: Vec<gix::ObjectId>,
    pub author_name: String,
    pub commit_time: i64,
    pub subject: String,
}

#[derive(Clone, Debug)]
pub struct FileState {
    pub path: String,
    pub unmerged: bool,
    pub exists: bool,
    /// Changed since the file was first shown in this operation.
    pub edited: bool,
    stamp: u64,
}

#[derive(Debug)]
pub enum Refresh {
    Unchanged,
    Changed(Option<Box<Conflict>>),
}

static CACHE: OnceLock<Mutex<BTreeMap<PathBuf, Conflict>>> = OnceLock::new();

fn cache() -> &'static Mutex<BTreeMap<PathBuf, Conflict>> {
    CACHE.get_or_init(Default::default)
}

fn short_file(path: &Path) -> Vec<u8> {
    use std::io::Read;
    fs::File::open(path)
        .map(|file| {
            let mut bytes = Vec::new();
            let _ = file.take(4096).read_to_end(&mut bytes);
            bytes
        })
        .unwrap_or_default()
}

fn stamp(path: &Path) -> u64 {
    metadata_stamp(fs::symlink_metadata(path).ok().as_ref())
}

fn metadata_stamp(metadata: Option<&fs::Metadata>) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    if let Some(meta) = metadata {
        meta.len().hash(&mut hash);
        meta.modified().ok().hash(&mut hash);
        meta.file_type().is_symlink().hash(&mut hash);
        meta.is_file().hash(&mut hash);
    }
    hash.finish()
}

fn fingerprint(context: &Context, paths: &[FileState]) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    for file in OP_FILES {
        file.hash(&mut hash);
        stamp(&context.git.join(file)).hash(&mut hash);
    }
    for directory in ["rebase-merge", "rebase-apply", "sequencer"] {
        stamp(&context.git.join(directory)).hash(&mut hash);
    }
    stamp(&context.git.join("index")).hash(&mut hash);
    stamp(&context.common.join("packed-refs")).hash(&mut hash);
    stamp(&context.common.join("reftable/tables.list")).hash(&mut hash);
    if let Ok(head) = std::str::from_utf8(&short_file(&context.git.join("HEAD")))
        && let Some(name) = head.trim().strip_prefix("ref: ")
        && name.starts_with("refs/")
        && !name.split('/').any(|part| part == "..")
    {
        stamp(&context.common.join(name)).hash(&mut hash);
    }
    for file in paths {
        file.path.hash(&mut hash);
        stamp(&context.root.join(&file.path)).hash(&mut hash);
    }
    hash.finish()
}

fn identity_files(kind: &str) -> &'static [&'static str] {
    match kind {
        "Merge" => &["MERGE_HEAD"],
        "Rebase" => &[
            "REBASE_HEAD",
            "rebase-merge/head-name",
            "rebase-merge/onto",
            "rebase-merge/orig-head",
            "rebase-merge/stopped-sha",
            "rebase-merge/msgnum",
            "rebase-apply/head-name",
            "rebase-apply/onto",
            "rebase-apply/orig-head",
            "rebase-apply/original-commit",
            "rebase-apply/next",
        ],
        "Cherry-pick" => &["CHERRY_PICK_HEAD", "sequencer/head", "sequencer/todo"],
        "Revert" => &["REVERT_HEAD", "sequencer/head", "sequencer/todo"],
        "Sequencer" => &["sequencer/head", "sequencer/todo"],
        "Stash apply" => &["rustle-operations/stash-apply"],
        _ => &[],
    }
}

fn identity(context: &Context, kind: &str, head: &super::super::refs::Head) -> String {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    kind.hash(&mut hash);
    head.id.hash(&mut hash);
    head.symbolic.hash(&mut hash);
    for file in identity_files(kind) {
        file.hash(&mut hash);
        short_file(&context.git.join(file)).hash(&mut hash);
        // The same branch pair can be aborted and restarted without changing
        // either commit ID. New operation-file versions still invalidate an
        // already-open confirmation or editor from the previous operation.
        stamp(&context.git.join(file)).hash(&mut hash);
    }
    format!("{kind}:{:016x}", hash.finish())
}

pub(super) fn operation_identity(context: &Context) -> Result<String> {
    let conflict = context.conflict()?.ok_or("There is no active conflict.")?;
    let repo = super::super::open(&context.root).map_err(err)?;
    let head = super::super::refs::head(&repo)?;
    Ok(identity(context, &conflict.kind, &head))
}

pub fn refresh(root: &Path, previous: &Conflict) -> Result<Refresh> {
    let context = Context::new(root)?;
    if fingerprint(&context, &previous.states) == previous.fingerprint {
        return Ok(Refresh::Unchanged);
    }
    Ok(Refresh::Changed(
        read(&context, Some(previous))?.map(Box::new),
    ))
}

pub fn read_preview(root: &Path, path: &str, identity: &str) -> Result<ConflictPreview> {
    let context = Context::new(root)?;
    let preview = read_conflict(&context, path)?;
    if preview.identity != identity {
        return Err("The active operation changed. Reopen this conflict.".into());
    }
    Ok(preview)
}

pub(super) fn inside_worktree(context: &Context, path: &Path) -> bool {
    path.parent()
        .and_then(|parent| fs::canonicalize(parent).ok())
        .zip(fs::canonicalize(&context.root).ok())
        .is_some_and(|(parent, root)| parent.starts_with(root))
}

pub(super) fn read(context: &Context, previous: Option<&Conflict>) -> Result<Option<Conflict>> {
    let Some(mut conflict) = context.conflict()? else {
        if let Ok(mut entries) = cache().lock() {
            entries.remove(&context.git);
        }
        return Ok(None);
    };
    let repo = super::super::open(&context.root).map_err(err)?;
    let head = super::super::refs::head(&repo)?;
    conflict.identity = identity(context, &conflict.kind, &head);
    let cached = previous
        .cloned()
        .or_else(|| cache().lock().ok()?.get(&context.git).cloned());
    let previous = cached
        .as_ref()
        .filter(|old| old.identity == conflict.identity);
    if let Some(old) = previous.filter(|old| old.metadata_error.is_none()) {
        conflict.incoming = old.incoming.clone();
        conflict.local = old.local.clone();
        conflict.incoming_label = old.incoming_label.clone();
        conflict.local_label = old.local_label.clone();
        conflict.commits_truncated = old.commits_truncated;
        conflict.metadata_error = old.metadata_error.clone();
    } else if let Err(error) = commits(context, &repo, &head, &mut conflict) {
        conflict.metadata_error = Some(error);
    }
    let mut paths: BTreeSet<String> = conflict.files.iter().cloned().collect();
    if let Some(old) = previous {
        paths.extend(old.states.iter().map(|file| file.path.clone()));
    }
    let old_states: BTreeMap<_, _> = previous
        .into_iter()
        .flat_map(|old| old.states.iter())
        .map(|file| (file.path.as_str(), file))
        .collect();
    for path in paths {
        let destination = context.root.join(&path);
        let metadata = fs::symlink_metadata(&destination).ok();
        let current_stamp = metadata_stamp(metadata.as_ref());
        let old = old_states.get(path.as_str()).copied();
        let mut state = if let Some(old) = old.filter(|old| old.stamp == current_stamp) {
            old.clone()
        } else {
            FileState {
                path: path.clone(),
                unmerged: false,
                exists: metadata.is_some(),
                edited: old.is_some(),
                stamp: current_stamp,
            }
        };
        state.unmerged = conflict.files.binary_search(&path).is_ok();
        conflict.states.push(state);
    }
    conflict.fingerprint = fingerprint(context, &conflict.states);
    if let Ok(mut entries) = cache().lock() {
        if !entries.contains_key(&context.git) && entries.len() >= 8 {
            entries.pop_first();
        }
        entries.insert(context.git.clone(), conflict.clone());
    }
    Ok(Some(conflict))
}

fn oid_file(context: &Context, file: &str) -> Vec<String> {
    String::from_utf8_lossy(&short_file(&context.git.join(file)))
        .lines()
        .filter_map(|line| gix::ObjectId::from_hex(line.trim().as_bytes()).ok())
        .map(|id| id.to_string())
        .collect()
}

fn commits(
    context: &Context,
    repo: &gix::Repository,
    head: &super::super::refs::Head,
    conflict: &mut Conflict,
) -> Result<()> {
    let head = head.id.map(|id| id.to_string());
    conflict.local_label = "Local commits".into();
    conflict.incoming_label = "Incoming commits".into();
    let Some(head) = head else { return Ok(()) };
    match conflict.kind.as_str() {
        "Merge" => {
            let incoming = oid_file(context, "MERGE_HEAD");
            if incoming.is_empty() {
                return Err("The incoming merge commit is unavailable.".into());
            }
            // Revision walking can stop exclusion traversal early when imported
            // descendants have much older commit dates than their ancestors.
            // Seed the actual common boundaries explicitly so date skew cannot
            // incorrectly show shared commits as local/incoming-only changes.
            let mut base_args = vec!["merge-base", "--all", head.as_str()];
            base_args.extend(incoming.iter().map(String::as_str));
            let bases = command::run(&context.root, &base_args, None, Duration::from_secs(10))?;
            if !matches!(bases.status.code(), Some(0 | 1)) {
                return Err(command::clean(&String::from_utf8_lossy(&bases.stderr)));
            }
            let boundaries: Vec<_> = String::from_utf8_lossy(&bases.stdout)
                .lines()
                .map(str::to_owned)
                .collect();
            let mut incoming_exclusions = vec![head.clone()];
            incoming_exclusions.extend(boundaries.iter().cloned());
            let mut local_exclusions = incoming.clone();
            local_exclusions.extend(boundaries);
            conflict.incoming = range(
                context,
                repo,
                &incoming,
                &incoming_exclusions,
                &mut conflict.commits_truncated,
            )?;
            conflict.local = range(
                context,
                repo,
                &[head],
                &local_exclusions,
                &mut conflict.commits_truncated,
            )?;
        }
        "Rebase" => {
            conflict.local_label = "Local commit being replayed (index stage 3)".into();
            conflict.incoming_label = "Incoming base and replayed commits (index stage 2)".into();
            let mut replay = oid_file(context, "REBASE_HEAD");
            if replay.is_empty() {
                replay = oid_file(context, "rebase-merge/stopped-sha");
            }
            if replay.is_empty() {
                replay = oid_file(context, "rebase-apply/original-commit");
            }
            conflict.local = summaries(repo, &replay)?;
            conflict.incoming = summaries(repo, &[head])?;
        }
        "Cherry-pick" | "Revert" => {
            let revert = conflict.kind == "Revert";
            conflict.incoming_label = if revert {
                "Commit being reverted (inverse changes)"
            } else {
                "Commit being cherry-picked"
            }
            .into();
            conflict.incoming = summaries(
                repo,
                &oid_file(
                    context,
                    if revert {
                        "REVERT_HEAD"
                    } else {
                        "CHERRY_PICK_HEAD"
                    },
                ),
            )?;
            conflict.local = summaries(repo, &[head])?;
        }
        "Stash apply" => {
            conflict.incoming_label = "Stashed changes (index stage 3)".into();
            conflict.local_label = "Current worktree (index stage 2)".into();
            conflict.incoming =
                summaries(repo, &oid_file(context, "rustle-operations/stash-apply"))?;
            conflict.local = summaries(repo, &[head])?;
        }
        _ => {
            conflict.local_label = "Index stage 2".into();
            conflict.incoming_label = "Index stage 3".into();
            conflict.local = summaries(repo, &[head])?;
        }
    }
    Ok(())
}

fn range(
    context: &Context,
    repo: &gix::Repository,
    include: &[String],
    exclude: &[String],
    truncated: &mut bool,
) -> Result<Vec<Summary>> {
    if include.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec!["rev-list", "--max-count=101", "--topo-order"];
    args.extend(include.iter().map(String::as_str));
    args.push("--not");
    args.extend(exclude.iter().map(String::as_str));
    args.push("--");
    let out = command::run(&context.root, &args, None, Duration::from_secs(10))?;
    if !out.status.success() {
        return Err(command::clean(&String::from_utf8_lossy(&out.stderr)));
    }
    let ids: Vec<_> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    *truncated |= ids.len() > COMMIT_LIMIT;
    summaries(repo, &ids[..ids.len().min(COMMIT_LIMIT)])
}

fn summaries(repo: &gix::Repository, ids: &[String]) -> Result<Vec<Summary>> {
    ids.iter()
        .take(COMMIT_LIMIT)
        .map(|id| {
            let id = gix::ObjectId::from_hex(id.as_bytes()).map_err(err)?;
            let detail = super::super::commit_details(repo, &id).map_err(err)?;
            Ok(Summary {
                id,
                parents: detail.parents,
                author_name: detail.author_name,
                commit_time: detail.commit_time,
                subject: command::clean(detail.message.lines().next().unwrap_or_default()),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_identity_tracks_each_kind_and_same_target_marker_replacement() {
        for (kind, marker) in [
            ("Merge", "MERGE_HEAD"),
            ("Rebase", "rebase-merge/stopped-sha"),
            ("Cherry-pick", "CHERRY_PICK_HEAD"),
            ("Revert", "REVERT_HEAD"),
            ("Stash apply", "rustle-operations/stash-apply"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            command::checked(dir.path(), &["init", "-q", "-b", "main"]).unwrap();
            command::checked(
                dir.path(),
                &[
                    "-c",
                    "user.name=Conflict Test",
                    "-c",
                    "user.email=conflict@example.invalid",
                    "-c",
                    "commit.gpgSign=false",
                    "-c",
                    "core.hooksPath=disabled-test-hooks",
                    "commit",
                    "--allow-empty",
                    "-qm",
                    "Conflict identity fixture",
                ],
            )
            .unwrap();
            let context = Context::new(dir.path()).unwrap();
            let head = output(dir.path(), &["rev-parse", "HEAD"]).unwrap();
            let marker = context.git.join(marker);
            fs::create_dir_all(marker.parent().unwrap()).unwrap();
            fs::write(&marker, format!("{head}\n")).unwrap();
            assert_eq!(context.conflict().unwrap().unwrap().kind, kind);
            let first = operation_identity(&context).unwrap();
            // Aborting/restarting can recreate the same operation marker with
            // identical commit bytes. Its new version must stale old actions.
            let bytes = fs::read(&marker).unwrap();
            fs::remove_file(&marker).unwrap();
            fs::write(&marker, &bytes).unwrap();
            fs::OpenOptions::new()
                .write(true)
                .open(&marker)
                .unwrap()
                .set_times(
                    fs::FileTimes::new()
                        .set_modified(std::time::SystemTime::now() + Duration::from_secs(1)),
                )
                .unwrap();
            assert_eq!(fs::read(&marker).unwrap(), bytes);
            assert_ne!(operation_identity(&context).unwrap(), first, "{kind}");
        }
    }
}
