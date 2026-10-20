//! Reference storage compatibility. Objects stay in gitoxide; only reftable
//! references use native Git until gitoxide supports that storage backend.
use super::command;
use gix::ObjectId;
use gix::bstr::ByteSlice;
use std::path::Path;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    pub id: Option<ObjectId>,
    pub branch: Option<String>,
    pub symbolic: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawRef {
    pub name: String,
    /// Raw object id, preserving annotated tag objects for recovery.
    pub id: Option<ObjectId>,
    pub symbolic: Option<String>,
    pub peeled: Option<ObjectId>,
}

pub fn is_reftable(repo: &gix::Repository) -> bool {
    repo.config_snapshot()
        .string("extensions.refStorage")
        .is_some_and(|value| value.as_slice().eq_ignore_ascii_case(b"reftable"))
}

fn root(repo: &gix::Repository) -> &Path {
    repo.workdir().unwrap_or_else(|| repo.git_dir())
}

fn checked(path: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = command::run(path, args, None, Duration::from_secs(30))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(command::clean(&String::from_utf8_lossy(&out.stderr)))
    }
}

fn utf8(bytes: &[u8]) -> Result<&str, String> {
    std::str::from_utf8(bytes).map_err(|_| "Git returned a non-UTF-8 reference name".into())
}

pub fn head(repo: &gix::Repository) -> Result<Head, String> {
    if !is_reftable(repo) {
        let value = repo.head().map_err(|e| e.to_string())?;
        return Ok(Head {
            id: value.id().map(|id| id.detach()),
            branch: value.referent_name().map(|name| name.shorten().to_string()),
            symbolic: value.referent_name().map(|name| name.to_string()),
        });
    }
    let path = root(repo);
    let symbolic = command::run(
        path,
        &["symbolic-ref", "--quiet", "HEAD"],
        None,
        Duration::from_secs(30),
    )?;
    let symbolic = match symbolic.status.code() {
        Some(0) => Some(utf8(&symbolic.stdout)?.trim_end().to_owned()),
        Some(1) => None,
        _ => return Err(command::clean(&String::from_utf8_lossy(&symbolic.stderr))),
    };
    let oid = command::run(
        path,
        &["rev-parse", "--verify", "HEAD"],
        None,
        Duration::from_secs(30),
    )?;
    let id = if oid.status.success() {
        Some(ObjectId::from_hex(oid.stdout.trim()).map_err(|e| e.to_string())?)
    } else if symbolic
        .as_ref()
        .is_some_and(|name| raw_refs(repo).is_ok_and(|refs| !refs.iter().any(|r| &r.name == name)))
    {
        None // An unborn branch has a symbolic HEAD but no object.
    } else {
        return Err(command::clean(&String::from_utf8_lossy(&oid.stderr)));
    };
    let branch = symbolic
        .as_deref()
        .and_then(|name| name.strip_prefix("refs/heads/"))
        .map(str::to_owned);
    Ok(Head {
        id,
        branch,
        symbolic,
    })
}

pub fn raw_refs(repo: &gix::Repository) -> Result<Vec<RawRef>, String> {
    if !is_reftable(repo) {
        let platform = repo.references().map_err(|e| e.to_string())?;
        return platform
            .all()
            .map_err(|e| e.to_string())?
            .map(|reference| {
                let reference = reference.map_err(|e| e.to_string())?;
                Ok(RawRef {
                    name: reference
                        .name()
                        .as_bstr()
                        .to_str()
                        .map_err(|e| e.to_string())?
                        .into(),
                    id: reference.target().try_id().map(ToOwned::to_owned),
                    symbolic: reference
                        .target()
                        .try_name()
                        .map(|n| n.as_bstr().to_str().map(str::to_owned))
                        .transpose()
                        .map_err(|e| e.to_string())?,
                    peeled: None,
                })
            })
            .collect();
    }
    let bytes = checked(
        root(repo),
        &[
            "for-each-ref",
            "--format=%(refname)%00%(objectname)%00%(*objectname)%00%(symref)",
        ],
    )?;
    parse_refs(&bytes)
}

