//! Read-only branch-base lookup. All reference/object access belongs on workers.
use super::{ObjectId, Path, command};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub id: ObjectId,
}

#[derive(Clone, Debug)]
pub enum Prepared {
    Default(Target),
    Choose(Vec<Target>),
}

#[cfg(test)]
pub fn prepare(root: &Path, selected: ObjectId) -> Result<Prepared, String> {
    prepare_for(root, selected, None, false)
}
pub fn prepare_for(
    root: &Path,
    selected: ObjectId,
    branch: Option<&str>,
    choose: bool,
) -> Result<Prepared, String> {
    let repo = super::open(root).map_err(|e| e.to_string())?;
    // A base comparison needs branch names, frozen IDs and symbolic defaults,
    // not a graph snapshot (tag peeling, tracking metadata or a fingerprint).
    // Read refs once and resolve symbolic branch aliases in this in-memory map.
    let raw = super::refs::raw_refs(&repo)?;
    let by_name: std::collections::HashMap<_, _> = raw
        .iter()
        .map(|reference| (reference.name.as_str(), reference))
        .collect();
    let locals: Vec<_> = raw
        .iter()
        .filter_map(|reference| {
            Some(Target {
                name: reference.name.strip_prefix("refs/heads/")?.to_owned(),
                id: raw_id(reference, &by_name)?,
            })
        })
        .collect();
    let mut targets: Vec<_> = raw
        .iter()
        .filter_map(|reference| {
            let name = reference
                .name
                .strip_prefix("refs/heads/")
                .or_else(|| reference.name.strip_prefix("refs/remotes/"))?;
            if name.ends_with("/HEAD") {
                return None;
            }
            Some(Target {
                name: name.to_owned(),
                id: raw_id(reference, &by_name)?,
            })
        })
        .collect();
    targets.sort_by(|a, b| a.name.cmp(&b.name));
    targets.dedup_by(|a, b| a.name == b.name && a.id == b.id);
    let explicit = branch.filter(|name| {
        locals
            .iter()
            .any(|local| local.name == *name && local.id == selected)
    });
    let mut tips = locals.iter().filter(|local| local.id == selected);
    let first = tips.next();
    let unique_tip = first
        .filter(|_| tips.next().is_none())
        .map(|local| local.name.as_str());
    let exact_branch = explicit.or(unique_tip);
    // Every name at the selected tip describes that same work. In particular,
    // selecting main must not silently compare main against itself.
    targets.retain(|target| target.id != selected);
    if choose {
        if targets.is_empty() {
            return Err("No other base branch is available. Fetch or create one first.".into());
        }
        return Ok(Prepared::Choose(targets));
    }
    if let Some(target) = creation_base(&repo, root, selected, exact_branch, &locals, &targets) {
        return Ok(Prepared::Default(target));
    }
    let mut defaults: Vec<_> = raw
        .iter()
        .filter(|reference| {
            reference.name.starts_with("refs/remotes/") && reference.name.ends_with("/HEAD")
        })
        .filter_map(|reference| reference.symbolic.as_deref()?.strip_prefix("refs/remotes/"))
        .filter_map(|name| targets.iter().find(|target| target.name == name).cloned())
        .collect();
    if let Some(target) = defaults
        .iter()
        .find(|target| target.name.starts_with("origin/"))
    {
        return Ok(Prepared::Default(target.clone()));
    }
    defaults.sort_by_key(|target| target.id);
    defaults.dedup_by_key(|target| target.id);
    if defaults.len() == 1 {
        return Ok(Prepared::Default(defaults.remove(0)));
    }
    // A configured root name also works in repositories without origin or with
    // a non-main default such as trunk, develop or release.
    if let Some(name) = repo.config_snapshot().string("init.defaultBranch") {
        let name = String::from_utf8_lossy(&name).into_owned();
        if let Some(target) = targets.iter().find(|target| target.name == name) {
            return Ok(Prepared::Default(target.clone()));
        }
    }
    for name in ["main", "master", "origin/main", "origin/master"] {
        if let Some(target) = targets.iter().find(|target| target.name == name) {
            return Ok(Prepared::Default(target.clone()));
        }
    }
    let mut seen = std::collections::HashSet::new();
    targets.retain(|target| seen.insert(target.id));
    match targets.len() {
        0 => Err("No base branch is available. Fetch or create a base branch first.".into()),
        1 => Ok(Prepared::Default(targets.remove(0))),
        _ => Ok(Prepared::Choose(targets)),
    }
}

