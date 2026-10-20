//! Conservative branch switching. Git owns index/worktree locking and dirty-file checks.
use super::command;
use crate::branches::Target;
use std::path::Path;
use std::time::Duration;

pub fn execute(path: &Path, request: &crate::checkout::Request) -> Result<String, String> {
    match request {
        crate::checkout::Request::Branch(target) => checkout(path, target),
        crate::checkout::Request::FetchBranch(target) => checkout_inner(path, target, true),
        crate::checkout::Request::Commit { id, remote } => {
            if output(path, &["rev-parse", "--is-bare-repository"])? == "true" {
                return Err("Checkout needs a working tree; this is a bare repository".into());
            }
            let sha = id.to_string();
            let object = command::run(
                path,
                &["cat-file", "-e", &format!("{sha}^{{commit}}")],
                None,
                Duration::from_secs(10),
            )?;
            if !object.status.success() {
                let remote = remote
                    .as_ref()
                    .ok_or("Commit is unavailable locally; refresh or fetch it with Git first")?;
                if remote.name.starts_with('-') || remote.name.is_empty() {
                    return Err("Invalid remote name".into());
                }
                let url = output(
                    path,
                    &["config", "--get", &format!("remote.{}.url", remote.name)],
                )?;
                if url != remote.url {
                    return Err(
                        "Remote URL changed; refresh before checking out this commit".into(),
                    );
                }
                output(
                    path,
                    &[
                        "-c",
                        "fetch.writeCommitGraph=false",
                        "fetch",
                        "--no-tags",
                        "--no-recurse-submodules",
                        "--no-write-fetch-head",
                        "--no-prune",
                        "--refmap=",
                        "--",
                        &remote.name,
                        &sha,
                    ],
                )?;
            }
            output(path, &["switch", "--detach", "--no-guess", "--", &sha])?;
            Ok(format!(
                "Checked out {} (detached HEAD)",
                id.to_hex_with_len(12)
            ))
        }
    }
}

fn output(path: &Path, args: &[&str]) -> Result<String, String> {
    Ok(String::from_utf8_lossy(&command::checked(path, args)?)
        .trim()
        .to_string())
}

fn exists(path: &Path, reference: &str) -> Result<bool, String> {
    let out = command::run(
        path,
        &["show-ref", "--verify", "--quiet", reference],
        None,
        Duration::from_secs(10),
    )?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err("Could not inspect branch refs".into()),
    }
}

pub fn checkout(path: &Path, target: &Target) -> Result<String, String> {
    checkout_inner(path, target, false)
}

fn fetch_selected(
    path: &Path,
    remote: &str,
    merge_ref: &str,
    tracking: &str,
) -> Result<(), String> {
    let refspec = format!("+{merge_ref}:{tracking}");
    output(
        path,
        &[
            "-c",
            "fetch.writeCommitGraph=false",
            "fetch",
            "--no-tags",
            "--no-recurse-submodules",
            "--no-write-fetch-head",
            "--no-prune",
            "--no-prune-tags",
            "--refmap=",
            "--",
            remote,
            &refspec,
        ],
    )?;
    Ok(())
}