fn parse_refs(bytes: &[u8]) -> Result<Vec<RawRef>, String> {
    bytes
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            let fields: Vec<_> = line.split(|&b| b == 0).collect();
            if fields.len() != 4 || !fields[0].starts_with(b"refs/") {
                return Err("Git returned an invalid reference record".into());
            }
            let oid = |bytes: &[u8]| {
                if bytes.is_empty() {
                    Ok(None)
                } else {
                    ObjectId::from_hex(bytes)
                        .map(Some)
                        .map_err(|e| e.to_string())
                }
            };
            Ok(RawRef {
                name: utf8(fields[0])?.into(),
                id: oid(fields[1])?,
                peeled: oid(fields[2])?,
                symbolic: if fields[3].is_empty() {
                    None
                } else {
                    Some(utf8(fields[3])?.into())
                },
            })
        })
        .collect()
}

pub fn tree_for_head(repo: &gix::Repository) -> Result<ObjectId, String> {
    if !is_reftable(repo) {
        return repo
            .head_tree_id()
            .map(|id| id.detach())
            .map_err(|e| e.to_string());
    }
    let id = head(repo)?.id.ok_or("The repository has no HEAD commit")?;
    repo.find_object(id)
        .and_then(|object| object.peel_to_tree())
        .map(|tree| tree.id)
        .map_err(|e| e.to_string())
}

pub fn protect(repo: &gix::Repository, name: &str, id: &str, message: &str) -> Result<(), String> {
    let oid = ObjectId::from_hex(id.as_bytes()).map_err(|e| e.to_string())?;
    if !is_reftable(repo) {
        let reference: gix::refs::FullName = name.try_into().map_err(|e| format!("{e}"))?;
        let edit = gix::refs::transaction::RefEdit::update(
            reference,
            oid,
            gix::refs::transaction::PreviousValue::Any,
            message,
        );
        return repo
            .edit_references_as(
                [edit],
                Some(gix::actor::SignatureRef {
                    name: b"Git Rustle Recovery".as_bstr(),
                    email: b"recovery@rustle.invalid".as_bstr(),
                    time: "0 +0000",
                }),
            )
            .map(|_| ())
            .map_err(|e| e.to_string());
    }
    checked(
        root(repo),
        &[
            "-c",
            "user.name=Git Rustle Recovery",
            "-c",
            "user.email=recovery@rustle.invalid",
            "update-ref",
            "-m",
            message,
            name,
            &oid.to_string(),
        ],
    )
    .map(|_| ())
}

/// Windows' files backend aliases case-only names. Refuse before any fetch,
/// branch creation or update; conversion is a separate explicit user action.
pub fn ensure_case_safe(path: &Path, names: &[&str]) -> Result<(), String> {
    if !cfg!(windows) {
        return Ok(());
    }
    let repo = super::open(path).map_err(|e| e.to_string())?;
    if is_reftable(&repo) {
        return Ok(());
    }
    let refs = raw_refs(&repo)?;
    for name in names {
        if let Some(other) = refs.iter().find(|r| aliases_case(&r.name, name)) {
            return Err(format!(
                "Git's files reference storage cannot safely distinguish '{name}' from '{}'. Convert this repository to reftable in Settings > Project before continuing.",
                other.name
            ));
        }
    }
    Ok(())
}

fn aliases_case(existing: &str, requested: &str) -> bool {
    for (a, b) in existing.split('/').zip(requested.split('/')) {
        if a == b {
            continue;
        }
        // A shared directory with different casing is unsafe too: creating
        // feature/b beside Feature/a silently changes the displayed ref name.
        return a.eq_ignore_ascii_case(b)
            || ((!a.is_ascii() || !b.is_ascii()) && a.to_uppercase() == b.to_uppercase());
    }
    false
}

#[derive(Clone, Debug)]
pub struct BackendInfo {
    pub reftable: bool,
    pub git_version: String,
    pub can_migrate: bool,
}