fn raw_id<'a>(
    mut reference: &'a super::refs::RawRef,
    by_name: &std::collections::HashMap<&str, &'a super::refs::RawRef>,
) -> Option<ObjectId> {
    for _ in 0..=by_name.len() {
        if let Some(id) = reference.id {
            return Some(id);
        }
        reference = *by_name.get(reference.symbolic.as_deref()?)?;
    }
    None // A symbolic cycle is not a usable branch target.
}

// Exact-tip comparisons touch one local reflog. Historical selections may use
// bounded local-only evidence; incomplete evidence must never prove uniqueness.
const MAX_LOCAL_LOGS: usize = 1024;
const MAX_LOG_BYTES: usize = 256 * 1024;
const MAX_TOTAL_LOG_BYTES: usize = 2 * 1024 * 1024;
const MAX_LOG_LINES: usize = 4096;
const MAX_LOG_LINE: usize = 16 * 1024;
struct LogEvidence {
    contains: bool,
    source: Option<String>,
}
fn source_name(line: &gix::refs::file::log::LineRef<'_>) -> Option<String> {
    if !line.previous_oid().is_null() {
        return None;
    }
    let message = std::str::from_utf8(line.message.as_ref()).ok()?;
    let source = message.strip_prefix("branch: Created from ")?;
    Some(
        source
            .strip_prefix("refs/heads/")
            .or_else(|| source.strip_prefix("refs/remotes/"))
            .unwrap_or(source)
            .to_owned(),
    )
}
fn local_log(
    repo: &gix::Repository,
    name: &str,
    selected: ObjectId,
    complete: bool,
    remaining: &mut usize,
    lines_left: &mut usize,
) -> Result<Option<LogEvidence>, ()> {
    use std::io::{BufRead, Read};
    let full_name = format!("refs/heads/{name}");
    let reference = repo.find_reference(full_name.as_str()).map_err(|_| ())?;
    let log = reference.log_iter();
    // Local branches are shared refs, including in linked worktrees. Resolve
    // their logs from the public common-dir and namespace accessors; gix's
    // generic reflog_path helper is intentionally crate-private.
    let mut path = log.store.common_dir_resolved().join("logs");
    if let Some(namespace) = &log.store.namespace {
        path.push(namespace.to_path().map_err(|_| ())?);
    }
    path.push(&full_name);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(());
    }
    let file = std::fs::File::open(path).map_err(|_| ())?;
    if !complete {
        // Creation is the oldest record: never read a long branch history to
        // learn its explicitly recorded source.
        let mut reader = std::io::BufReader::new(file).take((MAX_LOG_LINE + 1) as u64);
        let mut bytes = Vec::new();
        reader.read_until(b'\n', &mut bytes).map_err(|_| ())?;
        if bytes.is_empty() {
            return Ok(None);
        }
        if bytes.len() > MAX_LOG_LINE || !bytes.ends_with(b"\n") {
            return Err(());
        }
        let line =
            gix::refs::file::log::LineRef::from_bytes(&bytes[..bytes.len() - 1]).map_err(|_| ())?;
        return Ok(Some(LogEvidence {
            contains: line.new_oid() == selected,
            source: source_name(&line),
        }));
    }
    let limit = MAX_LOG_BYTES.min(*remaining);
    if metadata.len() > limit as u64 {
        return Err(());
    }
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() > limit || (!bytes.is_empty() && !bytes.ends_with(b"\n")) {
        return Err(());
    }
    *remaining -= bytes.len();
    let mut result = LogEvidence {
        contains: false,
        source: None,
    };
    for (index, raw) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
        if *lines_left == 0 || raw.len() > MAX_LOG_LINE {
            return Err(());
        }
        *lines_left -= 1;
        let line =
            gix::refs::file::log::LineRef::from_bytes(&raw[..raw.len() - 1]).map_err(|_| ())?;
        result.contains |= line.new_oid() == selected;
        if index == 0 {
            result.source = source_name(&line);
        }
    }
    Ok(Some(result))
}
fn creation_base(
    repo: &gix::Repository,
    root: &Path,
    selected: ObjectId,
    branch: Option<&str>,
    locals: &[Target],
    targets: &[Target],
) -> Option<Target> {
    if super::refs::is_reftable(repo) {
        return creation_base_reftable(root, selected, branch, targets)
            .ok()
            .flatten();
    }
    if branch.is_none()
        && locals
            .iter()
            .filter(|local| local.id == selected)
            .take(2)
            .count()
            > 1
    {
        return None;
    }
    if repo.refs.namespace.is_none() && !repo.common_dir().join("logs/refs/heads").is_dir() {
        return None;
    }
    let mut remaining = MAX_TOTAL_LOG_BYTES;
    let mut lines_left = MAX_LOG_LINES;
    if let Some(branch) = branch {
        let evidence = local_log(
            repo,
            branch,
            selected,
            false,
            &mut remaining,
            &mut lines_left,
        )
        .ok()??;
        return targets
            .iter()
            .find(|target| Some(&target.name) == evidence.source.as_ref())
            .cloned();
    }
    if locals.len() > MAX_LOCAL_LOGS {
        return None;
    }
    let mut matched = None;
    for local in locals {
        let Some(evidence) = local_log(
            repo,
            &local.name,
            selected,
            true,
            &mut remaining,
            &mut lines_left,
        )
        .ok()?
        else {
            continue;
        };
        if evidence.contains {
            if matched.is_some() {
                return None;
            }
            matched = Some(evidence.source);
        }
    }
    let source = matched.flatten()?;
    targets.iter().find(|target| target.name == source).cloned()
}