fn checkout_inner(path: &Path, target: &Target, fetch_and_ff: bool) -> Result<String, String> {
    if fetch_and_ff && (target.remote.is_none() || target.url.is_none()) {
        return Err(
            "Remote branch details are unavailable; refresh before fetching and checking out"
                .into(),
        );
    }
    if output(path, &["rev-parse", "--is-bare-repository"])? == "true" {
        return Err("Checkout needs a working tree; this is a bare repository".into());
    }
    // Reject revision expressions, option-like names and checkout shortcuts (@{-1}).
    if target.name.starts_with('-')
        || target.name == "HEAD"
        || target.name.contains('@') && target.name.contains('{')
    {
        return Err("Invalid branch name".into());
    }
    output(
        path,
        &["check-ref-format", &format!("refs/heads/{}", target.name)],
    )?;
    let local_ref = format!("refs/heads/{}", target.name);
    let tracking_ref = target
        .remote
        .as_ref()
        .map(|remote| format!("refs/remotes/{remote}/{}", target.name));
    let mut names = vec![local_ref.as_str()];
    if let Some(tracking) = &tracking_ref {
        names.push(tracking);
    }
    super::refs::ensure_case_safe(path, &names)?;
    let Some(remote) = &target.remote else {
        if !exists(path, &format!("refs/heads/{}", target.name))? {
            return Err("Local branch no longer exists; refresh the branch list".into());
        }
        output(path, &["switch", "--no-guess", "--", &target.name])?;
        return Ok(format!("Checked out {}", target.name));
    };
    if remote.starts_with('-') {
        return Err("Invalid remote name".into());
    }
    let remote_url = output(path, &["config", "--get", &format!("remote.{remote}.url")])?;
    if let Some(expected) = &target.url
        && &remote_url != expected
    {
        return Err("Remote URL changed; refresh the branch list before checking out".into());
    }
    let merge_ref = format!("refs/heads/{}", target.name);
    // Prefer an existing tracking branch (including a differently named local branch).
    let locals = output(
        path,
        &[
            "for-each-ref",
            "--format=%(refname:strip=2)%09%(upstream:remotename)%09%(upstream:remoteref)",
            "refs/heads/",
        ],
    )?;
    let mut matches = Vec::new();
    for line in locals.lines() {
        let parts: Vec<_> = line.split('\t').collect();
        if parts.len() == 3 && parts[1] == remote && parts[2] == merge_ref {
            matches.push(parts[0]);
        }
    }
    let existing = if matches.contains(&target.name.as_str()) {
        Some(target.name.as_str())
    } else if matches.len() == 1 {
        Some(matches[0])
    } else {
        None
    };
    if let Some(local) = existing {
        if fetch_and_ff {
            // The advertised SHA may already be stale. Fetch the named branch and
            // use its actual tip, never resetting an existing local branch to API data.
            let tracking = format!("refs/remotes/{remote}/{}", target.name);
            output(path, &["check-ref-format", &tracking])?;
            fetch_selected(path, remote, &merge_ref, &tracking)?;
            let ancestor = command::run(
                path,
                &[
                    "merge-base",
                    "--is-ancestor",
                    &format!("refs/heads/{local}"),
                    &tracking,
                ],
                None,
                Duration::from_secs(10),
            )?;
            if !ancestor.status.success() {
                return Err(format!(
                    "Fetched {remote}/{}; local branch '{local}' has commits outside the remote tip. Select the local branch and merge or rebase explicitly; checkout did not move it.",
                    target.name
                ));
            }
            output(path, &["switch", "--no-guess", "--", local])?;
            output(
                path,
                &[
                    "-c",
                    "merge.autoStash=false",
                    "merge",
                    "--ff-only",
                    "--no-edit",
                    "--no-stat",
                    &tracking,
                ],
            )?;
            return Ok(format!(
                "Fetched and checked out {local} (fast-forward only)"
            ));
        }
        output(path, &["switch", "--no-guess", "--", local])?;
        return Ok(format!(
            "Checked out {local} (existing local tracking branch; no pull)"
        ));
    }
    if matches.len() > 1 {
        return Err(
            "Several local branches track this remote branch; select a local branch explicitly"
                .into(),
        );
    }
    if exists(path, &merge_ref)? {
        return Err(format!(
            "Local branch '{}' already exists with a different upstream. Select it from the local list or rename it in Git.",
            target.name
        ));
    }
    let tracking = format!("refs/remotes/{remote}/{}", target.name);
    output(path, &["check-ref-format", &tracking])?;
    let sha = gix::ObjectId::from_hex(target.sha.as_bytes())
        .map_err(|_| "Invalid remote commit ID".to_string())?
        .to_string();
    let object = command::run(
        path,
        &["cat-file", "-e", &format!("{sha}^{{commit}}")],
        None,
        Duration::from_secs(10),
    )?;
    // API data tells us the tip but does not contain commit/tree/blob objects.
    // Local tracking entries have no API freshness information: refresh only that branch.
    if fetch_and_ff || target.url.is_none() || !object.status.success() {
        fetch_selected(path, remote, &merge_ref, &tracking)?;
    } else {
        // Only update this remote-tracking ref. Never reset a local branch.
        output(path, &["update-ref", &tracking, &sha])?;
    }
    // The temporary mapping also supports clones made with --single-branch.
    // Git switch creates the local branch and its upstream transactionally with checkout.
    let mapping = format!("remote.{remote}.fetch=+refs/heads/*:refs/remotes/{remote}/*");
    output(
        path,
        &[
            "-c",
            &mapping,
            "switch",
            "--no-guess",
            "--track=direct",
            "-c",
            &target.name,
            &tracking,
        ],
    )?;
    // Persist only the selected branch's mapping for a single-branch/custom-refspec
    // clone, so @{upstream} and subsequent Git pulls can resolve its upstream.
    let fetch_key = format!("remote.{remote}.fetch");
    let refspecs = output(path, &["config", "--get-all", &fetch_key]).unwrap_or_default();
    let wildcard = format!("refs/heads/*:refs/remotes/{remote}/*");
    let exact = format!("{merge_ref}:{tracking}");
    if !refspecs.lines().any(|s| {
        let s = s.trim_start_matches('+');
        s == wildcard || s == exact
    }) && output(path, &["config", "--add", &fetch_key, &format!("+{exact}")]).is_err()
    {
        return Ok(format!(
            "Checked out {}; could not save the upstream fetch mapping in Git config",
            target.name
        ));
    }
    Ok(format!(
        "Checked out {} tracking {remote}/{}",
        target.name, target.name
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Fixture {
        _dir: tempfile::TempDir,
        source: PathBuf,
        repo: PathBuf,
        main: String,
        topic: String,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let source = dir.path().join("source");
            let repo = dir.path().join("clone");
            std::fs::create_dir(&source).unwrap();
            output(&source, &["init", "-b", "main"]).unwrap();
            output(&source, &["config", "core.autocrlf", "false"]).unwrap();
            output(
                &source,
                &["config", "core.hooksPath", "disabled-test-hooks"],
            )
            .unwrap();
            std::fs::write(source.join("file.txt"), "base\n").unwrap();
            output(&source, &["add", "."]).unwrap();
            commit(&source);
            let main = output(&source, &["rev-parse", "HEAD"]).unwrap();
            output(&source, &["switch", "-c", "topic/nested"]).unwrap();
            std::fs::write(source.join("file.txt"), "topic\n").unwrap();
            output(&source, &["add", "."]).unwrap();
            commit(&source);
            let topic = output(&source, &["rev-parse", "HEAD"]).unwrap();
            output(&source, &["branch", "unselected"]).unwrap();
            output(&source, &["tag", "unselected-tag"]).unwrap();
            output(&source, &["switch", "main"]).unwrap();
            output(
                dir.path(),
                &[
                    "-c",
                    "core.autocrlf=false",
                    "clone",
                    "--single-branch",
                    "--no-local",
                    "--no-tags",
                    "--branch",
                    "main",
                    source.to_str().unwrap(),
                    repo.to_str().unwrap(),
                ],
            )
            .unwrap();
            output(&repo, &["config", "core.hooksPath", "disabled-test-hooks"]).unwrap();
            output(&repo, &["config", "core.autocrlf", "false"]).unwrap();
            Self {
                _dir: dir,
                source,
                repo,
                main,
                topic,
            }
        }
        fn remote(&self) -> Target {
            Target {
                name: "topic/nested".into(),
                remote: Some("origin".into()),
                url: Some(self.source.to_str().unwrap().into()),
                sha: self.topic.clone(),
            }
        }
        fn local(&self, name: &str) -> Target {
            Target {
                name: name.into(),
                remote: None,
                url: None,
                sha: self.main.clone(),
            }
        }
        fn advance(&self, content: &str) -> String {
            output(&self.source, &["switch", "topic/nested"]).unwrap();
            std::fs::write(self.source.join("file.txt"), content).unwrap();
            output(&self.source, &["add", "."]).unwrap();
            commit(&self.source);
            output(&self.source, &["rev-parse", "HEAD"]).unwrap()
        }
    }

    fn commit(path: &Path) {
        output(
            path,
            &[
                "-c",
                "user.name=Rustle Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgSign=false",
                "commit",
                "-m",
                "test",
            ],
        )
        .unwrap();
    }

    #[test]
    fn detached_local_checkout_does_not_fetch_or_move_branches() {
        let f = Fixture::new();
        output(
            &f.repo,
            &[
                "remote",
                "set-url",
                "origin",
                "https://unavailable.invalid/repo",
            ],
        )
        .unwrap();
        let refs = output(&f.repo, &["show-ref"]).unwrap();
        let request = crate::checkout::Request::Commit {
            id: gix::ObjectId::from_hex(f.main.as_bytes()).unwrap(),
            remote: None,
        };
        execute(&f.repo, &request).unwrap();
        assert!(
            output(&f.repo, &["branch", "--show-current"])
                .unwrap()
                .is_empty()
        );
        assert_eq!(output(&f.repo, &["rev-parse", "HEAD"]).unwrap(), f.main);
        assert_eq!(output(&f.repo, &["show-ref"]).unwrap(), refs);
        execute(&f.repo, &crate::checkout::Request::Branch(f.local("main"))).unwrap();
        assert_eq!(
            output(&f.repo, &["branch", "--show-current"]).unwrap(),
            "main"
        );
    }

    #[test]
    fn detached_remote_checkout_downloads_only_the_commit_without_writing_refs() {
        let f = Fixture::new();
        let remote = crate::remote::Remote {
            name: "origin".into(),
            url: f.source.to_string_lossy().into_owned(),
            provider: crate::remote::Provider::Github,
            endpoint: "https://unused.invalid/branches".into(),
            project: "fixture".into(),
        };
        let id = gix::ObjectId::from_hex(f.topic.as_bytes()).unwrap();
        let refs = output(&f.repo, &["show-ref"]).unwrap();
        let request = crate::checkout::Request::Commit {
            id,
            remote: Some(remote.clone()),
        };
        let mut wrong = remote;
        wrong.url = "https://wrong.invalid/repo".into();
        assert!(
            execute(
                &f.repo,
                &crate::checkout::Request::Commit {
                    id,
                    remote: Some(wrong)
                }
            )
            .unwrap_err()
            .contains("Remote URL changed")
        );
        assert!(
            execute(
                &f.repo,
                &crate::checkout::Request::Commit { id, remote: None }
            )
            .is_err()
        );
        execute(&f.repo, &request).unwrap();
        assert_eq!(output(&f.repo, &["rev-parse", "HEAD"]).unwrap(), f.topic);
        assert!(
            output(&f.repo, &["branch", "--show-current"])
                .unwrap()
                .is_empty()
        );
        assert_eq!(output(&f.repo, &["show-ref"]).unwrap(), refs);
        assert!(!f.repo.join(".git/FETCH_HEAD").exists());
        assert!(!exists(&f.repo, "refs/tags/unselected-tag").unwrap());
        output(&f.repo, &["switch", "main"]).unwrap();
        std::fs::write(f.repo.join("file.txt"), "unsaved\n").unwrap();
        assert!(execute(&f.repo, &request).is_err());
        assert_eq!(
            std::fs::read_to_string(f.repo.join("file.txt")).unwrap(),
            "unsaved\n"
        );
        assert_eq!(
            output(&f.repo, &["branch", "--show-current"]).unwrap(),
            "main"
        );
    }

    #[test]
    fn fetches_only_selected_branch_and_preserves_existing_tracking_tip() {
        let f = Fixture::new();
        assert!(
            !command::run(
                &f.repo,
                &["cat-file", "-e", &f.topic],
                None,
                Duration::from_secs(10)
            )
            .unwrap()
            .status
            .success()
        );
        checkout(&f.repo, &f.remote()).unwrap();
        assert_eq!(output(&f.repo, &["rev-parse", "HEAD"]).unwrap(), f.topic);
        assert_eq!(
            output(&f.repo, &["config", "branch.topic/nested.remote"]).unwrap(),
            "origin"
        );
        assert_eq!(
            output(&f.repo, &["config", "branch.topic/nested.merge"]).unwrap(),
            "refs/heads/topic/nested"
        );
        assert!(!exists(&f.repo, "refs/remotes/origin/unselected").unwrap());
        assert!(!exists(&f.repo, "refs/tags/unselected-tag").unwrap());
        checkout(&f.repo, &f.local("main")).unwrap();
        // Existing branch must remain usable offline and must never be reset to a supplied API tip.
        let mut target = f.remote();
        target.sha = f.main.clone();
        checkout(&f.repo, &target).unwrap();
        assert_eq!(output(&f.repo, &["rev-parse", "HEAD"]).unwrap(), f.topic);
    }

    #[test]
    fn ghost_checkout_fetches_and_fast_forwards_existing_current_tracking_branch() {
        let f = Fixture::new();
        checkout(&f.repo, &f.remote()).unwrap();
        let tip = f.advance("advanced\n");
        output(&f.repo, &["config", "fetch.prune", "true"]).unwrap();
        output(&f.repo, &["config", "fetch.pruneTags", "true"]).unwrap();
        output(&f.repo, &["tag", "local-only-tag"]).unwrap();
        output(
            &f.repo,
            &["update-ref", "refs/remotes/origin/local-only", &f.main],
        )
        .unwrap();
        assert!(
            !command::run(
                &f.repo,
                &["cat-file", "-e", &tip],
                None,
                Duration::from_secs(10)
            )
            .unwrap()
            .status
            .success()
        );
        let mut target = f.remote();
        target.sha = tip.clone();
        execute(&f.repo, &crate::checkout::Request::FetchBranch(target)).unwrap();
        assert_eq!(output(&f.repo, &["rev-parse", "HEAD"]).unwrap(), tip);
        assert_eq!(
            output(&f.repo, &["branch", "--show-current"]).unwrap(),
            "topic/nested"
        );
        assert_eq!(output(&f.repo, &["rev-parse", "@{upstream}"]).unwrap(), tip);
        assert_eq!(
            std::fs::read_to_string(f.repo.join("file.txt")).unwrap(),
            "advanced\n"
        );
        assert!(!exists(&f.repo, "refs/remotes/origin/unselected").unwrap());
        assert!(!exists(&f.repo, "refs/tags/unselected-tag").unwrap());
        assert_eq!(
            output(&f.repo, &["rev-parse", "refs/tags/local-only-tag"]).unwrap(),
            f.topic
        );
        assert_eq!(
            output(&f.repo, &["rev-parse", "refs/remotes/origin/local-only"]).unwrap(),
            f.main
        );
        assert!(!f.repo.join(".git/FETCH_HEAD").exists());
    }

    #[test]
    fn ghost_checkout_uses_fetched_tip_for_new_and_renamed_tracking_branches() {
        let f = Fixture::new();
        // The API advertises an old, locally available object; explicit fetch
        // must still obtain the current branch tip rather than trusting that SHA.
        let mut stale = f.remote();
        stale.sha = f.main.clone();
        execute(
            &f.repo,
            &crate::checkout::Request::FetchBranch(stale.clone()),
        )
        .unwrap();
        assert_eq!(output(&f.repo, &["rev-parse", "HEAD"]).unwrap(), f.topic);
        output(&f.repo, &["branch", "-m", "renamed-local"]).unwrap();
        output(&f.repo, &["switch", "main"]).unwrap();
        let tip = f.advance("latest remote\n");
        execute(&f.repo, &crate::checkout::Request::FetchBranch(stale)).unwrap();
        assert_eq!(
            output(&f.repo, &["branch", "--show-current"]).unwrap(),
            "renamed-local"
        );
        assert_eq!(output(&f.repo, &["rev-parse", "HEAD"]).unwrap(), tip);
        assert_eq!(output(&f.repo, &["rev-parse", "@{upstream}"]).unwrap(), tip);
        assert!(!exists(&f.repo, "refs/heads/topic/nested").unwrap());
        assert!(!exists(&f.repo, "refs/remotes/origin/unselected").unwrap());
        assert!(!exists(&f.repo, "refs/tags/unselected-tag").unwrap());
    }

    #[test]
    fn ghost_checkout_rejects_ahead_and_diverged_branches_without_switching_or_resetting() {
        for diverged in [false, true] {
            let f = Fixture::new();
            checkout(&f.repo, &f.remote()).unwrap();
            std::fs::write(f.repo.join("local.txt"), "local commit\n").unwrap();
            output(&f.repo, &["add", "."]).unwrap();
            commit(&f.repo);
            let local = output(&f.repo, &["rev-parse", "HEAD"]).unwrap();
            output(&f.repo, &["switch", "main"]).unwrap();
            std::fs::write(f.repo.join("untracked.txt"), "keep me\n").unwrap();
            let mut target = f.remote();
            if diverged {
                target.sha = f.advance("remote commit\n");
            }
            let error =
                execute(&f.repo, &crate::checkout::Request::FetchBranch(target)).unwrap_err();
            assert!(error.contains("merge or rebase explicitly"), "{error}");
            assert_eq!(output(&f.repo, &["rev-parse", "HEAD"]).unwrap(), f.main);
            assert_eq!(
                output(&f.repo, &["rev-parse", "refs/heads/topic/nested"]).unwrap(),
                local
            );
            assert_eq!(
                output(&f.repo, &["branch", "--show-current"]).unwrap(),
                "main"
            );
            assert_eq!(
                std::fs::read_to_string(f.repo.join("untracked.txt")).unwrap(),
                "keep me\n"
            );
        }
    }

    #[test]
    fn ghost_checkout_preserves_dirty_files_and_linked_worktree_branch() {
        let f = Fixture::new();
        checkout(&f.repo, &f.remote()).unwrap();
        let tip = f.advance("remote changed\n");
        output(&f.repo, &["config", "merge.autoStash", "true"]).unwrap();
        std::fs::write(f.repo.join("file.txt"), "unsaved local\n").unwrap();
        let mut target = f.remote();
        target.sha = tip;
        assert!(
            execute(
                &f.repo,
                &crate::checkout::Request::FetchBranch(target.clone())
            )
            .is_err()
        );
        assert_eq!(output(&f.repo, &["rev-parse", "HEAD"]).unwrap(), f.topic);
        assert_eq!(
            std::fs::read_to_string(f.repo.join("file.txt")).unwrap(),
            "unsaved local\n"
        );
        assert!(!exists(&f.repo, "refs/stash").unwrap());
        output(&f.repo, &["restore", "file.txt"]).unwrap();
        output(&f.repo, &["switch", "main"]).unwrap();
        let linked = f._dir.path().join("linked");
        output(
            &f.repo,
            &["worktree", "add", linked.to_str().unwrap(), "topic/nested"],
        )
        .unwrap();
        assert!(execute(&f.repo, &crate::checkout::Request::FetchBranch(target)).is_err());
        assert_eq!(output(&f.repo, &["rev-parse", "HEAD"]).unwrap(), f.main);
        assert_eq!(output(&linked, &["rev-parse", "HEAD"]).unwrap(), f.topic);
        assert_eq!(
            std::fs::read_to_string(linked.join("file.txt")).unwrap(),
            "topic\n"
        );
    }

    #[test]
    fn ordinary_existing_branch_checkout_stays_offline_even_with_missing_advertised_tip() {
        let f = Fixture::new();
        checkout(&f.repo, &f.remote()).unwrap();
        let missing_tip = f.advance("not fetched\n");
        let missing = f._dir.path().join("offline-remote");
        output(
            &f.repo,
            &["remote", "set-url", "origin", missing.to_str().unwrap()],
        )
        .unwrap();
        let mut target = f.remote();
        target.url = Some(missing.to_string_lossy().into_owned());
        target.sha = missing_tip;
        execute(&f.repo, &crate::checkout::Request::Branch(target)).unwrap();
        assert_eq!(output(&f.repo, &["rev-parse", "HEAD"]).unwrap(), f.topic);
    }

    #[test]
    #[ignore = "explicit ghost branch fetch and fast-forward worker measurement"]
    fn benchmark_ghost_checkout() {
        for sample in -1..15 {
            for branch in ["existing", "new"] {
                let f = Fixture::new();
                if branch == "existing" {
                    checkout(&f.repo, &f.remote()).unwrap();
                }
                let tip = f.advance(&format!("sample {sample} {branch}\n"));
                assert!(
                    !command::run(
                        &f.repo,
                        &["cat-file", "-e", &tip],
                        None,
                        Duration::from_secs(10)
                    )
                    .unwrap()
                    .status
                    .success()
                );
                let mut target = f.remote();
                target.sha = tip.clone();
                let request = crate::checkout::Request::FetchBranch(target);
                let start = std::time::Instant::now();
                execute(&f.repo, &request).unwrap();
                let checkout_ms = start.elapsed().as_secs_f64() * 1000.;
                assert_eq!(output(&f.repo, &["rev-parse", "HEAD"]).unwrap(), tip);
                assert_eq!(
                    output(&f.repo, &["branch", "--show-current"]).unwrap(),
                    "topic/nested"
                );
                assert_eq!(output(&f.repo, &["rev-parse", "@{upstream}"]).unwrap(), tip);
                assert!(!exists(&f.repo, "refs/remotes/origin/unselected").unwrap());
                assert!(!exists(&f.repo, "refs/tags/unselected-tag").unwrap());
                if sample >= 0 {
                    println!(
                        "{}",
                        serde_json::json!({"sample": sample, "branch": branch, "checkout_ms": checkout_ms})
                    );
                }
            }
        }
    }

    #[test]
    fn reftable_fetch_and_checkout_keep_case_distinct_branches_independent() {
        let f = Fixture::new();
        super::super::refs::migrate_reftable(&f.source).unwrap();
        output(&f.source, &["branch", "Case", &f.main]).unwrap();
        output(&f.source, &["branch", "case", &f.topic]).unwrap();
        super::super::refs::migrate_reftable(&f.repo).unwrap();
        for (name, sha, content) in [("Case", &f.main, "base\n"), ("case", &f.topic, "topic\n")] {
            // url=None forces the real bounded fetch path even when an object
            // is already known, as happens with a local tracking entry.
            let target = Target {
                name: name.into(),
                remote: Some("origin".into()),
                url: None,
                sha: sha.clone(),
            };
            checkout(&f.repo, &target).unwrap();
            assert_eq!(
                output(&f.repo, &["symbolic-ref", "--short", "HEAD"]).unwrap(),
                name
            );
            assert_eq!(output(&f.repo, &["rev-parse", "HEAD"]).unwrap(), *sha);
            assert_eq!(
                std::fs::read_to_string(f.repo.join("file.txt")).unwrap(),
                content
            );
        }
        assert_eq!(
            output(&f.repo, &["rev-parse", "refs/remotes/origin/Case"]).unwrap(),
            f.main
        );
        assert_eq!(
            output(&f.repo, &["rev-parse", "refs/remotes/origin/case"]).unwrap(),
            f.topic
        );
        assert_eq!(
            output(&f.repo, &["rev-parse", "refs/heads/Case"]).unwrap(),
            f.main
        );
        assert_eq!(
            output(&f.repo, &["rev-parse", "refs/heads/case"]).unwrap(),
            f.topic
        );
        assert!(!exists(&f.repo, "refs/remotes/origin/unselected").unwrap());
    }

    #[test]
    #[cfg(windows)]
    fn files_storage_rejects_case_alias_before_fetch_or_branch_changes() {
        let f = Fixture::new();
        output(&f.repo, &["branch", "Case", &f.main]).unwrap();
        let before = output(&f.repo, &["show-ref"]).unwrap();
        let target = Target {
            name: "case".into(),
            remote: Some("origin".into()),
            url: None,
            sha: f.topic.clone(),
        };
        let error = checkout(&f.repo, &target).unwrap_err();
        assert!(error.contains("reftable"), "{error}");
        assert_eq!(output(&f.repo, &["show-ref"]).unwrap(), before);
        assert_eq!(
            output(&f.repo, &["symbolic-ref", "--short", "HEAD"]).unwrap(),
            "main"
        );
        assert_eq!(
            std::fs::read_to_string(f.repo.join("file.txt")).unwrap(),
            "base\n"
        );
    }

    #[test]
    #[ignore = "explicit case-distinct reftable fetch/checkout worker measurement"]
    fn benchmark_case_distinct_checkout() {
        let f = Fixture::new();
        super::super::refs::migrate_reftable(&f.source).unwrap();
        output(&f.source, &["branch", "Case", &f.main]).unwrap();
        output(&f.source, &["branch", "case", &f.topic]).unwrap();
        super::super::refs::migrate_reftable(&f.repo).unwrap();
        for sample in -1..15 {
            for (name, sha) in [("Case", &f.main), ("case", &f.topic)] {
                output(&f.repo, &["switch", "main"]).unwrap();
                if exists(&f.repo, &format!("refs/heads/{name}")).unwrap() {
                    output(&f.repo, &["branch", "-D", name]).unwrap();
                }
                let target = Target {
                    name: name.into(),
                    remote: Some("origin".into()),
                    url: None,
                    sha: sha.clone(),
                };
                let start = std::time::Instant::now();
                checkout(&f.repo, &target).unwrap();
                let fetch_ms = start.elapsed().as_secs_f64() * 1000.;
                assert_eq!(output(&f.repo, &["rev-parse", "HEAD"]).unwrap(), *sha);
                output(&f.repo, &["switch", "main"]).unwrap();
                let start = std::time::Instant::now();
                checkout(&f.repo, &target).unwrap();
                let existing_ms = start.elapsed().as_secs_f64() * 1000.;
                assert_eq!(
                    output(&f.repo, &["symbolic-ref", "--short", "HEAD"]).unwrap(),
                    name
                );
                if sample >= 0 {
                    println!(
                        "{}",
                        serde_json::json!({"sample":sample,"branch":name,"fetchCheckoutMs":fetch_ms,"existingCheckoutMs":existing_ms})
                    );
                }
            }
        }
    }

    #[test]
    fn known_object_checkout_does_not_need_network() {
        let f = Fixture::new();
        let missing = f._dir.path().join("missing-remote");
        output(
            &f.repo,
            &["remote", "set-url", "origin", missing.to_str().unwrap()],
        )
        .unwrap();
        let target = Target {
            name: "cached/topic".into(),
            remote: Some("origin".into()),
            url: Some(missing.to_str().unwrap().into()),
            sha: f.main.clone(),
        };
        checkout(&f.repo, &target).unwrap();
        assert_eq!(
            output(&f.repo, &["branch", "--show-current"]).unwrap(),
            "cached/topic"
        );
        assert_eq!(output(&f.repo, &["rev-parse", "HEAD"]).unwrap(), f.main);
    }

    #[test]
    fn dirty_worktree_collision_and_reconfigured_remote_are_safe() {
        let f = Fixture::new();
        std::fs::write(f.repo.join("file.txt"), "unsaved work\n").unwrap();
        assert!(checkout(&f.repo, &f.remote()).is_err());
        assert_eq!(
            std::fs::read_to_string(f.repo.join("file.txt")).unwrap(),
            "unsaved work\n"
        );
        assert_eq!(
            output(&f.repo, &["branch", "--show-current"]).unwrap(),
            "main"
        );
        assert!(!exists(&f.repo, "refs/heads/topic/nested").unwrap());
        output(&f.repo, &["branch", "topic/nested"]).unwrap();
        let error = checkout(&f.repo, &f.remote()).unwrap_err();
        assert!(error.contains("different upstream"), "{error}");
        let mut target = f.remote();
        target.url = Some("https://other.invalid/changed/repo".into());
        assert!(
            checkout(&f.repo, &target)
                .unwrap_err()
                .contains("Remote URL changed")
        );
    }

    #[test]
    fn local_checkout_rejects_overwrite_and_other_worktree() {
        let f = Fixture::new();
        checkout(&f.repo, &f.remote()).unwrap();
        std::fs::write(f.repo.join("file.txt"), "unsaved work\n").unwrap();
        assert!(checkout(&f.repo, &f.local("main")).is_err());
        assert_eq!(
            output(&f.repo, &["branch", "--show-current"]).unwrap(),
            "topic/nested"
        );
        std::fs::write(f.repo.join("file.txt"), "topic\n").unwrap();
        let linked = f._dir.path().join("linked");
        output(
            &f.repo,
            &["worktree", "add", linked.to_str().unwrap(), "main"],
        )
        .unwrap();
        assert!(checkout(&f.repo, &f.local("main")).is_err());
        assert!(checkout(&linked, &f.local("topic/nested")).is_err());
        output(&f.repo, &["branch", "free"]).unwrap();
        checkout(&linked, &f.local("free")).unwrap();
        assert_eq!(
            output(&linked, &["branch", "--show-current"]).unwrap(),
            "free"
        );
    }

    #[test]
    #[ignore = "explicit network checkout verification in the marked synthetic GitHub fixture"]
    fn live_case_distinct_github_checkout() {
        let path = std::path::PathBuf::from(
            std::env::var_os("GIT_RUSTLE_LIVE_CASE_REPO")
                .expect("GIT_RUSTLE_LIVE_CASE_REPO must name the disposable test clone"),
        );
        assert_eq!(
            std::fs::read_to_string(path.join(".git/git-rustle-live-case-fixture")).unwrap(),
            "disposable Git Rustle case fixture"
        );
        assert_eq!(
            output(&path, &["config", "remote.origin.url"]).unwrap(),
            "https://github.com/pascaloon/git-rustle-case-branches-tests.git"
        );
        assert!(super::super::refs::is_reftable(
            &super::super::open(&path).unwrap()
        ));
        assert!(
            output(&path, &["status", "--porcelain=v1"])
                .unwrap()
                .is_empty()
        );
        let main = output(&path, &["rev-parse", "refs/heads/main"]).unwrap();
        for name in [
            "CaseFeature",
            "casefeature",
            "Feature/Topic",
            "feature/Other",
        ] {
            let remote_ref = format!("refs/remotes/origin/{name}");
            let sha = output(&path, &["rev-parse", &remote_ref]).unwrap();
            // The production worker fetches only the selected branch and adds
            // its exact upstream mapping even in a single-branch clone.
            let message = checkout(
                &path,
                &Target {
                    name: name.into(),
                    remote: Some("origin".into()),
                    url: None,
                    sha: sha.clone(),
                },
            )
            .unwrap();
            assert_eq!(
                output(&path, &["symbolic-ref", "--short", "HEAD"]).unwrap(),
                name
            );
            assert_eq!(output(&path, &["rev-parse", "HEAD"]).unwrap(), sha);
            assert_eq!(output(&path, &["rev-parse", "@{upstream}"]).unwrap(), sha);
            assert!(
                output(&path, &["status", "--porcelain=v1"])
                    .unwrap()
                    .is_empty()
            );
            println!("live_case_branch={name} head={sha} result={message}");
        }
        assert_eq!(
            output(&path, &["rev-parse", "refs/heads/main"]).unwrap(),
            main
        );
        assert_ne!(
            output(&path, &["rev-parse", "refs/heads/CaseFeature"]).unwrap(),
            output(&path, &["rev-parse", "refs/heads/casefeature"]).unwrap()
        );
    }

    #[test]
    fn bare_and_invalid_branches_are_rejected() {
        let f = Fixture::new();
        for name in ["--force", "@{-1}", "HEAD", "bad\nname", "x:refs/heads/main"] {
            assert!(checkout(&f.repo, &f.local(name)).is_err());
        }
        let bare = f._dir.path().join("bare");
        output(f._dir.path(), &["init", "--bare", bare.to_str().unwrap()]).unwrap();
        assert!(
            checkout(&bare, &f.local("main"))
                .unwrap_err()
                .contains("bare repository")
        );
    }
}