pub fn inspect_backend(path: &Path) -> Result<BackendInfo, String> {
    let repo = super::open(path).map_err(|e| e.to_string())?;
    let git_version = utf8(&checked(path, &["--version"])?)?.trim().to_owned();
    let help = command::run(
        path,
        &["refs", "migrate", "-h"],
        None,
        Duration::from_secs(30),
    )?;
    let can_migrate = [&help.stdout, &help.stderr].into_iter().any(|bytes| {
        bytes
            .windows(b"--ref-format".len())
            .any(|part| part == b"--ref-format")
    });
    Ok(BackendInfo {
        reftable: is_reftable(&repo),
        git_version,
        can_migrate,
    })
}

/// Worker-only, explicitly requested conversion. Git keeps the reflogs. Never
/// called by discovery, opening, rendering, or fetching.
pub fn migrate_reftable(path: &Path) -> Result<super::RefSnapshot, String> {
    let _guard = super::operations::ensure_storage_migration_allowed(path)?;
    let info = inspect_backend(path)?;
    if !info.can_migrate {
        return Err(
            "This Git version cannot migrate reference storage. Update Git for Windows first."
                .into(),
        );
    }
    let repo = super::open(path).map_err(|e| e.to_string())?;
    if info.reftable {
        return super::snapshot_refs(&repo).map_err(|e| e.to_string());
    }
    let before_head = head(&repo)?;
    let before = identity(raw_refs(&repo)?);
    checked(path, &["refs", "migrate", "--ref-format=reftable"])?;
    let repo = super::open(path)
        .map_err(|e| format!("Reference conversion finished, but reopening failed: {e}"))?;
    if !is_reftable(&repo) || head(&repo)? != before_head || identity(raw_refs(&repo)?) != before {
        return Err("Reference conversion finished, but reference validation failed. Inspect the repository with Git before continuing.".into());
    }
    super::snapshot_refs(&repo).map_err(|e| e.to_string())
}