// Reftable reflogs are not file-backed. Preserve the existing bounded native
// Git fallback there; exact-tip/local-branch launches remain scoped to one ref.
fn creation_base_reftable(
    root: &Path,
    selected: ObjectId,
    branch: Option<&str>,
    targets: &[Target],
) -> Result<Option<Target>, String> {
    let reference = branch.map(|name| format!("refs/heads/{name}"));
    let mut args = vec![
        "reflog",
        "show",
        "--max-count=4096",
        "--format=%H%x00%gD%x00%gs",
    ];
    if let Some(reference) = &reference {
        args.push(reference);
    } else {
        args.push("--all");
    }
    let Ok(output) = command::run(root, &args, None, std::time::Duration::from_secs(10)) else {
        return Ok(None);
    };
    if !output.status.success() {
        return Ok(None);
    }
    let selected = selected.to_string();
    let mut memberships = std::collections::HashSet::new();
    let mut sources = std::collections::HashMap::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.splitn(3, '\0');
        let (Some(oid), Some(selector), Some(subject)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Some(name) = selector
            .strip_prefix("refs/heads/")
            .and_then(|name| name.split_once("@{").map(|(name, _)| name))
        else {
            continue;
        };
        if oid == selected {
            memberships.insert(name.to_owned());
        }
        if let Some(source) = subject.strip_prefix("branch: Created from ") {
            let source = source
                .strip_prefix("refs/heads/")
                .or_else(|| source.strip_prefix("refs/remotes/"))
                .unwrap_or(source);
            if let Some(target) = targets.iter().find(|target| target.name == source) {
                sources.insert(name.to_owned(), target.clone());
            }
        }
    }
    if let Some(branch) = branch {
        return Ok(sources.remove(branch));
    }
    if memberships.len() == 1 {
        return Ok(sources.remove(memberships.iter().next().unwrap()));
    }
    Ok(None)
}

