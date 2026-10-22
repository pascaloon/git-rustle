//! On-demand cleanup of frozen branch references; commits remain recovery-pinned.
use super::*;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Section {
    FetchedOnly,
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
    pub tracking_oid: Option<String>,
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
}

fn checked_out(root: &Path) -> Result<HashSet<String>> {
    let bytes = command::checked(root, &["worktree", "list", "--porcelain", "-z"])?;
    bytes
        .split(|b| *b == 0)
        .filter_map(|line| line.strip_prefix(b"branch "))
        .map(|name| std::str::from_utf8(name).map(str::to_owned).map_err(err))
        .collect()
}
fn fetch_url(root: &Path, remote: &str) -> Result<String> {
    let text = output(root, &["remote", "get-url", "--all", remote])?;
    let urls: Vec<_> = text.lines().filter(|s| !s.is_empty()).collect();
    if urls.len() != 1 {
        return Err(format!("{remote}: expected one fetch URL"));
    }
    Ok(urls[0].into())
}
struct Server {
    url: String,
    heads: HashMap<String, String>,
    default: Option<String>,
}
fn advertise(root: &Path, remote: &str, timeout: Duration) -> Result<Server> {
    let url = fetch_url(root, remote)?;
    let out = command::run(
        root,
        &["ls-remote", "--symref", "--", &url, "HEAD", "refs/heads/*"],
        None,
        timeout,
    )?;
    if !out.status.success() {
        return Err(command::clean(&String::from_utf8_lossy(&out.stderr)));
    }
    let mut heads = HashMap::new();
    let mut default = None;
    for line in std::str::from_utf8(&out.stdout).map_err(err)?.lines() {
        let Some((value, name)) = line.split_once('\t') else {
            return Err("Malformed remote advertisement".into());
        };
        if name == "HEAD" {
            if let Some(branch) = value.strip_prefix("ref: refs/heads/") {
                default = Some(branch.into());
            }
        } else if let Some(branch) = name.strip_prefix("refs/heads/") {
            gix::ObjectId::from_hex(value.as_bytes()).map_err(err)?;
            heads.insert(branch.into(), value.into());
        }
    }
    Ok(Server {
        url,
        heads,
        default,
    })
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
    crate::ui::time::one_year_before(now)
}
pub fn analyze(root: &Path, now: i64) -> Result<Analysis> {
    let repo = super::super::open(root).map_err(err)?;
    let refs = super::super::snapshot_refs(&repo).map_err(err)?;
    let raw = super::super::refs::raw_refs(&repo)?;
    let checked = checked_out(root)?;
    let mut result = Analysis {
        storage_kib: storage_kib(root)?,
        ..Analysis::default()
    };
    let remote_names = output(root, &["remote"])?;
    let started = std::time::Instant::now();
    let mut servers = HashMap::new();
    for remote in remote_names.lines().filter(|s| !s.is_empty()) {
        let remaining = Duration::from_secs(20).saturating_sub(started.elapsed());
        let verification = if remaining.is_zero() {
            Err("Remote verification time budget exhausted".into())
        } else {
            advertise(root, remote, remaining.min(Duration::from_secs(5)))
        };
        match verification {
            Ok(server) => {
                servers.insert(remote.to_string(), server);
            }
            Err(e) => result.warnings.push(format!(
                "{remote}: verification unavailable; affected branches excluded. {e}"
            )),
        }
    }
    let mut protected = HashSet::from(["main".to_string(), "master".to_string()]);
    for server in servers.values() {
        if let Some(name) = &server.default {
            protected.insert(name.clone());
        }
    }
    for reference in &raw {
        if reference.name.ends_with("/HEAD")
            && let Some(target) = &reference.symbolic
            && let Some((_, name)) = target
                .strip_prefix("refs/remotes/")
                .and_then(|r| r.split_once('/'))
        {
            protected.insert(name.into());
        }
    }
    // Preserve namespaces: the graph snapshot intentionally deduplicates display
    // labels and must not determine cleanup's exact reference identities.
    let local_entries: Vec<_> = raw
        .iter()
        .filter(|r| r.symbolic.is_none())
        .filter_map(|r| {
            Some(super::super::RefEntry {
                name: r.name.strip_prefix("refs/heads/")?.into(),
                id: r.id?,
                kind: super::super::RefKind::Branch,
            })
        })
        .collect();
    let mut configured: Vec<_> = remote_names.lines().collect();
    configured.sort_by_key(|name| std::cmp::Reverse(name.len()));
    let cached: HashMap<_, _> = raw
        .iter()
        .filter(|r| r.symbolic.is_none())
        .filter_map(|r| {
            let short = r.name.strip_prefix("refs/remotes/")?;
            let remote = configured.iter().find(|name| {
                short
                    .strip_prefix(**name)
                    .is_some_and(|rest| rest.starts_with('/'))
            })?;
            Some((
                (remote.to_string(), short[remote.len() + 1..].to_string()),
                r.id?,
            ))
        })
        .collect();
    let locals: HashSet<_> = local_entries.iter().map(|r| r.name.clone()).collect();
    let associated: HashSet<_> = refs.upstreams.values().cloned().collect();
    let mut by_name = HashMap::<&str, Vec<(String, String)>>::new();
    for (remote, server) in &servers {
        for branch in server.heads.keys() {
            by_name
                .entry(branch)
                .or_default()
                .push((remote.clone(), branch.clone()));
        }
    }
    let mut owners = HashMap::<&(String, String), Vec<&str>>::new();
    for (name, key) in &refs.upstreams {
        owners.entry(key).or_default().push(name);
    }
    let server_names: HashSet<_> = servers.values().flat_map(|s| s.heads.keys()).collect();
    let mut details = HashMap::new();
    for reference in &local_entries {
        if checked.contains(&format!("refs/heads/{}", reference.name)) {
            result.excluded_worktrees += 1;
            continue;
        }
        let upstream = refs.upstreams.get(&reference.name).cloned();
        let identity = upstream
            .as_ref()
            .filter(|(r, _)| r != ".")
            .cloned()
            .or_else(|| {
                if upstream.is_some() {
                    return None;
                }
                let matches = by_name.get(reference.name.as_str())?;
                (matches.len() == 1).then(|| matches[0].clone())
            });
        let oid = reference.id.to_string();
        // A shared upstream is retained: deleting it would break another local branch.
        let shared = identity.as_ref().is_some_and(|key| {
            owners
                .get(key)
                .is_some_and(|names| names.iter().any(|name| *name != reference.name))
                || (key.1 != reference.name && locals.contains(&key.1))
        });
        let matching = !shared
            && identity
                .as_ref()
                .is_some_and(|(r, b)| servers.get(r).is_some_and(|s| s.heads.get(b) == Some(&oid)));
        let absent = servers.len() == remote_names.lines().filter(|s| !s.is_empty()).count()
            && !server_names.contains(&reference.name)
            && !identity
                .as_ref()
                .is_some_and(|(r, b)| servers.get(r).is_none_or(|s| s.heads.contains_key(b)))
            && upstream.as_ref().is_none_or(|(r, _)| r != ".");
        if !matching && !absent {
            continue;
        }
        if let std::collections::hash_map::Entry::Vacant(e) = details.entry(reference.id) {
            e.insert(super::super::commit_details(&repo, &reference.id).map_err(err)?);
        }
        let tip = &details[&reference.id];
        if !matching && tip.commit_time > cutoff(now) {
            continue;
        }
        let remote = if matching {
            identity.as_ref().map(|(r, b)| RemoteCopy {
                name: r.clone(),
                branch: b.clone(),
                url: servers[r].url.clone(),
            })
        } else {
            None
        };
        result.candidates.push(Candidate {
            name: reference.name.clone(),
            oid,
            author: command::clean(&tip.author_name),
            time: tip.commit_time,
            section: if matching {
                Section::Matching
            } else {
                Section::OldLocal
            },
            protected: protected.contains(&reference.name)
                || identity
                    .as_ref()
                    .is_some_and(|(_, b)| protected.contains(b)),
            remote,
            upstream,
            tracking_oid: if matching {
                identity
                    .as_ref()
                    .and_then(|key| cached.get(key))
                    .map(ToString::to_string)
            } else {
                None
            },
            tracking: if matching { identity } else { None },
        });
    }
    let mut missing_server_refs = 0usize;
    for ((r, b), id) in &cached {
        if associated.contains(&(r.clone(), b.clone())) || locals.contains(b) {
            continue;
        }
        let Some(server) = servers.get(r) else {
            continue;
        };
        // This is a local cached copy. The server must still have that branch,
        // but it may have advanced since the last fetch. Lease the cached OID
        // independently; never write server refs or fetch branch objects here.
        if !server.heads.contains_key(b) {
            missing_server_refs += 1;
            continue;
        }
        if let std::collections::hash_map::Entry::Vacant(e) = details.entry(*id) {
            e.insert(super::super::commit_details(&repo, id).map_err(err)?);
        }
        let tip = &details[id];
        result.candidates.push(Candidate {
            name: format!("{r}/{b}"),
            oid: id.to_string(),
            author: command::clean(&tip.author_name),
            time: tip.commit_time,
            section: Section::FetchedOnly,
            remote: Some(RemoteCopy {
                name: r.clone(),
                branch: b.clone(),
                url: server.url.clone(),
            }),
            protected: protected.contains(b),
            upstream: None,
            tracking_oid: Some(id.to_string()),
            tracking: Some((r.clone(), b.clone())),
        });
    }
    if missing_server_refs > 0 {
        result.warnings.push(format!("{missing_server_refs} fetched reference(s) were retained because their server branches no longer exist; these local references may be the last copy."));
    }
    let alias_targets: HashSet<_> = raw.iter().filter_map(|r| r.symbolic.as_deref()).collect();
    for candidate in &mut result.candidates {
        let local = format!("refs/heads/{}", candidate.name);
        let tracking = candidate
            .tracking
            .as_ref()
            .map(|(r, b)| format!("refs/remotes/{r}/{b}"));
        candidate.protected |= (candidate.section != Section::FetchedOnly
            && alias_targets.contains(local.as_str()))
            || tracking
                .as_ref()
                .is_some_and(|name| alias_targets.contains(name.as_str()));
    }
    result
        .candidates
        .sort_by(|a, b| (a.section, &a.name).cmp(&(b.section, &b.name)));
    Ok(result)
}
pub(super) fn apply(context: &Context, plan: &Plan) -> Result<()> {
    if plan.branches.is_empty() {
        return Err("Select at least one branch for cleanup.".into());
    }
    let frozen_config = fs::read(context.common.join("config")).map_err(err)?;
    let fresh = analyze(&context.root, crate::ui::time::now_unix())?;
    let candidates: HashMap<_, _> = fresh
        .candidates
        .iter()
        .map(|c| ((c.section, c.name.as_str()), c))
        .collect();
    let mut deletions = BTreeMap::new();
    let mut names = HashSet::new();
    for candidate in &plan.branches {
        if !names.insert((candidate.section, candidate.name.as_str())) {
            return Err("Duplicate cleanup entry".into());
        }
        let current = candidates
            .get(&(candidate.section, candidate.name.as_str()))
            .ok_or_else(|| {
                format!(
                    "{} changed or could not be verified. Analyze again.",
                    candidate.name
                )
            })?;
        if current.protected {
            return Err(format!(
                "{} is protected and cannot be cleaned up.",
                candidate.name
            ));
        }
        if current.oid != candidate.oid
            || current.upstream != candidate.upstream
            || current.tracking != candidate.tracking
            || current.tracking_oid != candidate.tracking_oid
            || current.remote != candidate.remote
        {
            return Err(format!(
                "{} changed since analysis. Analyze again.",
                candidate.name
            ));
        }
        if candidate.section != Section::FetchedOnly {
            deletions.insert(
                format!("refs/heads/{}", candidate.name),
                candidate.oid.clone(),
            );
        }
        if let (Some((r, b)), Some(oid)) = (&candidate.tracking, &candidate.tracking_oid) {
            deletions.insert(format!("refs/remotes/{r}/{b}"), oid.clone());
        }
    }
    // Recheck after all network waits, before the atomic expected-OID transaction.
    let checked = checked_out(&context.root)?;
    let repo = super::super::open(&context.root).map_err(err)?;
    let refs = super::super::snapshot_refs(&repo).map_err(err)?;
    let final_raw = super::super::refs::raw_refs(&repo)?;
    let final_locals: HashSet<_> = final_raw
        .iter()
        .filter_map(|r| r.name.strip_prefix("refs/heads/"))
        .collect();
    let mut owners = HashMap::<&(String, String), Vec<&str>>::new();
    for (name, key) in &refs.upstreams {
        owners.entry(key).or_default().push(name);
    }
    for reference in &final_raw {
        if reference
            .symbolic
            .as_ref()
            .is_some_and(|target| deletions.contains_key(target))
        {
            return Err(format!(
                "{} is referenced by symbolic alias {}. Retain it or reconfigure the alias first.",
                reference.symbolic.as_ref().unwrap(),
                reference.name
            ));
        }
    }
    let mut urls = HashMap::new();
    for candidate in &plan.branches {
        if candidate.section != Section::FetchedOnly
            && (checked.contains(&format!("refs/heads/{}", candidate.name))
                || refs.upstreams.get(&candidate.name) != candidate.upstream.as_ref())
        {
            return Err(format!(
                "{} changed or is checked out. Analyze again.",
                candidate.name
            ));
        }
        if let Some(copy) = &candidate.remote {
            if !urls.contains_key(&copy.name) {
                urls.insert(copy.name.clone(), fetch_url(&context.root, &copy.name)?);
            }
            if urls[&copy.name] != copy.url {
                return Err(format!("{} was reconfigured. Analyze again.", copy.name));
            }
        }
        if let Some(key) = &candidate.tracking
            && (owners.get(key).is_some_and(|names| {
                names.iter().any(|name| {
                    candidate.section == Section::FetchedOnly || *name != candidate.name
                })
            }) || ((candidate.section == Section::FetchedOnly || key.1 != candidate.name)
                && final_locals.contains(key.1.as_str())))
        {
            return Err("Another local branch now tracks this remote ref. Analyze again.".into());
        }
    }
    if fs::read(context.common.join("config")).map_err(err)? != frozen_config {
        return Err("Repository configuration changed during verification. Analyze again.".into());
    }
    let mut input = String::from("start\n");
    for candidate in &plan.branches {
        if let Some((remote, branch)) = &candidate.tracking
            && candidate.tracking_oid.is_none()
        {
            input += &format!(
                "verify refs/remotes/{remote}/{branch} {}\n",
                "0".repeat(candidate.oid.len())
            );
        }
    }
    for (name, oid) in deletions {
        input += &format!("delete {name} {oid}\n");
    }
    input += "prepare\ncommit\n";
    run(
        &context.root,
        &["update-ref", "--no-deref", "--stdin"],
        Some(input.as_bytes()),
        &[],
    )?;
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
    fn cleanup_matching_preserves_stale_cache_oid_through_undo_and_accepts_missing_cache() {
        let f = Fixture::new();
        let old = f.candidate("old/local");
        f.git(&["update-ref", "refs/remotes/origin/fixes/task-123", &old.oid]);
        let branch = f.candidate("fixes/task-123");
        assert_eq!(branch.section, Section::Matching);
        assert_eq!(branch.tracking_oid.as_deref(), Some(old.oid.as_str()));
        let server = refs_at(&f.bare);
        assert!(
            f.run(Plan {
                branches: vec![branch.clone()]
            })
            .result
            .is_ok()
        );
        assert!(execute(&f.root, Request::MoveTo(0)).result.is_ok());
        assert_eq!(
            f.git(&["rev-parse", "refs/remotes/origin/fixes/task-123"]),
            old.oid
        );
        assert_eq!(refs_at(&f.bare), server);
        f.git(&["update-ref", "-d", "refs/remotes/origin/fixes/task-123"]);
        let branch = f.candidate("fixes/task-123");
        assert!(branch.tracking_oid.is_none());
        assert!(
            f.run(Plan {
                branches: vec![branch]
            })
            .result
            .is_ok()
        );
    }
    #[test]
    fn cleanup_namespace_collisions_tags_and_symbolic_alias_are_safe() {
        let f = Fixture::new();
        f.git(&["push", "-q", "origin", "main:refs/heads/topic"]);
        f.git(&["branch", "origin/topic", "main"]);
        f.git(&["tag", "fixes/task-123", "old/local"]);
        let analysis = analyze(&f.root, crate::ui::time::now_unix()).unwrap();
        assert!(
            analysis
                .candidates
                .iter()
                .any(|c| c.section == Section::FetchedOnly && c.name == "origin/topic")
        );
        assert_eq!(
            analysis
                .candidates
                .iter()
                .filter(|c| c.name == "fixes/task-123")
                .count(),
            1
        );
        let matched = f.candidate("fixes/task-123");
        f.git(&[
            "symbolic-ref",
            "refs/remotes/origin/alias",
            "refs/remotes/origin/fixes/task-123",
        ]);
        assert!(
            f.run(Plan {
                branches: vec![matched]
            })
            .result
            .is_err()
        );
        assert!(f.git(&["rev-parse", "refs/heads/fixes/task-123"]).len() > 10);
        assert!(
            f.run(Plan {
                branches: vec![f.candidate("master")]
            })
            .result
            .is_err()
        );
    }

    #[test]
    fn cleanup_reftable_restores_cached_and_local_refs() {
        let f = Fixture::new();
        super::super::super::refs::migrate_reftable(&f.root).unwrap();
        let candidate = f.candidate("fixes/task-123");
        let server = refs_at(&f.bare);
        assert!(
            f.run(Plan {
                branches: vec![candidate.clone()]
            })
            .result
            .is_ok()
        );
        assert!(
            command::checked(
                &f.root,
                &["show-ref", "--verify", "refs/remotes/origin/fixes/task-123"]
            )
            .is_err()
        );
        assert!(execute(&f.root, Request::MoveTo(0)).result.is_ok());
        for reference in [
            "refs/heads/fixes/task-123",
            "refs/remotes/origin/fixes/task-123",
        ] {
            assert_eq!(f.git(&["rev-parse", reference]), candidate.oid);
        }
        assert_eq!(refs_at(&f.bare), server);
    }
    #[test]
    fn cleanup_fetched_batch_over_256_preserves_server_and_recovers() {
        let f = Fixture::new();
        let oid = f.git(&["rev-parse", "main"]);
        let mut server = String::from("start\n");
        let mut cached = String::from("start\n");
        for i in 0..300 {
            server += &format!("create refs/heads/fetched-batch/{i:04} {oid}\n");
            cached += &format!("create refs/remotes/origin/fetched-batch/{i:04} {oid}\n");
        }
        server += "prepare\ncommit\n";
        cached += "prepare\ncommit\n";
        run(
            &f.bare,
            &["update-ref", "--stdin"],
            Some(server.as_bytes()),
            &[],
        )
        .unwrap();
        run(
            &f.root,
            &["update-ref", "--stdin"],
            Some(cached.as_bytes()),
            &[],
        )
        .unwrap();
        let before = refs_at(&f.bare);
        let branches: Vec<_> = analyze(&f.root, crate::ui::time::now_unix())
            .unwrap()
            .candidates
            .into_iter()
            .filter(|c| {
                c.section == Section::FetchedOnly && c.name.starts_with("origin/fetched-batch/")
            })
            .collect();
        assert_eq!(branches.len(), 300);
        assert!(f.run(Plan { branches }).result.is_ok());
        assert!(
            f.git(&["for-each-ref", "refs/remotes/origin/fetched-batch/"])
                .is_empty()
        );
        assert_eq!(refs_at(&f.bare), before);
        assert!(execute(&f.root, Request::MoveTo(0)).result.is_ok());
        assert_eq!(
            f.git(&["for-each-ref", "refs/remotes/origin/fetched-batch/"])
                .lines()
                .count(),
            300
        );
    }
    fn refs_at(root: &Path) -> String {
        output(root, &["for-each-ref", "--format=%(refname) %(objectname)"]).unwrap()
    }
    #[test]
    fn cleanup_three_sections_preserve_server_and_recover_both_ref_types() {
        let f = Fixture::new();
        let oid = f.git(&["rev-parse", "main"]);
        f.git(&["push", "-q", "origin", "main:refs/heads/fetched-only"]);
        let fetched = f.candidate("origin/fetched-only");
        assert_eq!(fetched.section, Section::FetchedOnly);
        let matched = f.candidate("fixes/task-123");
        assert_eq!(matched.section, Section::Matching);
        let old = f.candidate("old/local");
        assert_eq!(old.section, Section::OldLocal);
        let before = refs_at(&f.root);
        let server = refs_at(&f.bare);
        fs::write(f.root.join("unrelated.txt"), "preserved").unwrap();
        let outcome = f.run(Plan {
            branches: vec![fetched, matched, old],
        });
        assert!(outcome.result.is_ok(), "{:?}", outcome.result);
        for reference in [
            "refs/remotes/origin/fetched-only",
            "refs/heads/fixes/task-123",
            "refs/remotes/origin/fixes/task-123",
            "refs/heads/old/local",
        ] {
            assert!(
                command::checked(&f.root, &["show-ref", "--verify", reference]).is_err(),
                "{reference}"
            );
        }
        assert_eq!(refs_at(&f.bare), server);
        assert_eq!(
            fs::read_to_string(f.root.join("unrelated.txt")).unwrap(),
            "preserved"
        );
        let after = refs_at(&f.root);
        assert!(execute(&f.root, Request::MoveTo(0)).result.is_ok());
        // Recovery pins are implementation refs, so compare only ordinary refs.
        let ordinary = |text: String| {
            text.lines()
                .filter(|line| !line.starts_with("refs/rustle/"))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert_eq!(ordinary(refs_at(&f.root)), ordinary(before));
        assert!(execute(&f.root, Request::MoveTo(1)).result.is_ok());
        assert_eq!(ordinary(refs_at(&f.root)), ordinary(after));
        assert_eq!(refs_at(&f.bare), server);
        assert_eq!(f.git(&["rev-parse", "main"]), oid);
    }

    #[test]
    fn fetched_only_cleanup_includes_server_ahead_but_retains_missing_server_last_copy() {
        let f = Fixture::new();
        let cached = f.git(&["rev-parse", "old/local"]);
        let ahead = f.git(&["rev-parse", "main"]);
        let unique = f.git(&[
            "commit-tree",
            "HEAD^{tree}",
            "-m",
            "unique cached tip retained by cleanup recovery",
        ]);
        for (name, oid) in [
            ("server-ahead", cached.as_str()),
            ("server-missing", unique.as_str()),
        ] {
            f.git(&["update-ref", &format!("refs/remotes/origin/{name}"), oid]);
        }
        command::checked(&f.bare, &["update-ref", "refs/heads/server-ahead", &ahead]).unwrap();
        let analysis = analyze(&f.root, crate::ui::time::now_unix()).unwrap();
        assert!(
            analysis
                .warnings
                .iter()
                .any(|warning| warning.contains("1 fetched reference")
                    && warning.contains("no longer exist"))
        );
        let branches: Vec<_> = analysis
            .candidates
            .into_iter()
            .filter(|c| c.name == "origin/server-ahead" || c.name == "origin/server-missing")
            .collect();
        assert_eq!(branches.len(), 1);
        assert!(branches.iter().all(|c| c.section == Section::FetchedOnly
            && !c.protected
            && c.tracking_oid.as_deref() == Some(c.oid.as_str())));
        let server = refs_at(&f.bare);
        assert!(f.run(Plan { branches }).result.is_ok());
        assert!(
            command::checked(
                &f.root,
                &["show-ref", "--verify", "refs/remotes/origin/server-ahead"]
            )
            .is_err()
        );
        assert_eq!(
            f.git(&["rev-parse", "refs/remotes/origin/server-missing"]),
            unique
        );
        assert_eq!(refs_at(&f.bare), server);
        assert!(execute(&f.root, Request::MoveTo(0)).result.is_ok());
        for (name, oid) in [
            ("server-ahead", cached.as_str()),
            ("server-missing", unique.as_str()),
        ] {
            assert_eq!(
                f.git(&["rev-parse", &format!("refs/remotes/origin/{name}")]),
                oid
            );
        }
        assert!(execute(&f.root, Request::MoveTo(1)).result.is_ok());
        assert_eq!(
            f.git(&["rev-parse", "refs/remotes/origin/server-missing"]),
            unique
        );
        assert_eq!(refs_at(&f.bare), server);
    }
    #[test]
    fn fetched_only_plan_leases_cached_ref_not_server_head_and_retains_unknown_server_refs() {
        let f = Fixture::new();
        let cached = f.git(&["rev-parse", "old/local"]);
        let current = f.git(&["rev-parse", "main"]);
        f.git(&["update-ref", "refs/remotes/origin/untracked-moved", &cached]);
        command::checked(
            &f.bare,
            &["update-ref", "refs/heads/untracked-moved", &cached],
        )
        .unwrap();
        let candidate = f.candidate("origin/untracked-moved");
        command::checked(
            &f.bare,
            &["update-ref", "refs/heads/untracked-moved", &current],
        )
        .unwrap();
        let server = refs_at(&f.bare);
        assert!(
            f.run(Plan {
                branches: vec![candidate]
            })
            .result
            .is_ok()
        );
        assert_eq!(refs_at(&f.bare), server);
        assert!(execute(&f.root, Request::MoveTo(0)).result.is_ok());
        let candidate = f.candidate("origin/untracked-moved");
        f.git(&[
            "update-ref",
            "refs/remotes/origin/untracked-moved",
            &current,
        ]);
        assert!(
            f.run(Plan {
                branches: vec![candidate]
            })
            .result
            .is_err()
        );
        assert_eq!(
            f.git(&["rev-parse", "refs/remotes/origin/untracked-moved"]),
            current
        );
        let candidate = f.candidate("origin/untracked-moved");
        command::checked(
            &f.bare,
            &["update-ref", "-d", "refs/heads/untracked-moved", &current],
        )
        .unwrap();
        assert!(
            f.run(Plan {
                branches: vec![candidate]
            })
            .result
            .is_err(),
            "server deletion invalidates frozen fetched-only eligibility"
        );
        assert_eq!(
            f.git(&["rev-parse", "refs/remotes/origin/untracked-moved"]),
            current
        );
        command::checked(
            &f.bare,
            &["update-ref", "refs/heads/untracked-moved", &current],
        )
        .unwrap();
        let candidate = f.candidate("origin/untracked-moved");
        f.git(&[
            "config",
            "remote.origin.url",
            f._dir.path().join("offline.git").to_str().unwrap(),
        ]);
        let analysis = analyze(&f.root, crate::ui::time::now_unix()).unwrap();
        assert!(!analysis.warnings.is_empty());
        assert!(
            !analysis
                .candidates
                .iter()
                .any(|c| c.name == "origin/untracked-moved")
        );
        assert!(
            f.run(Plan {
                branches: vec![candidate]
            })
            .result
            .is_err()
        );
        assert_eq!(
            f.git(&["rev-parse", "refs/remotes/origin/untracked-moved"]),
            current
        );
        assert_eq!(refs_at(&f.bare), server);
    }
    #[test]
    fn cleanup_server_stale_missing_and_offline_refs_are_not_matching() {
        let f = Fixture::new();
        let plan = Plan {
            branches: vec![f.candidate("fixes/task-123")],
        };
        let old = f.git(&["rev-parse", "old/local"]);
        command::checked(&f.bare, &["update-ref", "refs/heads/fixes/task-123", &old]).unwrap();
        assert!(f.run(plan).result.is_err());
        assert!(
            !analyze(&f.root, crate::ui::time::now_unix())
                .unwrap()
                .candidates
                .iter()
                .any(|c| c.name == "fixes/task-123")
        );
        command::checked(&f.bare, &["update-ref", "-d", "refs/heads/fixes/task-123"]).unwrap();
        assert!(
            !analyze(&f.root, crate::ui::time::now_unix())
                .unwrap()
                .candidates
                .iter()
                .any(|c| c.name == "fixes/task-123")
        );
        f.git(&[
            "config",
            "remote.origin.url",
            f._dir.path().join("offline.git").to_str().unwrap(),
        ]);
        let analysis = analyze(&f.root, crate::ui::time::now_unix()).unwrap();
        assert!(!analysis.warnings.is_empty());
        assert!(analysis.candidates.is_empty());
    }
    #[test]
    fn cleanup_shared_upstream_same_name_worktrees_and_reconfiguration_are_safe() {
        let f = Fixture::new();
        let branch = f.candidate("fixes/task-123");
        f.git(&["branch", "alias", "fixes/task-123"]);
        f.git(&["config", "branch.alias.remote", "origin"]);
        f.git(&["config", "branch.alias.merge", "refs/heads/fixes/task-123"]);
        let analysis = analyze(&f.root, crate::ui::time::now_unix()).unwrap();
        assert!(!analysis.candidates.iter().any(|c| c.name == "alias"
            || c.name == "fixes/task-123"
            || c.name == "origin/fixes/task-123"));
        assert!(
            f.run(Plan {
                branches: vec![branch]
            })
            .result
            .is_err()
        );
        let old = f.candidate("old/local");
        let worktree = f._dir.path().join("pinned");
        f.git(&["worktree", "add", worktree.to_str().unwrap(), "old/local"]);
        assert!(
            f.run(Plan {
                branches: vec![old]
            })
            .result
            .is_err()
        );
        assert_eq!(
            analyze(&f.root, crate::ui::time::now_unix())
                .unwrap()
                .excluded_worktrees,
            2
        );
    }
    #[test]
    fn cleanup_old_local_boundary_and_unfetched_server_counterpart() {
        let f = Fixture::new();
        let old = f.candidate("old/local");
        // At the exact one-year boundary this branch qualifies; one second newer does not.
        let anniversary = old.time + 366 * 86400; // 2000-01-01 -> 2001-01-01
        assert_eq!(cutoff(anniversary), old.time);
        assert!(
            analyze(&f.root, anniversary)
                .unwrap()
                .candidates
                .iter()
                .any(|c| c.name == "old/local")
        );
        assert!(
            !analyze(&f.root, anniversary - 1)
                .unwrap()
                .candidates
                .iter()
                .any(|c| c.name == "old/local")
        );
        command::checked(&f.bare, &["update-ref", "refs/heads/old/local", &old.oid]).unwrap();
        let verified = f.candidate("old/local");
        assert_eq!(verified.section, Section::Matching);
        assert!(verified.tracking_oid.is_none());
    }
    #[test]
    fn cleanup_changed_url_or_oid_rejects_frozen_plan_and_keeps_other_refs() {
        let f = Fixture::new();
        let matched = f.candidate("fixes/task-123");
        let old = f.candidate("old/local");
        f.git(&["update-ref", "refs/remotes/origin/fixes/task-123", &old.oid]);
        assert!(
            f.run(Plan {
                branches: vec![matched, old.clone()]
            })
            .result
            .is_err()
        );
        assert_eq!(f.git(&["rev-parse", "old/local"]), old.oid);
        f.git(&[
            "update-ref",
            "refs/remotes/origin/fixes/task-123",
            &f.git(&["rev-parse", "main"]),
        ]);
        let matched = f.candidate("fixes/task-123");
        f.git(&[
            "config",
            "remote.origin.url",
            f.bare.to_str().unwrap().trim_end_matches("origin.git"),
        ]);
        assert!(
            f.run(Plan {
                branches: vec![matched]
            })
            .result
            .is_err()
        );
    }
    fn large_branches(f: &Fixture, count: usize) {
        let oid = f.git(&["rev-parse", "old/local"]);
        let mut input = String::from("start\n");
        for i in 0..count {
            input += &format!("create refs/heads/large-cleanup/{i:04} {oid}\n");
        }
        input += "prepare\ncommit\n";
        run(
            &f.root,
            &["update-ref", "--no-deref", "--stdin"],
            Some(input.as_bytes()),
            &[],
        )
        .unwrap();
    }
    #[test]
    fn cleanup_more_than_256_is_atomic_recoverable_and_preserves_server() {
        let f = Fixture::new();
        large_branches(&f, 300);
        let server = refs_at(&f.bare);
        let branches: Vec<_> = analyze(&f.root, crate::ui::time::now_unix())
            .unwrap()
            .candidates
            .into_iter()
            .filter(|c| c.name.starts_with("large-cleanup/"))
            .collect();
        assert_eq!(branches.len(), 300);
        assert!(f.run(Plan { branches }).result.is_ok());
        assert!(
            f.git(&["for-each-ref", "refs/heads/large-cleanup/"])
                .is_empty()
        );
        assert!(execute(&f.root, Request::MoveTo(0)).result.is_ok());
        assert_eq!(
            f.git(&["for-each-ref", "refs/heads/large-cleanup/"])
                .lines()
                .count(),
            300
        );
        assert_eq!(refs_at(&f.bare), server);
    }
    #[test]
    fn cleanup_protects_default_names_and_optimize_retains_recovery() {
        let f = Fixture::new();
        assert!(f.candidate("master").protected);
        let branch = f.candidate("fixes/task-123");
        let dangling = f.git(&["commit-tree", "HEAD^{tree}", "-m", "preserve"]);
        assert!(
            f.run(Plan {
                branches: vec![branch.clone()]
            })
            .result
            .is_ok()
        );
        assert!(execute(&f.root, Request::OptimizeStorage).result.is_ok());
        f.git(&["cat-file", "-e", &dangling]);
        assert!(execute(&f.root, Request::MoveTo(0)).result.is_ok());
        assert_eq!(
            f.git(&["rev-parse", "refs/remotes/origin/fixes/task-123"]),
            branch.oid
        );
    }
    #[test]
    #[ignore = "one warmup and fifteen cleanup samples"]
    fn benchmark_cleanup() {
        benchmark(0, false);
    }
    #[test]
    #[ignore = "one warmup and fifteen 1500 branch samples"]
    fn benchmark_cleanup_large() {
        benchmark(1500, false);
    }
    #[test]
    #[ignore = "one warmup and fifteen 1500 matching local/cache samples"]
    fn benchmark_cleanup_matching_large() {
        benchmark(1500, true);
    }
    #[test]
    #[ignore = "one warmup and fifteen remote verification/local cleanup samples"]
    fn benchmark_cleanup_remote() {
        benchmark(0, true);
    }
    fn benchmark(count: usize, remote: bool) {
        for sample in -1..15 {
            let f = Fixture::new();
            if count > 0 {
                large_branches(&f, count);
                if remote {
                    let oid = f.git(&["rev-parse", "old/local"]);
                    let mut heads = String::from("start\n");
                    let mut cached = String::from("start\n");
                    for i in 0..count {
                        heads += &format!("create refs/heads/large-cleanup/{i:04} {oid}\n");
                        cached +=
                            &format!("create refs/remotes/origin/large-cleanup/{i:04} {oid}\n");
                    }
                    heads += "prepare\ncommit\n";
                    cached += "prepare\ncommit\n";
                    run(
                        &f.bare,
                        &["update-ref", "--stdin"],
                        Some(heads.as_bytes()),
                        &[],
                    )
                    .unwrap();
                    run(
                        &f.root,
                        &["update-ref", "--stdin"],
                        Some(cached.as_bytes()),
                        &[],
                    )
                    .unwrap();
                }
            }
            let server = refs_at(&f.bare);
            let start = std::time::Instant::now();
            let analysis = analyze(&f.root, crate::ui::time::now_unix()).unwrap();
            let analysis_ms = start.elapsed().as_secs_f64() * 1000.;
            let branches = analysis
                .candidates
                .into_iter()
                .filter(|c| {
                    if count > 0 {
                        c.name.starts_with("large-cleanup/")
                    } else if remote {
                        c.name == "fixes/task-123"
                    } else {
                        !c.protected
                    }
                })
                .collect();
            let start = std::time::Instant::now();
            let result = f.run(Plan { branches });
            assert!(result.result.is_ok(), "{:?}", result.result);
            let delete_ms = start.elapsed().as_secs_f64() * 1000.;
            assert_eq!(refs_at(&f.bare), server);
            let start = std::time::Instant::now();
            assert!(execute(&f.root, Request::MoveTo(0)).result.is_ok());
            let undo_ms = start.elapsed().as_secs_f64() * 1000.;
            let start = std::time::Instant::now();
            assert!(execute(&f.root, Request::OptimizeStorage).result.is_ok());
            println!(
                "{}",
                serde_json::json!({"sample":sample,"branches":count,"analysisMs":analysis_ms,"deleteMs":delete_ms,"undoMs":undo_ms,"repackMs":start.elapsed().as_secs_f64()*1000.})
            );
        }
    }
}