fn identity(
    refs: Vec<RawRef>,
) -> std::collections::BTreeMap<String, (Option<ObjectId>, Option<String>)> {
    refs.into_iter()
        .map(|r| {
            // Native Git exposes the resolved id for symbolic references; gix's
            // raw iterator exposes only their symbolic target.
            (
                r.name,
                (if r.symbolic.is_some() { None } else { r.id }, r.symbolic),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        aliases_case, head, identity, inspect_backend, is_reftable, migrate_reftable, parse_refs,
        protect, raw_refs, tree_for_head,
    };
    use crate::git::{self, RefKind};
    use std::path::Path;

    fn git(path: &Path, args: &[&str]) -> String {
        String::from_utf8(git::command::checked(path, args).unwrap())
            .unwrap()
            .trim_end()
            .to_owned()
    }

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-b", "main"]);
        for (key, value) in [
            ("user.name", "Reference Test"),
            ("user.email", "ref@example.test"),
            ("commit.gpgSign", "false"),
            ("core.autocrlf", "false"),
            ("core.hooksPath", "disabled-test-hooks"),
        ] {
            git(dir.path(), &["config", key, value]);
        }
        std::fs::write(dir.path().join("file.txt"), "first\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-m", "first"]);
        dir
    }

    #[test]
    fn reftable_object_access_case_distinct_refs_and_head() {
        let dir = fixture();
        let path = dir.path();
        let first = git(path, &["rev-parse", "HEAD"]);
        let first_id = gix::ObjectId::from_hex(first.as_bytes()).unwrap();
        std::fs::write(path.join("file.txt"), "second\n").unwrap();
        git(path, &["commit", "-am", "second"]);
        let second = git(path, &["rev-parse", "HEAD"]);
        git(path, &["tag", "-a", "inner", "-m", "inner", &first]);
        git(path, &["tag", "-a", "outer", "-m", "outer", "inner"]);
        let original = git::open(path).unwrap();
        let before = identity(raw_refs(&original).unwrap());
        let snapshot = migrate_reftable(path).unwrap();
        assert!(
            snapshot
                .refs
                .iter()
                .any(|r| r.name == "outer" && r.id == first_id)
        );
        let repo = git::open(path).unwrap(); // Proves gix opens reftable object stores.
        assert!(is_reftable(&repo));
        assert_eq!(identity(raw_refs(&repo).unwrap()), before);
        assert_eq!(
            git::commit_details(&repo, &snapshot.head_id.unwrap())
                .unwrap()
                .message
                .lines()
                .next(),
            Some("second")
        );
        assert_eq!(
            tree_for_head(&repo).unwrap().to_string(),
            git(path, &["rev-parse", "HEAD^{tree}"])
        );
        git(path, &["branch", "Topic", &first]);
        git(path, &["branch", "topic", &second]);
        let repo = git::open(path).unwrap();
        let snapshot = git::snapshot_refs(&repo).unwrap();
        for (name, sha) in [("Topic", &first), ("topic", &second)] {
            let expected_id = gix::ObjectId::from_hex(sha.as_bytes()).unwrap();
            assert!(
                snapshot
                    .refs
                    .iter()
                    .any(|r| r.name == name && r.id == expected_id)
            );
            crate::git::checkout::checkout(
                path,
                &crate::branches::Target {
                    name: name.into(),
                    remote: None,
                    url: None,
                    sha: sha.clone(),
                },
            )
            .unwrap();
            assert_eq!(
                head(&git::open(path).unwrap()).unwrap().branch.as_deref(),
                Some(name)
            );
            assert_eq!(git(path, &["rev-parse", "HEAD"]), *sha);
        }
        git(path, &["switch", "--detach", &first]);
        let detached = git::snapshot_refs(&git::open(path).unwrap()).unwrap();
        assert!(detached.head_branch.is_none());
        assert!(
            detached
                .refs
                .iter()
                .any(|r| r.name == "HEAD" && r.kind == RefKind::Head)
        );
        protect(
            &git::open(path).unwrap(),
            "refs/rustle/checkpoints/test",
            &first,
            "test recovery",
        )
        .unwrap();
        assert_eq!(
            git(path, &["rev-parse", "refs/rustle/checkpoints/test"]),
            first
        );
    }

    #[test]
    fn migration_preserves_reflogs_and_dirty_index_worktree_and_upstreams() {
        let dir = fixture();
        let path = dir.path();
        git(
            path,
            &["remote", "add", "origin", "https://example.test/repo.git"],
        );
        git(path, &["config", "branch.main.remote", "origin"]);
        git(
            path,
            &["config", "branch.main.merge", "refs/heads/CaseSensitive"],
        );
        std::fs::write(path.join("file.txt"), "stash\n").unwrap();
        git(path, &["stash", "push", "-m", "retained stash"]);
        std::fs::write(path.join("file.txt"), "staged\n").unwrap();
        git(path, &["add", "file.txt"]);
        std::fs::write(path.join("file.txt"), "unstaged\n").unwrap();
        let status = git(path, &["status", "--porcelain=v1"]);
        let logs = git(path, &["reflog", "show", "--all", "--format=%H %gs"]);
        let mut sorted_logs: Vec<_> = logs.lines().collect();
        sorted_logs.sort();
        let snapshot = migrate_reftable(path).unwrap();
        assert!(snapshot.tracking["origin"].contains("CaseSensitive"));
        assert_eq!(git(path, &["status", "--porcelain=v1"]), status);
        let logs = git(path, &["reflog", "show", "--all", "--format=%H %gs"]);
        let mut after: Vec<_> = logs.lines().collect();
        after.sort();
        assert_eq!(sorted_logs, after);
        assert_eq!(
            std::fs::read_to_string(path.join("file.txt")).unwrap(),
            "unstaged\n"
        );
        assert_eq!(git(path, &["show", ":file.txt"]), "staged");
        assert!(inspect_backend(path).unwrap().reftable);
        assert_eq!(
            migrate_reftable(path).unwrap().fingerprint,
            snapshot.fingerprint
        );
    }

    #[test]
    fn migration_refuses_active_git_and_stash_operations_before_changing_storage() {
        let dir = fixture();
        let path = dir.path();
        let config = std::fs::read(path.join(".git/config")).unwrap();
        let before = git(path, &["show-ref"]);
        for marker in [
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "sequencer",
            "rebase-merge",
            "rebase-apply",
            "rustle-operations/stash-apply",
        ] {
            let marker = path.join(".git").join(marker);
            std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
            std::fs::write(&marker, b"pending operation").unwrap();
            assert!(migrate_reftable(path).is_err());
            assert_eq!(std::fs::read(path.join(".git/config")).unwrap(), config);
            assert_eq!(git(path, &["show-ref"]), before);
            std::fs::remove_file(marker).unwrap();
        }
        migrate_reftable(path).unwrap();
        assert!(is_reftable(&git::open(path).unwrap()));
    }

    #[test]
    fn unborn_reftable_head_and_config_only_upstream_changes() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "--ref-format=reftable", "-b", "new"]);
        let snapshot = git::snapshot_refs(&git::open(dir.path()).unwrap()).unwrap();
        assert!(snapshot.head_id.is_none() && snapshot.refs.is_empty());
        assert_eq!(snapshot.head_branch.as_deref(), Some("new"));
        let dir = fixture();
        let before = git::snapshot_refs(&git::open(dir.path()).unwrap()).unwrap();
        git(dir.path(), &["config", "branch.main.remote", "origin"]);
        git(
            dir.path(),
            &["config", "branch.main.merge", "refs/heads/main"],
        );
        let after = git::snapshot_refs(&git::open(dir.path()).unwrap()).unwrap();
        assert_ne!(before.fingerprint, after.fingerprint);
        assert!(after.tracking["origin"].contains("main"));
    }

    #[test]
    fn native_reference_parser_rejects_partial_invalid_output() {
        assert!(parse_refs(b"refs/heads/main\0bad\0\0\n").is_err());
        assert!(parse_refs(b"refs/heads/main\0\n").is_err());
        let refs =
            parse_refs(b"refs/heads/main\0abcdef0123456789abcdef0123456789abcdef01\0\0\n").unwrap();
        assert_eq!(refs.len(), 1);
        assert!(refs[0].symbolic.is_none());
    }

    #[test]
    fn files_case_guard_includes_shared_directory_casing_and_unicode() {
        assert!(aliases_case("refs/heads/Feature/a", "refs/heads/feature/b"));
        assert!(aliases_case("refs/heads/Topic", "refs/heads/topic"));
        assert!(aliases_case("refs/heads/Σ", "refs/heads/ς"));
        assert!(!aliases_case("refs/heads/Topic", "refs/heads/Topic"));
        assert!(!aliases_case("refs/heads/Topic/a", "refs/heads/Other/a"));
        assert!(!aliases_case("refs/tags/Topic", "refs/heads/topic"));
    }

    #[test]
    #[ignore = "explicit files/reftable worker snapshot measurement"]
    fn benchmark_reference_storage() {
        for backend in ["files", "reftable"] {
            let dir = fixture();
            let path = dir.path();
            let id = git(path, &["rev-parse", "HEAD"]);
            let edits: String = (0..1000)
                .map(|n| format!("create refs/heads/branch-{n:04} {id}\n"))
                .collect();
            let out = git::command::run(
                path,
                &["update-ref", "--stdin"],
                Some(edits.as_bytes()),
                std::time::Duration::from_secs(30),
            )
            .unwrap();
            assert!(out.status.success());
            if backend == "reftable" {
                migrate_reftable(path).unwrap();
            }
            let repo = git::open(path).unwrap();
            for sample in -1..15 {
                let start = std::time::Instant::now();
                let snapshot = git::snapshot_refs(&repo).unwrap();
                let snapshot_ms = start.elapsed().as_secs_f64() * 1000.0;
                assert_eq!(snapshot.refs.len(), 1001);
                let start = std::time::Instant::now();
                std::hint::black_box(head(&repo).unwrap());
                let head_ms = start.elapsed().as_secs_f64() * 1000.0;
                if sample >= 0 {
                    println!(
                        "{}",
                        serde_json::json!({"backend":backend,"sample":sample,"entries":1001,"snapshotMs":snapshot_ms,"headMs":head_ms})
                    );
                }
            }
        }
    }
}