pub fn resolve(root: &Path, selected: ObjectId, target: &Target) -> Result<ObjectId, String> {
    // Frozen object IDs preserve the reviewed target if refs move meanwhile.
    let output = command::run(
        root,
        &[
            "merge-base",
            "--all",
            &selected.to_string(),
            &target.id.to_string(),
        ],
        None,
        std::time::Duration::from_secs(30),
    )?;
    if !output.status.success() {
        return Err("These commits have no available common ancestor. Fetch missing history or choose another base branch.".into());
    }
    let bases: Vec<_> = output
        .stdout
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|bytes| !bytes.is_empty())
        .map(|bytes| ObjectId::from_hex(bytes).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    match bases.as_slice() {
        [base] => Ok(*base),
        _ => Err("This history has multiple merge bases. Compare explicit commits to choose the intended base.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn git(root: &Path, args: &[&str]) -> String {
        String::from_utf8(command::checked(root, args).unwrap())
            .unwrap()
            .trim()
            .into()
    }
    #[test]
    fn default_base_uses_common_ancestor_not_the_latest_default_tip() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-b", "main"]);
        git(root, &["config", "user.name", "Base"]);
        git(root, &["config", "user.email", "base@example.test"]);
        git(root, &["config", "commit.gpgSign", "false"]);
        std::fs::write(root.join("a"), "base").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "base"]);
        let base = git(root, &["rev-parse", "HEAD"]);
        git(root, &["checkout", "-b", "topic"]);
        std::fs::write(root.join("a"), "topic").unwrap();
        git(root, &["commit", "-am", "topic"]);
        let selected = ObjectId::from_hex(git(root, &["rev-parse", "HEAD"]).as_bytes()).unwrap();
        git(root, &["checkout", "main"]);
        std::fs::write(root.join("b"), "main").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "main"]);
        let Prepared::Default(target) = prepare(root, selected).unwrap() else {
            panic!("main inferred")
        };
        assert_eq!(resolve(root, selected, &target).unwrap().to_string(), base);
        assert_ne!(target.id.to_string(), base);
        assert_eq!(git(root, &["branch", "--show-current"]), "main");
        git(root, &["branch", "-m", "main", "trunk"]);
        git(root, &["branch", "another", &base]);
        assert!(
            matches!(prepare(root,selected).unwrap(),Prepared::Choose(choices) if choices.iter().any(|t|t.name=="trunk") && choices.iter().any(|t|t.name=="another"))
        );
        let missing = Target {
            name: "missing".into(),
            id: ObjectId::from_bytes_or_panic(&[9; 20]),
        };
        assert!(resolve(root, selected, &missing).is_err());
        let tree = git(root, &["rev-parse", "HEAD^{tree}"]);
        let unrelated = git(root, &["commit-tree", &tree, "-m", "unrelated"]);
        assert!(
            resolve(
                root,
                selected,
                &Target {
                    name: "unrelated".into(),
                    id: ObjectId::from_hex(unrelated.as_bytes()).unwrap()
                }
            )
            .is_err()
        );
        let left = git(root, &["commit-tree", &tree, "-p", &base, "-m", "left"]);
        let right = git(root, &["commit-tree", &tree, "-p", &base, "-m", "right"]);
        let merge_a = git(
            root,
            &[
                "commit-tree",
                &tree,
                "-p",
                &left,
                "-p",
                &right,
                "-m",
                "merge-a",
            ],
        );
        let merge_b = git(
            root,
            &[
                "commit-tree",
                &tree,
                "-p",
                &right,
                "-p",
                &left,
                "-m",
                "merge-b",
            ],
        );
        let result = resolve(
            root,
            ObjectId::from_hex(merge_a.as_bytes()).unwrap(),
            &Target {
                name: "criss-cross".into(),
                id: ObjectId::from_hex(merge_b.as_bytes()).unwrap(),
            },
        );
        assert!(result.unwrap_err().contains("multiple merge bases"));
    }

    #[test]
    fn non_origin_remote_default_and_configured_trunk_are_inferred() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-b", "trunk"]);
        git(root, &["config", "user.name", "Base"]);
        git(root, &["config", "user.email", "base@example.test"]);
        git(root, &["config", "commit.gpgSign", "false"]);
        std::fs::write(root.join("base"), "root").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "root"]);
        let base = git(root, &["rev-parse", "HEAD"]);
        git(root, &["checkout", "-b", "topic"]);
        std::fs::write(root.join("work"), "topic").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "topic"]);
        let selected = ObjectId::from_hex(git(root, &["rev-parse", "HEAD"]).as_bytes()).unwrap();
        git(root, &["branch", "main"]); // A misleading conventional name at the selected tip.
        git(root, &["update-ref", "refs/remotes/company/trunk", &base]);
        git(
            root,
            &[
                "symbolic-ref",
                "refs/remotes/company/HEAD",
                "refs/remotes/company/trunk",
            ],
        );
        let Prepared::Default(target) = prepare(root, selected).unwrap() else {
            panic!("company default inferred")
        };
        assert_eq!(target.name, "company/trunk");
        assert_eq!(resolve(root, selected, &target).unwrap().to_string(), base);
        git(
            root,
            &["symbolic-ref", "--delete", "refs/remotes/company/HEAD"],
        );
        git(root, &["config", "init.defaultBranch", "trunk"]);
        let Prepared::Default(target) = prepare(root, selected).unwrap() else {
            panic!("configured root inferred")
        };
        assert_eq!(target.name, "trunk");
        assert_ne!(
            target.id, selected,
            "selected aliases are never inferred as their own base"
        );
        // An earlier selected commit can legitimately be the common ancestor.
        assert_eq!(
            resolve(
                root,
                target.id,
                &Target {
                    name: "topic".into(),
                    id: selected
                }
            )
            .unwrap(),
            target.id
        );
    }

    #[test]
    fn explicit_creation_source_beats_main_and_works_for_older_topic_commits() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-b", "main"]);
        git(root, &["config", "user.name", "Base"]);
        git(root, &["config", "user.email", "base@example.test"]);
        git(root, &["config", "commit.gpgSign", "false"]);
        std::fs::write(root.join("base"), "root").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "root"]);
        git(root, &["checkout", "-b", "lab/base/trunk", "main"]);
        std::fs::write(root.join("trunk"), "trunk work").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "trunk"]);
        let base = git(root, &["rev-parse", "HEAD"]);
        git(
            root,
            &["checkout", "-b", "lab/base/topic", "lab/base/trunk"],
        );
        std::fs::write(root.join("topic"), "first").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "first topic commit"]);
        let earlier = ObjectId::from_hex(git(root, &["rev-parse", "HEAD"]).as_bytes()).unwrap();
        std::fs::write(root.join("topic"), "second").unwrap();
        git(root, &["commit", "-am", "second topic commit"]);
        let selected = ObjectId::from_hex(git(root, &["rev-parse", "HEAD"]).as_bytes()).unwrap();
        git(root, &["checkout", "main"]);
        let Prepared::Default(target) =
            prepare_for(root, selected, Some("lab/base/topic"), false).unwrap()
        else {
            panic!("explicit source inferred")
        };
        assert_eq!(target.name, "lab/base/trunk");
        assert_eq!(resolve(root, selected, &target).unwrap().to_string(), base);
        let Prepared::Default(target) = prepare(root, earlier).unwrap() else {
            panic!("unambiguous historical branch inferred")
        };
        assert_eq!(target.name, "lab/base/trunk");
        assert_eq!(resolve(root, earlier, &target).unwrap().to_string(), base);
        let linked = root.join("linked-topic");
        git(
            root,
            &[
                "worktree",
                "add",
                linked.to_str().unwrap(),
                "lab/base/topic",
            ],
        );
        let Prepared::Default(linked_target) =
            prepare_for(&linked, selected, Some("lab/base/topic"), false).unwrap()
        else {
            panic!("shared local branch creation source")
        };
        assert_eq!(linked_target.name, "lab/base/trunk");
        let Prepared::Default(linked_target) = prepare(&linked, earlier).unwrap() else {
            panic!("shared historical branch source")
        };
        assert_eq!(linked_target.name, "lab/base/trunk");
        git(
            root,
            &["worktree", "remove", "--force", linked.to_str().unwrap()],
        );
        let Prepared::Choose(choices) = prepare_for(root, earlier, None, true).unwrap() else {
            panic!("explicit override always offers choices")
        };
        assert!(choices.iter().any(|target| target.name == "main"));
        assert!(choices.iter().any(|target| target.name == "lab/base/trunk"));
    }

    #[test]
    fn ambiguous_or_incomplete_local_reflogs_fall_back_without_affecting_explicit_branch() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-b", "main"]);
        git(root, &["config", "user.name", "Base"]);
        git(root, &["config", "user.email", "base@example.test"]);
        git(root, &["config", "commit.gpgSign", "false"]);
        std::fs::write(root.join("base"), "root").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "root"]);
        git(root, &["checkout", "-b", "trunk", "main"]);
        std::fs::write(root.join("trunk"), "trunk").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "trunk"]);
        git(root, &["checkout", "-b", "topic", "trunk"]);
        std::fs::write(root.join("topic"), "one").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "one"]);
        let earlier_text = git(root, &["rev-parse", "HEAD"]);
        let earlier = ObjectId::from_hex(earlier_text.as_bytes()).unwrap();
        std::fs::write(root.join("topic"), "two").unwrap();
        git(root, &["commit", "-am", "two"]);
        let tip = ObjectId::from_hex(git(root, &["rev-parse", "HEAD"]).as_bytes()).unwrap();
        git(root, &["branch", "alias", &earlier_text]);
        let Prepared::Default(target) = prepare(root, earlier).unwrap() else {
            panic!("default fallback")
        };
        assert_eq!(
            target.name, "main",
            "two branch reflogs cannot establish a unique historical source"
        );
        git(root, &["branch", "-D", "alias"]);
        git(root, &["branch", "other", "main"]);
        std::fs::OpenOptions::new()
            .append(true)
            .open(root.join(".git/logs/refs/heads/other"))
            .unwrap()
            .write_all(b"incomplete record")
            .unwrap();
        let Prepared::Default(target) = prepare(root, earlier).unwrap() else {
            panic!("default fallback")
        };
        assert_eq!(
            target.name, "main",
            "an uninspected or malformed log cannot prove unique membership elsewhere"
        );
        let Prepared::Default(target) = prepare_for(root, tip, Some("topic"), false).unwrap()
        else {
            panic!("known branch source")
        };
        assert_eq!(
            target.name, "trunk",
            "known identity does not inspect unrelated logs"
        );
        std::fs::OpenOptions::new()
            .append(true)
            .open(root.join(".git/logs/refs/heads/topic"))
            .unwrap()
            .write_all(&vec![b'x'; MAX_LOG_BYTES + 1])
            .unwrap();
        let Prepared::Default(target) = prepare(root, tip).unwrap() else {
            panic!("unique tip source")
        };
        assert_eq!(
            target.name, "trunk",
            "unique local tip only needs its bounded first creation record"
        );
    }

    #[test]
    fn symbolic_base_aliases_resolve_from_one_ref_listing_and_cycles_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-b", "main"]);
        git(root, &["config", "user.name", "Base"]);
        git(root, &["config", "user.email", "base@example.test"]);
        git(root, &["config", "commit.gpgSign", "false"]);
        std::fs::write(root.join("base"), "root").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "root"]);
        let base = git(root, &["rev-parse", "HEAD"]);
        git(
            root,
            &["tag", "-a", "unrelated-tag", "-m", "Not a base branch"],
        );
        git(root, &["checkout", "-b", "topic"]);
        std::fs::write(root.join("topic"), "topic").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "topic"]);
        let selected = ObjectId::from_hex(git(root, &["rev-parse", "HEAD"]).as_bytes()).unwrap();
        git(
            root,
            &[
                "symbolic-ref",
                "refs/remotes/company/alias",
                "refs/heads/main",
            ],
        );
        git(
            root,
            &[
                "symbolic-ref",
                "refs/remotes/company/HEAD",
                "refs/remotes/company/alias",
            ],
        );
        git(
            root,
            &[
                "symbolic-ref",
                "refs/remotes/company/one",
                "refs/remotes/company/two",
            ],
        );
        git(
            root,
            &[
                "symbolic-ref",
                "refs/remotes/company/two",
                "refs/remotes/company/one",
            ],
        );
        let Prepared::Default(target) = prepare(root, selected).unwrap() else {
            panic!("symbolic remote default")
        };
        assert_eq!(target.name, "company/alias");
        assert_eq!(target.id.to_string(), base);
        assert_eq!(resolve(root, selected, &target).unwrap().to_string(), base);
        let Prepared::Choose(choices) = prepare_for(root, selected, None, true).unwrap() else {
            panic!("base choices")
        };
        assert!(choices.iter().any(|target| target.name == "company/alias"));
        assert!(
            choices
                .iter()
                .all(|target| !target.name.contains("unrelated-tag")
                    && !target.name.ends_with("/one")
                    && !target.name.ends_with("/two"))
        );
    }

    #[test]
    #[ignore = "one warmup and fifteen real merge-base and resulting tree-diff samples"]
    fn benchmark_branch_base() {
        let path = std::path::PathBuf::from(
            std::env::var_os("GIT_RUSTLE_BENCH_REPO").expect("repository"),
        );
        let repo = super::super::open(&path).unwrap();
        let selected = std::env::var("GIT_RUSTLE_BENCH_BASE_COMMIT")
            .ok()
            .map(|value| ObjectId::from_hex(value.as_bytes()).unwrap())
            .unwrap_or_else(|| super::super::refs::head(&repo).unwrap().id.unwrap());
        for sample in -1..15 {
            let start = std::time::Instant::now();
            let prepared = prepare(&path, selected).unwrap();
            let target = match prepared {
                Prepared::Default(target) => target,
                Prepared::Choose(mut targets) => targets.remove(0),
            };
            let prepare_ms = start.elapsed().as_secs_f64() * 1000.;
            let start = std::time::Instant::now();
            let base = resolve(&path, selected, &target).unwrap();
            let merge_base_ms = start.elapsed().as_secs_f64() * 1000.;
            let start = std::time::Instant::now();
            let changes = crate::diff::tree_diff(&repo, Some(&base), &selected).unwrap();
            println!(
                "{}",
                serde_json::json!({"sample":sample,"prepareMs":prepare_ms,"mergeBaseMs":merge_base_ms,"treeMs":start.elapsed().as_secs_f64()*1000.,"files":changes.files.len()})
            );
        }
    }
}
