//! On-demand cleanup of frozen branch references; commits remain recovery-pinned.
use super::*;
use std::collections::{HashMap, HashSet};

pub const MAX_BATCH: usize = 256;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Matching,
    OldLocal,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteCopy {
    pub name: String,
    pub branch: String,
    pub url: String,
}
#[derive(Clone, Debug)]
pub struct Candidate {
    pub name: String,
    pub oid: String,
    pub author: String,
    pub time: i64,
    pub section: Section,
    pub remote: Option<RemoteCopy>,
    pub protected: bool,
    pub upstream: Option<(String, String)>,
    pub tracking: Option<(String, String)>,
}
#[derive(Clone, Debug, Default)]
pub struct Analysis {
    pub candidates: Vec<Candidate>,
    pub excluded_worktrees: usize,
    pub storage_kib: u64,
    pub warnings: Vec<String>,
}
#[derive(Clone, Debug)]
pub struct Plan {
    pub branches: Vec<Candidate>,
    pub delete_remotes: bool,
}

fn checked_out(root: &Path) -> Result<HashSet<String>> {
    let bytes = command::checked(root, &["worktree", "list", "--porcelain", "-z"])?;
    bytes
        .split(|b| *b == 0)
        .filter_map(|line| line.strip_prefix(b"branch "))
        .map(|name| std::str::from_utf8(name).map(str::to_owned).map_err(err))
        .collect()
}
fn push_url(root: &Path, remote: &str) -> Result<String> {
    let text = output(root, &["remote", "get-url", "--push", "--all", remote])?;
    let urls: Vec<_> = text.lines().filter(|s| !s.is_empty()).collect();
    if urls.len() != 1 {
        return Err(format!(
            "{remote} has multiple push URLs; remote deletion is unavailable."
        ));
    }
    Ok(urls[0].into())
}
pub fn storage_kib(root: &Path) -> Result<u64> {
    let text = output(root, &["count-objects", "-v"])?;
    let mut total = 0;
    for line in text.lines() {
        if let Some(value) = line
            .strip_prefix("size: ")
            .or_else(|| line.strip_prefix("size-pack: "))
        {
            total += value.parse::<u64>().map_err(err)?;
        }
    }
    Ok(total)
}
pub fn cutoff(now: i64) -> i64 {
    crate::ui::time::six_months_before(now)
}
pub fn analyze(root: &Path, now: i64) -> Result<Analysis> {
    let repo = super::super::open(root).map_err(err)?;
    let refs = super::super::snapshot_refs(&repo).map_err(err)?;
    let checked = checked_out(root)?;
    let raw = super::super::refs::raw_refs(&repo)?;
    let mut protected = HashSet::from(["main".to_string(), "master".to_string()]);
    for reference in &raw {
        if reference.name.ends_with("/HEAD")
            && let Some(target) = &reference.symbolic
            && let Some((_, name)) = target
                .strip_prefix("refs/remotes/")
                .and_then(|r| r.split_once('/'))
        {
            protected.insert(name.to_string());
        }
    }
    let remote_refs: HashMap<_, _> = refs
        .refs
        .iter()
        .filter(|r| r.kind == super::super::RefKind::Remote)
        .filter_map(|r| {
            r.name
                .split_once('/')
                .map(|(remote, branch)| ((remote.to_string(), branch.to_string()), r.id))
        })
        .collect();
    let mut by_name = HashMap::<&str, Vec<(&(String, String), &gix::ObjectId)>>::new();
    for (key, oid) in &remote_refs {
        by_name.entry(&key.1).or_default().push((key, oid));
    }
    let mut urls = HashMap::<String, Option<String>>::new();
    let mut details = HashMap::new();
    let mut result = Analysis {
        storage_kib: storage_kib(root)?,
        ..Analysis::default()
    };
    for reference in refs.refs.iter().filter(|r| {
        r.kind == super::super::RefKind::Branch
            || (r.kind == super::super::RefKind::Head && refs.head_branch.as_ref() == Some(&r.name))
    }) {
        if checked.contains(&format!("refs/heads/{}", reference.name)) {
            result.excluded_worktrees += 1;
            continue;
        }
        let upstream = refs.upstreams.get(&reference.name);
        let identity = upstream
            .filter(|(remote, _)| remote != ".")
            .cloned()
            .or_else(|| {
                if upstream.is_some() {
                    return None;
                }
                let matches: Vec<_> = by_name
                    .get(reference.name.as_str())
                    .into_iter()
                    .flatten()
                    .filter(|(_, oid)| **oid == reference.id)
                    .map(|(key, _)| (key.0.clone(), key.1.clone()))
                    .collect();
                (matches.len() == 1).then(|| matches[0].clone())
            });
        let matching = identity
            .as_ref()
            .is_some_and(|key| remote_refs.get(key) == Some(&reference.id));
        let has_remote_copy = identity
            .as_ref()
            .is_some_and(|key| remote_refs.contains_key(key))
            || by_name.contains_key(reference.name.as_str());
        if !matching && has_remote_copy {
            continue;
        }
        if let std::collections::hash_map::Entry::Vacant(entry) = details.entry(reference.id) {
            entry.insert(super::super::commit_details(&repo, &reference.id).map_err(err)?);
        }
        let tip = &details[&reference.id];
        if !matching && tip.commit_time > cutoff(now) {
            continue;
        }
        let tracking = if matching { identity.clone() } else { None };
        let mut remote = None;
        if matching && let Some((name, branch)) = identity {
            let url = urls
                .entry(name.clone())
                .or_insert_with(|| match push_url(root, &name) {
                    Ok(url) => Some(url),
                    Err(error) => {
                        result.warnings.push(error);
                        None
                    }
                });
            if let Some(url) = url {
                remote = Some(RemoteCopy {
                    name,
                    branch,
                    url: url.clone(),
                });
            }
        }
        result.candidates.push(Candidate {
            name: reference.name.clone(),
            oid: reference.id.to_string(),
            author: command::clean(&tip.author_name),
            time: tip.commit_time,
            section: if matching {
                Section::Matching
            } else {
                Section::OldLocal
            },
            protected: protected.contains(&reference.name)
                || tracking
                    .as_ref()
                    .is_some_and(|(_, branch)| protected.contains(branch)),
            remote,
            upstream: upstream.cloned(),
            tracking,
        });
    }
    result.candidates.sort_by(|a, b| {
        (a.section != Section::Matching, &a.name).cmp(&(b.section != Section::Matching, &b.name))
    });
    Ok(result)
}
fn validate_plan(context: &Context, plan: &Plan) -> Result<()> {
    if plan.branches.is_empty() || plan.branches.len() > MAX_BATCH {
        return Err("Select 1–256 branches for a cleanup batch.".into());
    }
    let checked = checked_out(&context.root)?;
    let repo = super::super::open(&context.root).map_err(err)?;
    let refs = super::super::snapshot_refs(&repo).map_err(err)?;
    let tips: HashMap<_, _> = refs
        .refs
        .iter()
        .filter(|r| {
            matches!(
                r.kind,
                super::super::RefKind::Branch | super::super::RefKind::Head
            )
        })
        .map(|r| (r.name.as_str(), r.id))
        .collect();
    let tracking_tips: HashMap<_, _> = refs
        .refs
        .iter()
        .filter(|r| r.kind == super::super::RefKind::Remote)
        .map(|r| (r.name.as_str(), r.id))
        .collect();
    let mut urls = HashMap::new();
    let mut names = HashSet::new();
    for candidate in &plan.branches {
        validate_name(&context.root, &candidate.name)?;
        if !names.insert(&candidate.name) {
            return Err("Duplicate cleanup branch.".into());
        }
        if checked.contains(&format!("refs/heads/{}", candidate.name)) {
            return Err(format!(
                "{} is now checked out in a worktree. Analyze again.",
                candidate.name
            ));
        }
        let expected = gix::ObjectId::from_hex(candidate.oid.as_bytes()).map_err(err)?;
        if tips.get(candidate.name.as_str()) != Some(&expected)
            || refs.upstreams.get(&candidate.name) != candidate.upstream.as_ref()
        {
            return Err(format!(
                "{} changed since analysis. Analyze again.",
                candidate.name
            ));
        }
        if let Some((remote, branch)) = &candidate.tracking {
            let name = format!("{remote}/{branch}");
            if tracking_tips.get(name.as_str()) != Some(&expected) {
                return Err(format!("Tracking ref {name} changed. Analyze again."));
            }
        }
        if plan.delete_remotes
            && let Some(remote) = &candidate.remote
        {
            let current = if let Some(url) = urls.get(&remote.name) {
                url
            } else {
                let url = push_url(&context.root, &remote.name)?;
                urls.entry(remote.name.clone()).or_insert(url)
            };
            if current != &remote.url {
                return Err(format!("{} was reconfigured. Analyze again.", remote.name));
            }
        }
    }
    Ok(())
}
pub(super) fn apply(context: &Context, plan: &Plan) -> Result<()> {
    validate_plan(context, plan)?;
    let mut groups = BTreeMap::<(String, String), BTreeMap<String, String>>::new();
    if plan.delete_remotes {
        for candidate in &plan.branches {
            if let Some(remote) = &candidate.remote {
                groups
                    .entry((remote.name.clone(), remote.url.clone()))
                    .or_default()
                    .insert(remote.branch.clone(), candidate.oid.clone());
            }
        }
        // Validate every remote before changing any remote or local reference.
        for ((_, url), branches) in &groups {
            for (branch, oid) in branches {
                let name = format!("refs/heads/{branch}");
                let remote = output(
                    &context.root,
                    &["ls-remote", "--exit-code", "--heads", "--", url, &name],
                )?;
                let fields: Vec<_> = remote.split_whitespace().collect();
                if fields.as_slice() != [oid.as_str(), name.as_str()] {
                    return Err(format!(
                        "Remote branch {branch} changed. Fetch and analyze again."
                    ));
                }
            }
        }
        for ((name, url), branches) in &groups {
            let mut owned = vec!["push".to_string(), "--atomic".into()];
            for (branch, oid) in branches {
                owned.push(format!("--force-with-lease=refs/heads/{branch}:{oid}"));
            }
            owned.extend(["--".into(), url.clone()]);
            for branch in branches.keys() {
                owned.push(format!(":refs/heads/{branch}"));
            }
            command::checked(&context.root, &owned.iter().map(String::as_str).collect::<Vec<_>>())
                .map_err(|e| format!("Remote cleanup stopped at {name}: {e}. Earlier remote groups may have completed; local branches were retained."))?;
        }
    }
    // Recheck after any network wait. Expected-OID deletions are one transaction.
    validate_plan(context, plan).map_err(|e| {
        if groups.is_empty() {
            e
        } else {
            format!("{e} Remote deletions may have completed; local branches were retained.")
        }
    })?;
    let mut input = String::from("start\n");
    for candidate in &plan.branches {
        input += &format!("delete refs/heads/{} {}\n", candidate.name, candidate.oid);
    }
    input += "prepare\ncommit\n";
    run(
        &context.root,
        &["update-ref", "--stdin"],
        Some(input.as_bytes()),
        &[],
    )?;
    for candidate in &plan.branches {
        if plan.delete_remotes
            && let Some(remote) = &candidate.remote
        {
            // A newer tracking ref must not be overwritten after the remote lease.
            let reference = format!("refs/remotes/{}/{}", remote.name, remote.branch);
            let _ = command::checked(
                &context.root,
                &["update-ref", "-d", &reference, &candidate.oid],
            );
        }
    }
    Ok(())
}
pub(super) fn optimize(context: &Context) -> Result<String> {
    let before = storage_kib(&context.root)?;
    // Git explicitly retains packed and loose unreachable objects with this flag.
    command::checked(&context.root, &["repack", "-a", "-d", "--keep-unreachable"])?;
    let after = storage_kib(&context.root)?;
    Ok(format!(
        "Storage repacked: {before} → {after} KiB (Git-reported object storage); {} KiB reclaimed, {} KiB added. Recovery and unreachable objects were retained; no pruning was requested.",
        before.saturating_sub(after),
        after.saturating_sub(before)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        bare: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("working");
            let bare = dir.path().join("origin.git");
            fs::create_dir(&root).unwrap();
            fs::create_dir(&bare).unwrap();
            command::checked(&bare, &["init", "--bare", "-q"]).unwrap();
            let f = Self {
                _dir: dir,
                root,
                bare,
            };
            f.git(&["init", "-q", "-b", "main"]);
            for (name, value) in [
                ("user.name", "Cleanup Owner"),
                ("user.email", "cleanup@example.invalid"),
                ("core.autocrlf", "false"),
                ("core.hooksPath", "disabled-hooks"),
                ("commit.gpgSign", "false"),
            ] {
                f.git(&["config", name, value]);
            }
            fs::write(f.root.join("file.txt"), "old\n").unwrap();
            f.git(&["add", "."]);
            let date = OsStr::new("2000-01-01T00:00:00Z");
            let out = command::run_env(
                &f.root,
                &["commit", "-qm", "old"],
                None,
                Duration::from_secs(30),
                &[("GIT_AUTHOR_DATE", date), ("GIT_COMMITTER_DATE", date)],
            )
            .unwrap();
            assert!(out.status.success());
            f.git(&["branch", "old/local"]);
            fs::write(f.root.join("file.txt"), "current\n").unwrap();
            f.git(&["add", "."]);
            f.git(&["commit", "-qm", "current"]);
            f.git(&["branch", "fixes/task-123"]);
            f.git(&["branch", "master"]);
            f.git(&["remote", "add", "origin", f.bare.to_str().unwrap()]);
            f.git(&["push", "-q", "origin", "main", "fixes/task-123", "master"]);
            f.git(&[
                "branch",
                "--set-upstream-to=origin/fixes/task-123",
                "fixes/task-123",
            ]);
            f.git(&["branch", "--set-upstream-to=origin/master", "master"]);
            f
        }
        fn git(&self, args: &[&str]) -> String {
            output(&self.root, args).unwrap()
        }
        fn candidate(&self, name: &str) -> Candidate {
            analyze(&self.root, crate::ui::time::now_unix())
                .unwrap()
                .candidates
                .into_iter()
                .find(|c| c.name == name)
                .unwrap()
        }
        fn run(&self, plan: Plan) -> Outcome {
            execute(&self.root, Request::Cleanup(plan))
        }
    }
    #[test]
    fn cleanup_analysis_classifies_dates_protection_and_worktree_exclusions() {
        let f = Fixture::new();
        let analysis = analyze(&f.root, crate::ui::time::now_unix()).unwrap();
        assert_eq!(analysis.excluded_worktrees, 1);
        assert!(
            analysis
                .candidates
                .iter()
                .all(|c| !c.author.is_empty() && c.time > 0)
        );
        assert!(f.candidate("master").protected);
        assert_eq!(f.candidate("fixes/task-123").section, Section::Matching);
        assert_eq!(f.candidate("old/local").section, Section::OldLocal);
        let worktree = f._dir.path().join("pinned");
        f.git(&["worktree", "add", worktree.to_str().unwrap(), "old/local"]);
        let analysis = analyze(&f.root, crate::ui::time::now_unix()).unwrap();
        assert_eq!(analysis.excluded_worktrees, 2);
        assert!(!analysis.candidates.iter().any(|c| c.name == "old/local"));
    }
    #[test]
    fn cleanup_branch_validation_ignores_same_named_tag() {
        let f = Fixture::new();
        let branch = f.candidate("fixes/task-123");
        f.git(&["tag", "fixes/task-123", "old/local"]);
        assert!(
            f.run(Plan {
                branches: vec![branch],
                delete_remotes: false
            })
            .result
            .is_ok()
        );
        assert!(
            !f.git(&[
                "for-each-ref",
                "--format=%(refname)",
                "refs/heads/fixes/task-123"
            ])
            .contains("refs/heads/")
        );
        assert!(
            f.git(&[
                "for-each-ref",
                "--format=%(refname)",
                "refs/tags/fixes/task-123"
            ])
            .contains("refs/tags/")
        );
    }
    #[test]
    fn protected_upstream_stays_unchecked_when_remote_deletion_is_unavailable() {
        let f = Fixture::new();
        f.git(&["branch", "release-alias", "master"]);
        f.git(&["config", "branch.release-alias.remote", "origin"]);
        f.git(&["config", "branch.release-alias.merge", "refs/heads/master"]);
        f.git(&["config", "--add", "remote.origin.pushurl", "first.invalid"]);
        f.git(&["config", "--add", "remote.origin.pushurl", "second.invalid"]);
        let candidate = f.candidate("release-alias");
        assert!(candidate.protected);
        assert!(candidate.remote.is_none());
        assert_eq!(candidate.tracking, Some(("origin".into(), "master".into())));
    }
    #[test]
    fn cleanup_local_batch_is_recoverable_and_keeps_remote_and_worktree() {
        let f = Fixture::new();
        let branch = f.candidate("fixes/task-123");
        let old = f.candidate("old/local");
        let before = fs::read(f.root.join("file.txt")).unwrap();
        let outcome = f.run(Plan {
            branches: vec![branch.clone(), old.clone()],
            delete_remotes: false,
        });
        assert!(outcome.result.is_ok(), "{:?}", outcome.result);
        assert!(
            command::checked(
                &f.root,
                &["show-ref", "--verify", "refs/heads/fixes/task-123"]
            )
            .is_err()
        );
        assert_eq!(
            output(&f.bare, &["rev-parse", "refs/heads/fixes/task-123"]).unwrap(),
            branch.oid
        );
        assert_eq!(fs::read(f.root.join("file.txt")).unwrap(), before);
        assert!(f.git(&["status", "--porcelain"]).is_empty());
        assert!(execute(&f.root, Request::MoveTo(0)).result.is_ok());
        assert_eq!(
            f.git(&["rev-parse", "refs/heads/fixes/task-123"]),
            branch.oid
        );
        assert_eq!(f.git(&["rev-parse", "refs/heads/old/local"]), old.oid);
    }
    #[test]
    fn cleanup_refuses_moved_or_newly_checked_out_branch() {
        let f = Fixture::new();
        let old = f.candidate("old/local");
        f.git(&["branch", "-f", "old/local", "main"]);
        assert!(
            f.run(Plan {
                branches: vec![old],
                delete_remotes: false
            })
            .result
            .is_err()
        );
        let branch = f.candidate("fixes/task-123");
        let path = f._dir.path().join("new-worktree");
        f.git(&["worktree", "add", path.to_str().unwrap(), "fixes/task-123"]);
        assert!(
            f.run(Plan {
                branches: vec![branch],
                delete_remotes: false
            })
            .result
            .is_err()
        );
        assert!(
            !f.git(&["rev-parse", "refs/heads/fixes/task-123"])
                .is_empty()
        );
    }
    #[test]
    fn cleanup_remote_delete_is_leased_and_local_undo_does_not_recreate_server_branch() {
        let f = Fixture::new();
        let branch = f.candidate("fixes/task-123");
        let outcome = f.run(Plan {
            branches: vec![branch.clone()],
            delete_remotes: true,
        });
        assert!(outcome.result.is_ok(), "{:?}", outcome.result);
        assert!(
            command::checked(
                &f.bare,
                &["show-ref", "--verify", "refs/heads/fixes/task-123"]
            )
            .is_err()
        );
        assert!(execute(&f.root, Request::MoveTo(0)).result.is_ok());
        assert_eq!(
            f.git(&["rev-parse", "refs/heads/fixes/task-123"]),
            branch.oid
        );
        assert!(
            command::checked(
                &f.bare,
                &["show-ref", "--verify", "refs/heads/fixes/task-123"]
            )
            .is_err()
        );
    }
    #[test]
    fn cleanup_remote_change_rejects_before_any_local_deletion() {
        let f = Fixture::new();
        let branch = f.candidate("fixes/task-123");
        let old = f.candidate("old/local");
        command::checked(
            &f.bare,
            &["update-ref", "refs/heads/fixes/task-123", &old.oid],
        )
        .unwrap();
        assert!(
            f.run(Plan {
                branches: vec![branch.clone()],
                delete_remotes: true
            })
            .result
            .is_err()
        );
        assert_eq!(
            f.git(&["rev-parse", "refs/heads/fixes/task-123"]),
            branch.oid
        );
        assert_eq!(
            output(&f.bare, &["rev-parse", "refs/heads/fixes/task-123"]).unwrap(),
            old.oid
        );
    }
    #[test]
    fn cleanup_repack_preserves_unreachable_objects_and_recovery() {
        let f = Fixture::new();
        let branch = f.candidate("fixes/task-123");
        let dangling = f.git(&["commit-tree", "HEAD^{tree}", "-m", "unreachable preserved"]);
        assert!(
            f.run(Plan {
                branches: vec![branch.clone()],
                delete_remotes: false
            })
            .result
            .is_ok()
        );
        let result = execute(&f.root, Request::OptimizeStorage);
        assert!(result.result.is_ok(), "{:?}", result.result);
        f.git(&["cat-file", "-e", &dangling]);
        assert!(execute(&f.root, Request::MoveTo(0)).result.is_ok());
        assert_eq!(
            f.git(&["rev-parse", "refs/heads/fixes/task-123"]),
            branch.oid
        );
    }
    #[test]
    #[ignore = "one warmup and fifteen disposable cleanup analysis/action samples"]
    fn benchmark_cleanup() {
        for sample in -1..15 {
            let f = Fixture::new();
            let start = std::time::Instant::now();
            let analysis = analyze(&f.root, crate::ui::time::now_unix()).unwrap();
            let analyze_ms = start.elapsed().as_secs_f64() * 1000.;
            let branches = analysis
                .candidates
                .into_iter()
                .filter(|c| !c.protected)
                .collect();
            let start = std::time::Instant::now();
            assert!(
                f.run(Plan {
                    branches,
                    delete_remotes: false
                })
                .result
                .is_ok()
            );
            let delete_ms = start.elapsed().as_secs_f64() * 1000.;
            let start = std::time::Instant::now();
            assert!(execute(&f.root, Request::OptimizeStorage).result.is_ok());
            println!(
                "{}",
                serde_json::json!({"sample":sample,"analysisMs":analyze_ms,"deleteMs":delete_ms,"repackMs":start.elapsed().as_secs_f64()*1000.})
            );
        }
    }
}
