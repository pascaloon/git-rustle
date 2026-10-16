//! Conservative branch switching. Git owns index/worktree locking and dirty-file checks.
use super::command;
use crate::branches::Target;
use std::path::Path;
use std::time::Duration;

pub fn execute(path: &Path, request: &crate::checkout::Request) -> Result<String, String> {
    match request {
        crate::checkout::Request::Branch(target) => checkout(path, target),
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
    if target.url.is_none() || !object.status.success() {
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
                "--refmap=",
                "--",
                remote,
                &refspec,
            ],
        )?;
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
