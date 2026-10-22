use super::*;

struct Fixture {
    dir: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let f = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        f.git(&["init", "-b", "main"]);
        f.git(&["config", "user.name", "Rustle Test"]);
        f.git(&["config", "user.email", "test@example.invalid"]);
        f.git(&["config", "commit.gpgSign", "false"]);
        f.git(&["config", "core.autocrlf", "false"]);
        f.git(&["config", "core.hooksPath", "disabled-hooks"]);
        f.write("file.txt", "base\n");
        f.commit("base");
        f
    }
    fn root(&self) -> &Path {
        self.dir.path()
    }
    fn git(&self, args: &[&str]) -> String {
        output(self.root(), args).unwrap()
    }
    fn write(&self, path: &str, text: &str) {
        fs::write(self.root().join(path), text).unwrap();
    }
    fn commit(&self, message: &str) {
        self.git(&["add", "."]);
        self.git(&["commit", "-m", message]);
    }
    fn run(&self, request: Request) -> View {
        let out = execute(self.root(), request);
        assert!(out.result.is_ok(), "{:?}", out.result);
        out.view
    }
    fn conflict(&self) {
        self.git(&["switch", "-c", "topic"]);
        self.write("file.txt", "topic\n");
        self.commit("topic");
        self.git(&["switch", "main"]);
        self.write("file.txt", "main\n");
        self.commit("main");
    }
}

#[test]
fn discard_file_recovery_restores_exact_index_and_worktree_and_preserves_other_files() {
    let f = Fixture::new();
    f.write("other.txt", "other base\n");
    f.commit("other file");
    f.write("file.txt", "staged target\n");
    f.write("other.txt", "staged other\n");
    f.git(&["add", "."]);
    f.write("file.txt", "unstaged target\n");
    f.write("untracked.txt", "untracked preserve\n");
    let snapshot =
        super::super::worktree::snapshot(&super::super::open(f.root()).unwrap()).unwrap();
    let file = snapshot
        .files
        .iter()
        .find(|file| file.path == "file.txt")
        .unwrap();
    let request = super::super::worktree::DiscardRequest::new(&snapshot, file);
    let head = f.git(&["rev-parse", "HEAD"]);
    let original_status = f.git(&["status", "--porcelain=v1"]);
    f.run(Request::DiscardFile(request));
    let check_unrelated = || {
        assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
        assert_eq!(f.git(&["show", ":other.txt"]), "staged other");
        assert_eq!(
            fs::read(f.root().join("other.txt")).unwrap(),
            b"staged other\n"
        );
        assert_eq!(
            fs::read(f.root().join("untracked.txt")).unwrap(),
            b"untracked preserve\n"
        );
    };
    check_unrelated();
    assert_eq!(fs::read(f.root().join("file.txt")).unwrap(), b"base\n");
    assert_eq!(f.git(&["show", ":file.txt"]), "base");
    f.run(Request::MoveTo(0));
    check_unrelated();
    assert_eq!(f.git(&["status", "--porcelain=v1"]), original_status);
    assert_eq!(
        fs::read(f.root().join("file.txt")).unwrap(),
        b"unstaged target\n"
    );
    assert_eq!(f.git(&["show", ":file.txt"]), "staged target");
    f.run(Request::MoveTo(1));
    check_unrelated();
    assert_eq!(fs::read(f.root().join("file.txt")).unwrap(), b"base\n");
    assert_eq!(f.git(&["show", ":file.txt"]), "base");
}

fn stash_fixture(conflict: bool) -> (Fixture, stashes::Stash) {
    let f = Fixture::new();
    f.write("file.txt", "stashed contents\n");
    f.write("stash-only.txt", "untracked stash contents\n");
    f.git(&[
        "stash",
        "push",
        "--include-untracked",
        "-m",
        "stash fixture",
    ]);
    if conflict {
        f.write("file.txt", "new committed contents\n");
        f.commit("diverge");
    }
    let stash = stashes::list(f.root()).unwrap().remove(0);
    (f, stash)
}

#[test]
fn stash_apply_keeps_entry_and_recovers_tracked_and_untracked_files() {
    let (f, stash) = stash_fixture(false);
    let head = f.git(&["rev-parse", "HEAD"]);
    f.run(Request::ApplyStash(stash.clone()));
    assert_eq!(
        fs::read(f.root().join("file.txt")).unwrap(),
        b"stashed contents\n"
    );
    assert_eq!(
        fs::read(f.root().join("stash-only.txt")).unwrap(),
        b"untracked stash contents\n"
    );
    assert_eq!(stashes::list(f.root()).unwrap(), vec![stash]);
    f.run(Request::MoveTo(0));
    assert_eq!(fs::read(f.root().join("file.txt")).unwrap(), b"base\n");
    assert!(!f.root().join("stash-only.txt").exists());
    f.run(Request::MoveTo(1));
    assert_eq!(
        fs::read(f.root().join("stash-only.txt")).unwrap(),
        b"untracked stash contents\n"
    );
    assert_eq!(head, f.git(&["rev-parse", "HEAD"]));
}

#[test]
fn stash_pop_and_preview_include_untracked_and_restore_the_stack_on_undo() {
    let (f, stash) = stash_fixture(false);
    let preview = stashes::preview(f.root(), &stash).unwrap();
    assert_eq!(
        preview
            .changes
            .files
            .iter()
            .map(|f| f.path.as_str())
            .collect::<Vec<_>>(),
        vec!["file.txt", "stash-only.txt"]
    );
    assert!(preview.changes.files.iter().all(|f| !f.worktree));
    f.run(Request::PopStash(stash.clone()));
    assert!(stashes::list(f.root()).unwrap().is_empty());
    assert_eq!(
        fs::read(f.root().join("file.txt")).unwrap(),
        b"stashed contents\n"
    );
    assert_eq!(
        fs::read(f.root().join("stash-only.txt")).unwrap(),
        b"untracked stash contents\n"
    );
    assert!(stashes::preview(f.root(), &stash).is_err());
    f.run(Request::MoveTo(0));
    assert_eq!(stashes::list(f.root()).unwrap(), vec![stash]);
    assert_eq!(fs::read(f.root().join("file.txt")).unwrap(), b"base\n");
    assert!(!f.root().join("stash-only.txt").exists());
    f.run(Request::MoveTo(1));
    assert!(stashes::list(f.root()).unwrap().is_empty());
    assert!(f.root().join("stash-only.txt").exists());
}

#[test]
fn stash_pop_conflict_keeps_stash_and_remains_recoverable() {
    let (f, stash) = stash_fixture(true);
    let out = execute(f.root(), Request::PopStash(stash.clone()));
    assert!(out.result.is_err());
    assert_eq!(out.view.conflict.unwrap().kind, "Stash apply");
    assert_eq!(stashes::list(f.root()).unwrap(), vec![stash.clone()]);
    f.run(Request::Abort);
    assert_eq!(stashes::list(f.root()).unwrap(), vec![stash]);
    assert_eq!(
        fs::read(f.root().join("file.txt")).unwrap(),
        b"new committed contents\n"
    );
}

#[test]
fn discard_batch_is_one_recoverable_action_and_preserves_unselected_files() {
    let f = Fixture::new();
    f.write("file.txt", "staged\n");
    f.git(&["add", "file.txt"]);
    f.write("file.txt", "unstaged\n");
    f.write("new.txt", "new\n");
    f.write("keep.txt", "keep\n");
    let snapshot =
        super::super::worktree::snapshot(&super::super::open(f.root()).unwrap()).unwrap();
    let requests = snapshot
        .files
        .iter()
        .filter(|file| file.path != "keep.txt")
        .map(|file| super::super::worktree::DiscardRequest::new(&snapshot, file))
        .collect();
    f.run(Request::DiscardFiles(requests));
    assert_eq!(fs::read(f.root().join("file.txt")).unwrap(), b"base\n");
    assert!(!f.root().join("new.txt").exists());
    assert_eq!(fs::read(f.root().join("keep.txt")).unwrap(), b"keep\n");
    f.run(Request::MoveTo(0));
    assert_eq!(f.git(&["show", ":file.txt"]), "staged");
    assert_eq!(fs::read(f.root().join("file.txt")).unwrap(), b"unstaged\n");
    assert!(f.root().join("new.txt").exists());
    f.run(Request::MoveTo(1));
    assert!(!f.root().join("new.txt").exists());
    assert!(f.root().join("keep.txt").exists());
}

#[test]
fn stash_drop_re_resolves_frozen_entry_and_undo_restores_entire_stack() {
    let (f, stash) = stash_fixture(false);
    f.write("file.txt", "second stash\n");
    f.git(&["stash", "push", "-m", "newer"]);
    let original = stashes::list(f.root()).unwrap();
    f.run(Request::DropStash(stash.clone()));
    assert_eq!(stashes::list(f.root()).unwrap().len(), 1);
    assert_eq!(stashes::list(f.root()).unwrap()[0].oid, original[0].oid);
    f.run(Request::MoveTo(0));
    assert_eq!(stashes::list(f.root()).unwrap(), original);
    f.run(Request::MoveTo(1));
    assert!(execute(f.root(), Request::DropStash(stash)).result.is_err());
}

#[test]
fn stash_conflict_finish_and_abort_remain_recoverable() {
    for abort in [false, true] {
        let (f, stash) = stash_fixture(true);
        let head = f.git(&["rev-parse", "HEAD"]);
        let result = execute(f.root(), Request::ApplyStash(stash.clone()));
        assert!(result.result.is_err());
        assert_eq!(result.view.conflict.unwrap().kind, "Stash apply");
        if abort {
            f.run(Request::Abort);
            assert_eq!(
                fs::read(f.root().join("file.txt")).unwrap(),
                b"new committed contents\n"
            );
            assert!(f.root().join("stash-only.txt").exists());
        } else {
            f.write("file.txt", "resolved\n");
            let view = f.run(Request::Resolve {
                path: "file.txt".into(),
                choice: Resolution::Mark,
            });
            assert_eq!(view.conflict.unwrap().kind, "Stash apply");
            f.run(Request::Continue);
        }
        assert_eq!(stashes::list(f.root()).unwrap(), vec![stash]);
        assert_eq!(head, f.git(&["rev-parse", "HEAD"]));
        f.run(Request::MoveTo(0));
        assert!(!f.root().join("stash-only.txt").exists());
        f.run(Request::MoveTo(1));
        assert!(f.root().join("stash-only.txt").exists());
    }
}

#[test]
fn stash_abort_checks_packed_detached_and_reftable_heads_without_moving_the_commit() {
    for layout in ["packed", "detached", "reftable"] {
        let (f, stash) = stash_fixture(true);
        match layout {
            "packed" => {
                f.git(&["pack-refs", "--all", "--prune"]);
                assert!(!f.root().join(".git/refs/heads/main").exists());
            }
            "detached" => {
                f.git(&["switch", "--detach"]);
            }
            "reftable" => {
                f.git(&["refs", "migrate", "--ref-format=reftable"]);
            }
            _ => unreachable!(),
        }
        let head = f.git(&["rev-parse", "HEAD^{commit}"]);
        let branch = f.git(&["branch", "--show-current"]);
        let stopped = execute(f.root(), Request::ApplyStash(stash.clone()));
        assert!(stopped.result.is_err());
        assert_eq!(stopped.view.conflict.unwrap().kind, "Stash apply");
        let aborted = f.run(Request::Abort);
        assert!(aborted.conflict.is_none(), "{layout}");
        assert_eq!(f.git(&["rev-parse", "HEAD^{commit}"]), head, "{layout}");
        assert_eq!(f.git(&["branch", "--show-current"]), branch, "{layout}");
        assert_eq!(
            fs::read(f.root().join("file.txt")).unwrap(),
            b"new committed contents\n"
        );
        assert!(f.root().join("stash-only.txt").exists());
        assert_eq!(stashes::list(f.root()).unwrap(), vec![stash]);
    }
}

#[test]
fn stash_abort_refuses_an_externally_changed_head_without_resetting_files_or_index() {
    let (f, stash) = stash_fixture(true);
    assert!(
        execute(f.root(), Request::ApplyStash(stash))
            .result
            .is_err()
    );
    let other = f.git(&["rev-parse", "HEAD~1"]);
    f.git(&["update-ref", "refs/heads/main", &other]);
    let index = fs::read(f.root().join(".git/index")).unwrap();
    let contents = fs::read(f.root().join("file.txt")).unwrap();
    let error = execute(f.root(), Request::Abort).result.unwrap_err();
    assert!(
        error.contains("HEAD changed outside the stash apply"),
        "{error}"
    );
    assert_eq!(f.git(&["rev-parse", "HEAD"]), other);
    assert_eq!(fs::read(f.root().join(".git/index")).unwrap(), index);
    assert_eq!(fs::read(f.root().join("file.txt")).unwrap(), contents);
    assert!(f.root().join(".git/rustle-operations/stash-apply").exists());
}

#[test]
fn stash_abort_fails_closed_for_missing_or_non_commit_detached_head() {
    for missing in [false, true] {
        let (f, stash) = stash_fixture(true);
        assert!(
            execute(f.root(), Request::ApplyStash(stash))
                .result
                .is_err()
        );
        let invalid = if missing {
            "1111111111111111111111111111111111111111".to_string()
        } else {
            f.git(&["hash-object", "-w", "file.txt"])
        };
        let head = format!("{invalid}\n");
        f.write(".git/HEAD", &head);
        let index = fs::read(f.root().join(".git/index")).unwrap();
        let contents = fs::read(f.root().join("file.txt")).unwrap();
        assert!(execute(f.root(), Request::Abort).result.is_err());
        assert_eq!(
            fs::read_to_string(f.root().join(".git/HEAD")).unwrap(),
            head
        );
        assert_eq!(fs::read(f.root().join(".git/index")).unwrap(), index);
        assert_eq!(fs::read(f.root().join("file.txt")).unwrap(), contents);
        assert!(f.root().join(".git/rustle-operations/stash-apply").exists());
    }
}

#[test]
fn stash_abort_rejects_malformed_commit_payload_at_the_checkpoint_head_id() {
    let (f, stash) = stash_fixture(true);
    let head = f.git(&["rev-parse", "HEAD"]);
    assert!(
        execute(f.root(), Request::ApplyStash(stash))
            .result
            .is_err()
    );
    let malformed = command::run(
        f.root(),
        &[
            "hash-object",
            "--literally",
            "-t",
            "commit",
            "-w",
            "--stdin",
        ],
        Some(b"malformed commit body\n"),
        Duration::from_secs(30),
    )
    .unwrap();
    assert!(malformed.status.success());
    let malformed = std::str::from_utf8(&malformed.stdout).unwrap().trim();
    let object = |id: &str| f.root().join(".git/objects").join(&id[..2]).join(&id[2..]);
    let destination = object(&head);
    // Corrupt only this disposable fixture's loose object while keeping HEAD's
    // exact ID unchanged, so an ID-only comparison cannot detect the problem.
    #[cfg(windows)]
    {
        let mut permissions = fs::metadata(&destination).unwrap().permissions();
        // Windows read-only flag only; the Unix case below sets an owner-only mode.
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(&destination, permissions).unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fs::copy(object(malformed), destination).unwrap();
    let index = fs::read(f.root().join(".git/index")).unwrap();
    let contents = fs::read(f.root().join("file.txt")).unwrap();
    let head_ref = fs::read(f.root().join(".git/HEAD")).unwrap();
    assert!(execute(f.root(), Request::Abort).result.is_err());
    assert_eq!(fs::read(f.root().join(".git/HEAD")).unwrap(), head_ref);
    assert_eq!(fs::read(f.root().join(".git/index")).unwrap(), index);
    assert_eq!(fs::read(f.root().join("file.txt")).unwrap(), contents);
    assert!(f.root().join(".git/rustle-operations/stash-apply").exists());
}

#[test]
fn stash_apply_refuses_dirty_tree_without_changing_it() {
    let (f, stash) = stash_fixture(false);
    f.write("file.txt", "local edits\n");
    assert!(
        execute(f.root(), Request::ApplyStash(stash))
            .result
            .is_err()
    );
    assert_eq!(
        fs::read(f.root().join("file.txt")).unwrap(),
        b"local edits\n"
    );
    assert!(
        !Context::new(f.root())
            .unwrap()
            .directory
            .join("stash-apply")
            .exists()
    );
}

#[test]
fn reftable_stash_drop_restores_log_order_identity_dates_and_last_entry() {
    let (f, _) = stash_fixture(false);
    f.write("file.txt", "second stash\n");
    f.git(&["stash", "push", "-m", "newer"]);
    f.git(&["refs", "migrate", "--ref-format=reftable"]);
    let original = stashes::list(f.root()).unwrap();
    let log = f.git(&[
        "reflog",
        "show",
        "--date=raw",
        "--format=%H%x00%gn%x00%ge%x00%gD%x00%gs",
        "refs/stash",
    ]);
    f.run(Request::DropStash(original[1].clone()));
    f.run(Request::MoveTo(0));
    assert_eq!(stashes::list(f.root()).unwrap(), original);
    assert_eq!(
        f.git(&[
            "reflog",
            "show",
            "--date=raw",
            "--format=%H%x00%gn%x00%ge%x00%gD%x00%gs",
            "refs/stash"
        ]),
        log
    );
    f.run(Request::MoveTo(1));
    f.run(Request::DropStash(original[0].clone()));
    assert!(stashes::list(f.root()).unwrap().is_empty());
    assert!(
        !super::super::command::run(
            f.root(),
            &["reflog", "exists", "refs/stash"],
            None,
            Duration::from_secs(30)
        )
        .unwrap()
        .status
        .success()
    );
    f.run(Request::MoveTo(1));
    assert_eq!(stashes::list(f.root()).unwrap()[0].oid, original[0].oid);
    f.run(Request::MoveTo(2));
    assert!(stashes::list(f.root()).unwrap().is_empty());
}

#[test]
fn reference_storage_migration_invalidates_old_config_checkpoints() {
    let f = Fixture::new();
    f.run(Request::Create {
        name: "topic".into(),
        target: "HEAD".into(),
    });
    f.git(&["refs", "migrate", "--ref-format=reftable"]);
    assert!(execute(f.root(), Request::MoveTo(0)).result.is_err());
    assert_eq!(f.git(&["rev-parse", "--show-ref-format"]), "reftable");
    assert!(
        !f.git(&["rev-parse", "--verify", "refs/heads/topic"])
            .is_empty()
    );
}

#[test]
fn reference_storage_migration_refuses_interrupted_pending_mutation() {
    let f = Fixture::new();
    let context = Context::new(f.root()).unwrap();
    let before = context.snapshot().unwrap();
    let journal = Journal {
        pending: Some(Pending {
            label: "Interrupted operation".into(),
            before,
        }),
        ..Journal::default()
    };
    context.save(&journal).unwrap();
    assert!(super::super::refs::migrate_reftable(f.root()).is_err());
    assert_eq!(f.git(&["rev-parse", "--show-ref-format"]), "files");
    assert!(context.load().unwrap().pending.is_some());
}

#[test]
#[ignore = "One warmup and fifteen disposable stash operation samples"]
fn benchmark_stashes() {
    use std::time::Instant;
    for sample in 0..16 {
        let (f, stash) = stash_fixture(false);
        let start = Instant::now();
        f.run(Request::ApplyStash(stash.clone()));
        let apply_ms = start.elapsed().as_secs_f64() * 1000.;
        let start = Instant::now();
        f.run(Request::MoveTo(0));
        let undo_ms = start.elapsed().as_secs_f64() * 1000.;
        let start = Instant::now();
        f.run(Request::MoveTo(1));
        let redo_ms = start.elapsed().as_secs_f64() * 1000.;
        let start = Instant::now();
        f.run(Request::DropStash(stash));
        let drop_ms = start.elapsed().as_secs_f64() * 1000.;
        f.run(Request::MoveTo(1));
        assert_eq!(stashes::list(f.root()).unwrap().len(), 1);
        let (c, stash) = stash_fixture(true);
        let start = Instant::now();
        assert!(
            execute(c.root(), Request::ApplyStash(stash))
                .result
                .is_err()
        );
        let conflict_ms = start.elapsed().as_secs_f64() * 1000.;
        let start = Instant::now();
        c.run(Request::Abort);
        let abort_ms = start.elapsed().as_secs_f64() * 1000.;
        if sample > 0 {
            println!(
                "{}",
                serde_json::json!({"sample":sample,"apply_ms":apply_ms,"undo_ms":undo_ms,"redo_ms":redo_ms,"drop_ms":drop_ms,"conflict_ms":conflict_ms,"abort_ms":abort_ms})
            );
        }
    }
}

#[test]
fn branches_undo_redo_jump_and_fork() {
    let f = Fixture::new();
    f.run(Request::Create {
        name: "topic".into(),
        target: "HEAD".into(),
    });
    f.run(Request::Rename {
        old: "topic".into(),
        new: "renamed".into(),
    });
    let view = f.run(Request::Delete {
        name: "renamed".into(),
        force: false,
    });
    assert_eq!(view.cursor, 3);
    f.run(Request::MoveTo(1));
    assert!(f.git(&["branch", "--list", "topic"]).contains("topic"));
    f.run(Request::MoveTo(3));
    assert!(f.git(&["branch", "--list", "renamed"]).is_empty());
    f.run(Request::MoveTo(0));
    let view = f.run(Request::Create {
        name: "new-history".into(),
        target: "HEAD".into(),
    });
    assert_eq!(view.labels.len(), 1);
}

#[test]
fn lightweight_tag_uses_frozen_commit_without_identity_and_preserves_dirty_state() {
    let f = Fixture::new();
    let target = f.git(&["rev-parse", "HEAD"]);
    f.write("second.txt", "second\n");
    f.commit("advance HEAD after choosing target");
    f.git(&["config", "user.name", ""]);
    f.git(&["config", "user.email", ""]);
    f.git(&["config", "user.useConfigOnly", "true"]);
    f.write("file.txt", "staged\n");
    f.git(&["add", "file.txt"]);
    f.write("file.txt", "unstaged\n");
    f.write("untracked.txt", "keep me\n");
    let context = Context::new(f.root()).unwrap();
    let before = context.snapshot().unwrap();
    let created = f.run(Request::CreateTag {
        name: "release/v1".into(),
        target: target.clone(),
        message: None,
    });
    assert_eq!(created.labels, vec!["Create tag release/v1"]);
    assert_eq!(f.git(&["rev-parse", "refs/tags/release/v1"]), target);
    assert_eq!(f.git(&["cat-file", "-t", "refs/tags/release/v1"]), "commit");
    let after = context.snapshot().unwrap();
    assert_eq!(after.head, before.head);
    assert_eq!(after.symbolic, before.symbolic);
    assert_eq!(after.index, before.index);
    assert_eq!(after.tree, before.tree);
    assert_eq!(after.config, before.config);
    f.run(Request::MoveTo(0));
    assert!(context.snapshot().unwrap().equivalent(&before));
    f.run(Request::MoveTo(1));
    assert!(context.snapshot().unwrap().equivalent(&after));
    assert_eq!(
        fs::read_to_string(f.root().join("untracked.txt")).unwrap(),
        "keep me\n"
    );
}

#[test]
fn annotated_tag_is_unsigned_and_redo_survives_pruning_after_undo() {
    let f = Fixture::new();
    let target = f.git(&["rev-parse", "HEAD"]);
    f.git(&["config", "tag.gpgSign", "true"]);
    f.git(&["config", "gpg.program", "nonexistent-rustle-test-signer"]);
    f.run(Request::CreateTag {
        name: "v2".into(),
        target: target.clone(),
        message: Some("Release notes\n\nMore details".into()),
    });
    let object = f.git(&["rev-parse", "refs/tags/v2"]);
    assert_ne!(object, target);
    assert_eq!(f.git(&["cat-file", "-t", &object]), "tag");
    assert_eq!(f.git(&["rev-parse", "refs/tags/v2^{commit}"]), target);
    let text = f.git(&["cat-file", "-p", &object]);
    assert!(text.contains("tagger Rustle Test <test@example.invalid>"));
    assert!(text.ends_with("Release notes\n\nMore details"));
    assert!(!text.contains("BEGIN PGP SIGNATURE"));
    f.run(Request::MoveTo(0));
    assert!(output(f.root(), &["show-ref", "--verify", "refs/tags/v2"]).is_err());
    f.git(&["reflog", "expire", "--expire=now", "--all"]);
    f.git(&["prune", "--expire", "now"]);
    assert_eq!(f.git(&["cat-file", "-t", &object]), "tag");
    f.run(Request::MoveTo(1));
    assert_eq!(f.git(&["rev-parse", "refs/tags/v2"]), object);
    assert_eq!(f.git(&["rev-parse", "HEAD"]), target);
}

#[test]
fn tags_refuse_duplicates_invalid_names_and_unavailable_commits() {
    let f = Fixture::new();
    let target = f.git(&["rev-parse", "HEAD"]);
    f.run(Request::CreateTag {
        name: "v1".into(),
        target: target.clone(),
        message: None,
    });
    let before = Context::new(f.root()).unwrap().snapshot().unwrap();
    for name in [
        "v1",
        "",
        "-bad",
        "bad..name",
        "bad name",
        "bad.lock",
        "HEAD",
        "bad\nname",
    ] {
        let failed = execute(
            f.root(),
            Request::CreateTag {
                name: name.into(),
                target: target.clone(),
                message: Some("Annotation".into()),
            },
        );
        assert!(failed.result.is_err(), "accepted {name:?}");
        assert_eq!(failed.view.labels, vec!["Create tag v1"]);
        assert_eq!(failed.view.cursor, 1);
        assert!(
            Context::new(f.root())
                .unwrap()
                .snapshot()
                .unwrap()
                .equivalent(&before)
        );
    }
    for target in [
        "f".repeat(40),
        "HEAD".into(),
        f.git(&["rev-parse", "HEAD:file.txt"]),
    ] {
        assert!(
            execute(
                f.root(),
                Request::CreateTag {
                    name: "unavailable".into(),
                    target,
                    message: None,
                }
            )
            .result
            .is_err()
        );
    }
    for message in [String::new(), "\0invalid".into(), "a".repeat(65536)] {
        assert!(
            execute(
                f.root(),
                Request::CreateTag {
                    name: "invalid-message".into(),
                    target: target.clone(),
                    message: Some(message),
                }
            )
            .result
            .is_err()
        );
    }
    assert!(
        Context::new(f.root())
            .unwrap()
            .snapshot()
            .unwrap()
            .equivalent(&before)
    );
    assert_eq!(f.git(&["tag", "--list"]), "v1");
}

#[test]
#[ignore = "one warm-up and 15 disposable tag creation/undo/redo samples per tag kind"]
fn benchmark_tags() {
    use std::time::Instant;
    for annotated in [false, true] {
        for sample in 0..16 {
            let f = Fixture::new();
            let target = f.git(&["rev-parse", "HEAD"]);
            f.write("file.txt", "second\n");
            f.commit("HEAD advanced");
            f.write("file.txt", "staged\n");
            f.git(&["add", "file.txt"]);
            f.write("file.txt", "unstaged\n");
            f.write("untracked.txt", "retained\n");
            let context = Context::new(f.root()).unwrap();
            let before = context.snapshot().unwrap();
            let start = Instant::now();
            f.run(Request::CreateTag {
                name: "bench-tag".into(),
                target: target.clone(),
                message: annotated.then(|| "Benchmark annotation".into()),
            });
            let create_ms = start.elapsed().as_secs_f64() * 1000.;
            let after = context.snapshot().unwrap();
            assert_eq!(after.head, before.head);
            assert_eq!(after.index, before.index);
            assert_eq!(after.tree, before.tree);
            assert_eq!(
                f.git(&["rev-parse", "refs/tags/bench-tag^{commit}"]),
                target
            );
            let start = Instant::now();
            f.run(Request::MoveTo(0));
            let undo_ms = start.elapsed().as_secs_f64() * 1000.;
            assert!(context.snapshot().unwrap().equivalent(&before));
            let start = Instant::now();
            f.run(Request::MoveTo(1));
            let redo_ms = start.elapsed().as_secs_f64() * 1000.;
            assert!(context.snapshot().unwrap().equivalent(&after));
            if sample > 0 {
                println!(
                    "{}",
                    serde_json::json!({"sample":sample,"annotated":annotated,
                    "create_ms":create_ms,"undo_ms":undo_ms,"redo_ms":redo_ms})
                );
            }
        }
    }
}

#[test]
fn clear_history_preserves_dirty_state_removes_both_tails_and_persists() {
    let f = Fixture::new();
    f.run(Request::Create {
        name: "topic".into(),
        target: "HEAD".into(),
    });
    f.run(Request::Rename {
        old: "topic".into(),
        new: "renamed".into(),
    });
    f.run(Request::MoveTo(1));
    f.write("file.txt", "staged\n");
    f.git(&["add", "file.txt"]);
    f.write("file.txt", "unstaged\n");
    f.write("untracked.txt", "keep me\n");
    let context = Context::new(f.root()).unwrap();
    let before = context.snapshot().unwrap();
    let pins = f.git(&[
        "for-each-ref",
        "--format=%(refname) %(objectname)",
        "refs/rustle/checkpoints/",
    ]);
    let outcome = execute(f.root(), Request::ClearHistory);
    assert!(outcome.result.is_ok());
    assert!(!outcome.refresh_repository);
    assert!(!Request::ClearHistory.is_read_only());
    assert!(!Request::ClearHistory.changes_repository());
    assert!(outcome.view.labels.is_empty());
    assert_eq!(outcome.view.cursor, 0);
    assert!(before.equivalent(&context.snapshot().unwrap()));
    assert_eq!(
        pins,
        f.git(&[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/rustle/checkpoints/"
        ])
    );
    assert!(f.run(Request::Inspect).labels.is_empty());
    assert!(execute(f.root(), Request::MoveTo(1)).result.is_err());
    f.run(Request::ClearHistory);
    let view = f.run(Request::Create {
        name: "new-start".into(),
        target: "HEAD".into(),
    });
    assert_eq!(view.labels.len(), 1);
    f.run(Request::MoveTo(0));
    assert!(before.equivalent(&context.snapshot().unwrap()));
}

#[test]
fn clear_history_refuses_active_and_pending_recovery() {
    let f = Fixture::new();
    f.conflict();
    execute(f.root(), Request::Merge("topic".into()));
    let context = Context::new(f.root()).unwrap();
    let journal_path = context.directory.join("journal.json");
    let before = fs::read(&journal_path).unwrap();
    assert!(execute(f.root(), Request::ClearHistory).result.is_err());
    assert_eq!(before, fs::read(&journal_path).unwrap());
    f.run(Request::Abort);
    let mut journal = context.load().unwrap();
    journal.pending = Some(Pending {
        label: "Interrupted operation".into(),
        before: context.snapshot().unwrap(),
    });
    context.save(&journal).unwrap();
    let before = fs::read(&journal_path).unwrap();
    assert!(execute(f.root(), Request::ClearHistory).result.is_err());
    assert_eq!(before, fs::read(&journal_path).unwrap());
}

#[test]
fn clear_history_is_scoped_to_its_worktree_journal() {
    let f = Fixture::new();
    let linked_dir = tempfile::tempdir().unwrap();
    let linked = linked_dir.path().join("linked");
    f.git(&["worktree", "add", "-b", "linked", linked.to_str().unwrap()]);
    f.run(Request::Create {
        name: "topic".into(),
        target: "HEAD".into(),
    });
    let primary = Context::new(f.root()).unwrap();
    let secondary = Context::new(&linked).unwrap();
    secondary.save(&primary.load().unwrap()).unwrap();
    let original = fs::read(primary.directory.join("journal.json")).unwrap();
    let refs = f.git(&["show-ref"]);
    let out = execute(&linked, Request::ClearHistory);
    assert!(out.result.is_ok());
    assert!(secondary.load().unwrap().entries.is_empty());
    assert_eq!(
        original,
        fs::read(primary.directory.join("journal.json")).unwrap()
    );
    assert_eq!(refs, f.git(&["show-ref"]));
}

#[test]
#[ignore = "creates a new disposable 100-step undo benchmark fixture"]
fn prepare_undo_fixture() {
    let root = PathBuf::from(
        std::env::var_os("GIT_RUSTLE_UNDO_FIXTURE").expect("GIT_RUSTLE_UNDO_FIXTURE"),
    );
    assert!(!root.exists(), "Use a new fixture directory");
    fs::create_dir_all(&root).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "Undo Benchmark"],
        vec!["config", "user.email", "bench@example.invalid"],
        vec!["config", "commit.gpgSign", "false"],
        vec!["config", "core.autocrlf", "false"],
        vec!["config", "core.hooksPath", "disabled-hooks"],
    ] {
        output(&root, &args).unwrap();
    }
    fs::write(root.join("file.txt"), "base\n").unwrap();
    output(&root, &["add", "."]).unwrap();
    output(&root, &["commit", "-m", "Initial commit"]).unwrap();
    for i in 0..100 {
        let request = if i == 0 {
            Request::Create {
                name: "topic-0".into(),
                target: "HEAD".into(),
            }
        } else {
            Request::Rename {
                old: format!("topic-{}", i - 1),
                new: format!("topic-{i}"),
            }
        };
        assert!(execute(&root, request).result.is_ok());
    }
    let out = execute(&root, Request::MoveTo(80));
    assert!(out.result.is_ok());
    assert_eq!(out.view.labels.len(), 100);
    assert_eq!(out.view.cursor, 80);
    fs::write(
        root.join(".git/rustle-operations/benchmark-journal.json"),
        fs::read(root.join(".git/rustle-operations/journal.json")).unwrap(),
    )
    .unwrap();
    fs::write(root.join(".git/rustle-undo-benchmark"), "undo fixture 1\n").unwrap();
}

#[test]
fn merge_conflict_continue_undo_and_redo() {
    let f = Fixture::new();
    f.conflict();
    let before = f.git(&["rev-parse", "HEAD"]);
    let out = execute(f.root(), Request::Merge("topic".into()));
    assert!(out.result.is_err());
    assert_eq!(out.view.conflict.unwrap().files, ["file.txt"]);
    assert!(execute(f.root(), Request::Continue).result.is_err());
    assert!(
        execute(
            f.root(),
            Request::Resolve {
                path: "file.txt".into(),
                choice: Resolution::Mark
            }
        )
        .result
        .is_err()
    );
    f.run(Request::Resolve {
        path: "file.txt".into(),
        choice: Resolution::Theirs,
    });
    let view = f.run(Request::Continue);
    assert_eq!(view.cursor, 1);
    let after = f.git(&["rev-parse", "HEAD"]);
    f.run(Request::MoveTo(0));
    assert_eq!(f.git(&["rev-parse", "HEAD"]), before);
    assert_eq!(
        fs::read_to_string(f.root().join("file.txt")).unwrap(),
        "main\n"
    );
    f.run(Request::MoveTo(1));
    assert_eq!(f.git(&["rev-parse", "HEAD"]), after);
}

#[test]
fn rebase_conflict_abort_and_external_edit_invalidation() {
    let f = Fixture::new();
    f.conflict();
    let before = f.git(&["rev-parse", "HEAD"]);
    let out = execute(f.root(), Request::Rebase("topic".into()));
    assert!(out.view.conflict.is_some());
    f.run(Request::Abort);
    assert_eq!(f.git(&["rev-parse", "HEAD"]), before);
    f.run(Request::Create {
        name: "new".into(),
        target: "HEAD".into(),
    });
    f.write("file.txt", "external work\n");
    let out = execute(f.root(), Request::MoveTo(0));
    assert!(out.result.is_err());
    assert!(out.view.labels.is_empty());
    assert_eq!(
        fs::read_to_string(f.root().join("file.txt")).unwrap(),
        "external work\n"
    );
}

#[test]
fn checkpoint_preserves_dirty_index_and_untracked_files() {
    let f = Fixture::new();
    f.write("file.txt", "staged\n");
    f.git(&["add", "file.txt"]);
    f.write("file.txt", "unstaged\n");
    f.write("new.txt", "untracked\n");
    let before = f.git(&["status", "--porcelain=v1"]);
    f.run(Request::Create {
        name: "topic".into(),
        target: "HEAD".into(),
    });
    f.run(Request::MoveTo(0));
    assert_eq!(f.git(&["status", "--porcelain=v1"]), before);
    assert_eq!(f.git(&["show", ":file.txt"]), "staged");
    assert_eq!(
        fs::read_to_string(f.root().join("file.txt")).unwrap(),
        "unstaged\n"
    );
    assert_eq!(
        fs::read_to_string(f.root().join("new.txt")).unwrap(),
        "untracked\n"
    );
}

#[test]
fn unsafe_names_and_arbitrary_rebase_commands_are_rejected() {
    let f = Fixture::new();
    for name in ["--force", "HEAD", "bad\nname", "x:y"] {
        assert!(
            execute(
                f.root(),
                Request::Create {
                    name: name.into(),
                    target: "HEAD".into()
                }
            )
            .result
            .is_err()
        );
    }
    f.write("two", "two");
    f.commit("two");
    let plan = plan(f.root(), "HEAD~1").unwrap();
    assert!(
        execute(
            f.root(),
            Request::Interactive {
                plan: plan.clone(),
                text: format!("exec {} whoami", plan.commits[0])
            }
        )
        .result
        .is_err()
    );
    assert!(
        execute(
            f.root(),
            Request::Interactive {
                plan: plan.clone(),
                text: format!("fixup {} message", plan.commits[0])
            }
        )
        .result
        .is_err()
    );
}

#[test]
fn commit_undo_redo_restores_staging_and_preserves_unselected_work() {
    let f = Fixture::new();
    f.write("file.txt", "selected\n");
    f.write("unselected.txt", "untouched\n");
    f.git(&["add", "unselected.txt"]);
    f.write("untracked.txt", "untracked\n");
    let repo = super::super::open(f.root()).unwrap();
    let snapshot = super::super::worktree::snapshot(&repo).unwrap();
    let status = f.git(&["status", "--porcelain=v1"]);
    let head = f.git(&["rev-parse", "HEAD"]);
    f.run(Request::Commit(crate::pending::Request {
        head: snapshot.head,
        branch: snapshot.branch,
        new_branch: String::new(),
        description: "selected commit".into(),
        files: snapshot
            .files
            .into_iter()
            .filter(|f| f.path == "file.txt")
            .collect(),
    }));
    let committed = f.git(&["rev-parse", "HEAD"]);
    assert_ne!(head, committed);
    f.run(Request::MoveTo(0));
    assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(f.git(&["status", "--porcelain=v1"]), status);
    f.run(Request::MoveTo(1));
    assert_eq!(f.git(&["rev-parse", "HEAD"]), committed);
    assert_eq!(
        fs::read_to_string(f.root().join("untracked.txt")).unwrap(),
        "untracked\n"
    );
}

#[test]
fn linked_worktree_branch_is_protected_and_checkpoints_are_hidden() {
    let f = Fixture::new();
    f.run(Request::Create {
        name: "topic".into(),
        target: "HEAD".into(),
    });
    let linked = f.root().join("linked");
    f.git(&["worktree", "add", linked.to_str().unwrap(), "topic"]);
    // Place linked checkout outside the tracked worktree before checking history.
    f.write(".git/info/exclude", "linked/\n");
    assert!(execute(f.root(), Request::MoveTo(0)).result.is_err());
    assert!(f.git(&["branch", "--list", "topic"]).contains("topic"));
    let refs = super::super::snapshot_refs(&super::super::open(f.root()).unwrap()).unwrap();
    assert!(
        !refs
            .refs
            .iter()
            .any(|r| r.name.contains("rustle/checkpoints"))
    );
}

#[test]
fn interactive_reorder_reword_fixup_drop_and_restore() {
    let f = Fixture::new();
    let base = f.git(&["rev-parse", "HEAD"]);
    for name in ["one", "two", "three", "four"] {
        f.write(name, name);
        f.commit(name);
    }
    let old = f.git(&["rev-parse", "HEAD"]);
    let p = plan(f.root(), &base).unwrap();
    let text = format!(
        "reword {} new two\npick {} one\nfixup {} three\ndrop {} four\n",
        p.commits[1], p.commits[0], p.commits[2], p.commits[3]
    );
    f.run(Request::Interactive { plan: p, text });
    assert_eq!(
        f.git(&["log", "--format=%s", "--reverse", &format!("{base}..HEAD")]),
        "new two\none"
    );
    assert!(!f.root().join("four").exists());
    let new = f.git(&["rev-parse", "HEAD"]);
    f.run(Request::MoveTo(0));
    assert_eq!(f.git(&["rev-parse", "HEAD"]), old);
    f.run(Request::MoveTo(1));
    assert_eq!(f.git(&["rev-parse", "HEAD"]), new);
}

#[test]
fn pull_fetches_only_upstream_and_push_seals_history() {
    let source = Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    let clone = dir.path().join("clone");
    output(
        dir.path(),
        &[
            "clone",
            source.root().to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    )
    .unwrap();
    output(&clone, &["config", "user.name", "Rustle Test"]).unwrap();
    output(&clone, &["config", "user.email", "test@example.invalid"]).unwrap();
    source.git(&["config", "receive.denyCurrentBranch", "ignore"]);
    source.write("new", "new");
    source.commit("new upstream");
    source.git(&["branch", "unrelated"]);
    source.git(&["tag", "unrelated-tag"]);
    let result = execute(&clone, Request::Pull);
    assert!(result.result.is_ok(), "{:?}", result.result);
    assert_eq!(
        output(&clone, &["rev-parse", "HEAD"]).unwrap(),
        source.git(&["rev-parse", "HEAD"])
    );
    assert!(
        output(&clone, &["branch", "-r", "--list", "origin/unrelated"])
            .unwrap()
            .is_empty()
    );
    assert!(
        output(&clone, &["tag", "--list", "unrelated-tag"])
            .unwrap()
            .is_empty()
    );
    let result = execute(&clone, Request::PushReviewed(preview_push(&clone).unwrap()));
    assert!(result.result.is_ok(), "{:?}", result.result);
    assert_eq!(
        result.result.as_ref().unwrap(),
        "Pushed main to origin/main."
    );
    assert!(result.view.labels.is_empty());
    assert!(result.view.boundary.contains("Push"));
}

#[test]
fn force_push_rejects_stale_remote_tip() {
    let source = Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    let clone = dir.path().join("clone");
    output(
        dir.path(),
        &[
            "clone",
            source.root().to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    )
    .unwrap();
    source.git(&["config", "receive.denyCurrentBranch", "ignore"]);
    source.write("new", "remote advanced");
    source.commit("remote advanced");
    let tip = source.git(&["rev-parse", "HEAD"]);
    let result = execute(&clone, Request::PushReviewed(preview_push(&clone).unwrap()));
    assert!(result.result.is_err());
    assert_eq!(source.git(&["rev-parse", "HEAD"]), tip);
}

#[test]
fn invalid_push_does_not_discard_undo_history() {
    let f = Fixture::new();
    f.run(Request::Create {
        name: "topic".into(),
        target: "HEAD".into(),
    });
    let result = execute(f.root(), Request::PreviewPush);
    assert!(result.result.is_err());
    assert_eq!(result.view.cursor, 1);
    f.run(Request::MoveTo(0));
}

#[test]
fn resolution_editor_rejects_stale_content_and_preserves_manual_result() {
    let f = Fixture::new();
    f.conflict();
    execute(f.root(), Request::Merge("topic".into()));
    let read = f
        .run(Request::ReadConflict("file.txt".into()))
        .preview
        .unwrap();
    assert_eq!(read.ours, "main\n");
    assert_eq!(read.theirs, "topic\n");
    f.write("file.txt", "external edit\n");
    assert!(
        execute(
            f.root(),
            Request::SaveConflict {
                path: "file.txt".into(),
                identity: read.identity,
                original: read.original.unwrap(),
                edited: "overwrite".into()
            }
        )
        .result
        .is_err()
    );
    let read = f
        .run(Request::ReadConflict("file.txt".into()))
        .preview
        .unwrap();
    f.run(Request::SaveConflict {
        path: "file.txt".into(),
        identity: read.identity,
        original: read.original.unwrap(),
        edited: "combined\n".into(),
    });
    f.run(Request::Continue);
    assert_eq!(f.git(&["show", "HEAD:file.txt"]), "combined");
}

#[test]
fn conflict_merge_metadata_and_automatic_file_refresh_do_not_stage_or_write_the_journal() {
    let f = Fixture::new();
    f.conflict();
    let main = f.git(&["rev-parse", "main"]);
    let topic = f.git(&["rev-parse", "topic"]);
    let view = execute(f.root(), Request::Merge("topic".into())).view;
    let conflict = view.conflict.unwrap();
    assert_eq!(conflict.local[0].id.to_string(), main);
    assert_eq!(conflict.incoming[0].id.to_string(), topic);
    assert_eq!(conflict.local[0].subject, "main");
    assert_eq!(conflict.incoming[0].author_name, "Rustle Test");
    assert!(!conflict.incoming[0].parents.is_empty());
    assert!(conflict.metadata_error.is_none());
    assert!(conflict.states[0].exists && conflict.states[0].unmerged);
    let journal_path = f.root().join(".git/rustle-operations/journal.json");
    let journal = fs::read(&journal_path).unwrap();
    let index = fs::read(f.root().join(".git/index")).unwrap();
    assert!(matches!(
        conflicts::refresh(f.root(), &conflict).unwrap(),
        conflicts::Refresh::Unchanged
    ));
    f.write("file.txt", "manually resolved without staging\n");
    let conflicts::Refresh::Changed(Some(changed)) =
        conflicts::refresh(f.root(), &conflict).unwrap()
    else {
        panic!("file edit must refresh")
    };
    assert_eq!(changed.identity, conflict.identity);
    assert_eq!(changed.files, ["file.txt"]);
    assert!(changed.states[0].edited && changed.states[0].unmerged);
    assert_eq!(fs::read(&journal_path).unwrap(), journal);
    assert_eq!(fs::read(f.root().join(".git/index")).unwrap(), index);
    f.git(&["add", "file.txt"]);
    let conflicts::Refresh::Changed(Some(staged)) = conflicts::refresh(f.root(), &changed).unwrap()
    else {
        panic!("index edit must refresh")
    };
    assert!(staged.files.is_empty());
    assert_eq!(staged.states.len(), 1);
    assert!(!staged.states[0].unmerged);
    assert!(matches!(
        conflicts::refresh(f.root(), &staged).unwrap(),
        conflicts::Refresh::Unchanged
    ));
    f.git(&["merge", "--abort"]);
    assert!(matches!(
        conflicts::refresh(f.root(), &staged).unwrap(),
        conflicts::Refresh::Changed(None)
    ));
}

#[test]
fn conflict_rebase_and_stash_summaries_describe_actual_index_sides() {
    let f = Fixture::new();
    f.conflict();
    let local = f.git(&["rev-parse", "main"]);
    let incoming = f.git(&["rev-parse", "topic"]);
    let conflict = execute(f.root(), Request::Rebase("topic".into()))
        .view
        .conflict
        .unwrap();
    assert_eq!(conflict.kind, "Rebase");
    assert_eq!(conflict.local[0].id.to_string(), local);
    assert_eq!(conflict.incoming[0].id.to_string(), incoming);
    assert!(conflict.local_label.contains("replayed"));
    assert!(conflict.incoming_label.contains("base"));
    let preview = conflicts::read_preview(f.root(), "file.txt", &conflict.identity).unwrap();
    assert_eq!(preview.ours, "topic\n");
    assert_eq!(preview.theirs, "main\n");
    assert_eq!(preview.identity, conflict.identity);
    f.run(Request::Abort);
    let (f, stash) = stash_fixture(true);
    let head = f.git(&["rev-parse", "HEAD"]);
    let conflict = execute(f.root(), Request::ApplyStash(stash.clone()))
        .view
        .conflict
        .unwrap();
    assert_eq!(conflict.kind, "Stash apply");
    assert_eq!(conflict.incoming[0].id.to_string(), stash.oid);
    assert_eq!(conflict.local[0].id.to_string(), head);
    assert!(conflict.incoming_label.contains("Stashed"));
    let preview = conflicts::read_preview(f.root(), "file.txt", &conflict.identity).unwrap();
    assert_eq!(preview.ours, "new committed contents\n");
    assert_eq!(preview.theirs, "stashed contents\n");
}

#[test]
fn conflict_operation_identity_rejects_stale_editor_even_when_file_bytes_match() {
    let f = Fixture::new();
    f.conflict();
    execute(f.root(), Request::Merge("topic".into()));
    let preview = f
        .run(Request::ReadConflict("file.txt".into()))
        .preview
        .unwrap();
    let original = preview.original.unwrap();
    // Preserve the displayed bytes while substituting a different operation's
    // incoming commit; a text-only stale guard would accept the old editor.
    let old = f.git(&["rev-parse", "topic~1"]);
    f.write(".git/MERGE_HEAD", &format!("{old}\n"));
    let error = execute(
        f.root(),
        Request::SaveConflict {
            path: "file.txt".into(),
            identity: preview.identity.clone(),
            original: original.clone(),
            edited: "wrong context\n".into(),
        },
    )
    .result
    .unwrap_err();
    assert!(error.contains("operation changed"), "{error}");
    assert_eq!(
        fs::read_to_string(f.root().join("file.txt")).unwrap(),
        original
    );
    assert!(!f.git(&["ls-files", "-u"]).is_empty());
    assert!(conflicts::read_preview(f.root(), "file.txt", &preview.identity).is_err());
}

#[test]
fn conflict_binary_and_deletion_states_remain_safe_whole_side_resolutions() {
    let f = Fixture::new();
    f.git(&["switch", "-c", "topic"]);
    fs::write(f.root().join("file.txt"), b"topic\0bytes").unwrap();
    f.commit("binary topic");
    f.git(&["switch", "main"]);
    fs::write(f.root().join("file.txt"), b"local\0bytes").unwrap();
    f.commit("binary local");
    let conflict = execute(f.root(), Request::Merge("topic".into()))
        .view
        .conflict
        .unwrap();
    assert!(conflict.states[0].exists && conflict.states[0].unmerged);
    let preview = conflicts::read_preview(f.root(), "file.txt", &conflict.identity).unwrap();
    assert!(preview.original.is_none());
    assert!(preview.ours.contains("Binary"));
    f.run(Request::Resolve {
        path: "file.txt".into(),
        choice: Resolution::Theirs,
    });
    assert_eq!(
        fs::read(f.root().join("file.txt")).unwrap(),
        b"topic\0bytes"
    );
    let current = f.run(Request::Inspect).conflict.unwrap();
    assert!(current.files.is_empty());
    assert!(!current.states[0].unmerged);
}

#[test]
fn large_conflict_worktree_file_stays_unmerged_and_supports_bounded_selected_preview() {
    let f = Fixture::new();
    f.conflict();
    assert!(
        !command::run(
            f.root(),
            &["merge", "--no-edit", "topic"],
            None,
            Duration::from_secs(30)
        )
        .unwrap()
        .status
        .success()
    );
    let large = vec![b'x'; 3 * 1024 * 1024];
    fs::write(f.root().join("file.txt"), &large).unwrap();
    let context = Context::new(f.root()).unwrap();
    let conflict = conflicts::read(&context, None).unwrap().unwrap();
    assert_eq!(conflict.files, ["file.txt"]);
    assert!(conflict.states[0].exists && conflict.states[0].unmerged);
    let preview = conflicts::read_preview(f.root(), "file.txt", &conflict.identity).unwrap();
    assert!(
        preview.original.is_none(),
        "large working text must not enter the editor"
    );
    assert_eq!(preview.ours, "main\n");
    assert_eq!(preview.theirs, "topic\n");
    assert!(
        execute(
            f.root(),
            Request::SaveConflict {
                path: "file.txt".into(),
                identity: conflict.identity,
                original: String::new(),
                edited: "accidental overwrite\n".into(),
            }
        )
        .result
        .is_err()
    );
    assert_eq!(fs::read(f.root().join("file.txt")).unwrap(), large);
    assert!(!f.git(&["ls-files", "-u"]).is_empty());
    f.run(Request::Resolve {
        path: "file.txt".into(),
        choice: Resolution::Theirs,
    });
    assert_eq!(fs::read(f.root().join("file.txt")).unwrap(), b"topic\n");
    assert!(f.git(&["ls-files", "-u"]).is_empty());
}

#[test]
fn conflict_cherry_pick_and_revert_summaries_identify_the_requested_change() {
    for revert in [false, true] {
        let f = Fixture::new();
        f.conflict();
        let target = if revert {
            let id = f.git(&["rev-parse", "HEAD"]);
            f.write("file.txt", "later local contents\n");
            f.commit("later local");
            id
        } else {
            f.git(&["rev-parse", "topic"])
        };
        let verb = if revert { "revert" } else { "cherry-pick" };
        assert!(
            !command::run(
                f.root(),
                &[verb, "--no-edit", &target],
                None,
                Duration::from_secs(30)
            )
            .unwrap()
            .status
            .success()
        );
        let conflict = f.run(Request::Inspect).conflict.unwrap();
        assert_eq!(conflict.kind, if revert { "Revert" } else { "Cherry-pick" });
        assert_eq!(conflict.incoming[0].id.to_string(), target);
        assert_eq!(
            conflict.local[0].id.to_string(),
            f.git(&["rev-parse", "HEAD"])
        );
        assert!(
            conflict
                .incoming_label
                .contains(if revert { "inverse" } else { "cherry-picked" })
        );
        let preview = conflicts::read_preview(f.root(), "file.txt", &conflict.identity).unwrap();
        assert_eq!(preview.theirs, if revert { "base\n" } else { "topic\n" });
    }
}

#[test]
fn conflict_commit_ranges_are_capped_and_report_truncation() {
    let f = Fixture::new();
    let base = f.git(&["rev-parse", "HEAD"]);
    let mut input = String::new();
    // Deliberately older than the base commit: plain rev-list date pruning can
    // otherwise misclassify that shared ancestor as a local-only commit.
    for n in 1..=105 {
        let subject = format!("incoming {n:03}\n");
        input += &format!(
            "commit refs/heads/topic\nmark :{n}\nauthor Rustle Test <test@example.invalid> 1700000000 +0000\ncommitter Rustle Test <test@example.invalid> 1700000000 +0000\ndata {}\n{subject}from {}\n\n",
            subject.len(),
            if n == 1 {
                base.clone()
            } else {
                format!(":{}", n - 1)
            }
        );
    }
    let imported = command::run(
        f.root(),
        &["fast-import", "--quiet"],
        Some(input.as_bytes()),
        Duration::from_secs(30),
    )
    .unwrap();
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    // A stopped merge can have staged resolutions and therefore no unmerged
    // index entries; the operation marker still supplies its bounded ranges.
    let topic = f.git(&["rev-parse", "topic"]);
    f.write(".git/MERGE_HEAD", &format!("{topic}\n"));
    let conflict = f.run(Request::Inspect).conflict.unwrap();
    assert!(
        conflict.metadata_error.is_none(),
        "{:?}",
        conflict.metadata_error
    );
    assert_eq!(conflict.incoming.len(), 100);
    assert!(conflict.commits_truncated);
    assert!(conflict.local.is_empty());
    assert_eq!(conflict.incoming[0].subject, "incoming 105");
}

#[test]
fn frozen_conflict_actions_reject_replaced_operation_without_touching_repository_or_journal() {
    let f = Fixture::new();
    f.conflict();
    let conflict = execute(f.root(), Request::Merge("topic".into()))
        .view
        .conflict
        .unwrap();
    let base = f.git(&["rev-parse", "topic~1"]);
    f.write(".git/MERGE_HEAD", &format!("{base}\n"));
    let journal_path = f.root().join(".git/rustle-operations/journal.json");
    let journal = fs::read(&journal_path).unwrap();
    let journal_mtime = fs::metadata(&journal_path).unwrap().modified().unwrap();
    let index = fs::read(f.root().join(".git/index")).unwrap();
    let bytes = fs::read(f.root().join("file.txt")).unwrap();
    let head = f.git(&["rev-parse", "HEAD"]);
    for request in [
        Request::Resolve {
            path: "file.txt".into(),
            choice: Resolution::Ours,
        },
        Request::Resolve {
            path: "file.txt".into(),
            choice: Resolution::Theirs,
        },
        Request::Resolve {
            path: "file.txt".into(),
            choice: Resolution::Mark,
        },
        Request::Abort,
        Request::Continue,
        Request::Skip,
    ] {
        let outcome = execute(
            f.root(),
            Request::OnConflict {
                identity: conflict.identity.clone(),
                request: Box::new(request),
            },
        );
        assert!(
            outcome
                .view
                .conflict
                .as_ref()
                .is_some_and(|current| current.identity != conflict.identity)
        );
        let error = outcome.result.unwrap_err();
        assert!(error.contains("operation changed"), "{error}");
        assert_eq!(fs::read(f.root().join(".git/index")).unwrap(), index);
        assert_eq!(fs::read(f.root().join("file.txt")).unwrap(), bytes);
        assert_eq!(fs::read(&journal_path).unwrap(), journal);
        assert_eq!(
            fs::metadata(&journal_path).unwrap().modified().unwrap(),
            journal_mtime
        );
        assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
    }
}

#[test]
fn frozen_conflict_identity_changes_when_same_merge_pair_is_aborted_and_restarted() {
    let f = Fixture::new();
    f.conflict();
    let old = execute(f.root(), Request::Merge("topic".into()))
        .view
        .conflict
        .unwrap();
    f.git(&["merge", "--abort"]);
    assert!(
        !command::run(
            f.root(),
            &["merge", "--no-edit", "topic"],
            None,
            Duration::from_secs(30)
        )
        .unwrap()
        .status
        .success()
    );
    let context = Context::new(f.root()).unwrap();
    let current = conflicts::read(&context, None).unwrap().unwrap();
    assert_ne!(old.identity, current.identity);
    let error = execute(
        f.root(),
        Request::OnConflict {
            identity: old.identity,
            request: Box::new(Request::Abort),
        },
    )
    .result
    .unwrap_err();
    assert!(error.contains("operation changed"), "{error}");
    assert!(f.root().join(".git/MERGE_HEAD").exists());
    // Supported actions still dispatch when their reviewed context matches.
    f.run(Request::OnConflict {
        identity: current.identity,
        request: Box::new(Request::Resolve {
            path: "file.txt".into(),
            choice: Resolution::Theirs,
        }),
    });
    assert!(f.git(&["ls-files", "-u"]).is_empty());
}

#[test]
#[ignore = "explicit conflict metadata and automatic refresh worker measurement"]
fn benchmark_conflict_refresh() {
    use std::time::Instant;
    for sample in -1..15 {
        let f = Fixture::new();
        f.conflict();
        // Produce the stopped Git operation without pre-populating metadata.
        assert!(
            !command::run(
                f.root(),
                &["merge", "--no-edit", "topic"],
                None,
                Duration::from_secs(30)
            )
            .unwrap()
            .status
            .success()
        );
        let context = Context::new(f.root()).unwrap();
        let start = Instant::now();
        let conflict = conflicts::read(&context, None).unwrap().unwrap();
        let metadata_ms = start.elapsed().as_secs_f64() * 1000.;
        assert!(conflict.metadata_error.is_none());
        assert_eq!(conflict.local.len(), 1);
        assert_eq!(conflict.incoming.len(), 1);
        let start = Instant::now();
        for _ in 0..20 {
            assert!(matches!(
                conflicts::refresh(f.root(), &conflict).unwrap(),
                conflicts::Refresh::Unchanged
            ));
        }
        let idle_20_ms = start.elapsed().as_secs_f64() * 1000.;
        f.write("file.txt", "external resolution\n");
        let start = Instant::now();
        let conflicts::Refresh::Changed(Some(changed)) =
            conflicts::refresh(f.root(), &conflict).unwrap()
        else {
            panic!("edit missing")
        };
        let changed_ms = start.elapsed().as_secs_f64() * 1000.;
        assert!(changed.states[0].edited && changed.states[0].unmerged);
        assert_eq!(changed.files, ["file.txt"]);
        let start = Instant::now();
        let preview = conflicts::read_preview(f.root(), "file.txt", &changed.identity).unwrap();
        let preview_ms = start.elapsed().as_secs_f64() * 1000.;
        assert_eq!(preview.original.as_deref(), Some("external resolution\n"));
        assert!(!f.git(&["ls-files", "-u"]).is_empty());
        if sample >= 0 {
            println!(
                "{}",
                serde_json::json!({"sample":sample,"metadata_ms":metadata_ms,"idle_20_ms":idle_20_ms,"changed_ms":changed_ms,"preview_ms":preview_ms})
            );
        }
    }
}

#[test]
fn interactive_squash_and_edit_continue_are_recoverable() {
    let f = Fixture::new();
    let base = f.git(&["rev-parse", "HEAD"]);
    f.write("one.txt", "one\n");
    f.commit("first message");
    f.write("two.txt", "two\n");
    f.commit("second message");
    let original = f.git(&["rev-parse", "HEAD"]);
    let p = plan(f.root(), &base).unwrap();
    let text = format!("pick {}\nsquash {}\n", p.commits[0], p.commits[1]);
    f.run(Request::Interactive { plan: p, text });
    assert_eq!(
        f.git(&["rev-list", "--count", &format!("{base}..HEAD")]),
        "1"
    );
    let message = f.git(&["log", "-1", "--format=%B"]);
    assert!(message.contains("first message") && message.contains("second message"));
    f.run(Request::MoveTo(0));
    assert_eq!(f.git(&["rev-parse", "HEAD"]), original);
    let p = plan(f.root(), &base).unwrap();
    let text = format!("edit {}\npick {}\n", p.commits[0], p.commits[1]);
    let stopped = execute(f.root(), Request::Interactive { plan: p, text });
    assert!(stopped.result.unwrap_err().contains("paused"));
    let stopped = stopped.view;
    assert_eq!(stopped.conflict.as_ref().unwrap().kind, "Rebase");
    assert!(stopped.conflict.unwrap().files.is_empty());
    f.git(&["commit", "--amend", "-m", "edited message"]);
    f.run(Request::Continue);
    assert_eq!(
        f.git(&["log", "-1", "--format=%s", "HEAD~1"]),
        "edited message"
    );
    assert_eq!(
        f.git(&["rev-list", "--count", &format!("{base}..HEAD")]),
        "2"
    );
    f.run(Request::MoveTo(0));
    assert_eq!(f.git(&["rev-parse", "HEAD"]), original);
    f.run(Request::MoveTo(1));
    assert_eq!(
        f.git(&["log", "-1", "--format=%s", "HEAD~1"]),
        "edited message"
    );
}

#[test]
fn checkpoints_survive_gc_and_reopening_with_staged_only_content() {
    let f = Fixture::new();
    f.write("file.txt", "only staged\n");
    f.git(&["add", "."]);
    f.write("file.txt", "working\n");
    f.run(Request::Create {
        name: "topic".into(),
        target: "HEAD".into(),
    });
    f.git(&["gc", "--prune=now"]);
    let c = Context::new(f.root()).unwrap();
    fs::write(c.directory.join("journal.json.lock"), "interrupted save").unwrap();
    f.run(Request::Inspect);
    f.run(Request::MoveTo(0));
    assert_eq!(f.git(&["show", ":file.txt"]), "only staged");
}

#[test]
fn current_branch_rename_and_detached_checkout_roundtrip() {
    let f = Fixture::new();
    f.run(Request::Rename {
        old: "main".into(),
        new: "renamed".into(),
    });
    f.run(Request::MoveTo(0));
    assert_eq!(f.git(&["branch", "--show-current"]), "main");
    f.run(Request::MoveTo(1));
    assert_eq!(f.git(&["branch", "--show-current"]), "renamed");
    let id = gix::ObjectId::from_hex(f.git(&["rev-parse", "HEAD"]).as_bytes()).unwrap();
    f.run(Request::Checkout(crate::checkout::Request::Commit {
        id,
        remote: None,
    }));
    assert!(f.git(&["branch", "--show-current"]).is_empty());
    f.run(Request::MoveTo(1));
    assert_eq!(f.git(&["branch", "--show-current"]), "renamed");
    f.run(Request::MoveTo(2));
    assert!(f.git(&["branch", "--show-current"]).is_empty());
}

#[test]
fn checkpoints_preserve_packed_and_symbolic_refs_without_a_user_identity() {
    let f = Fixture::new();
    f.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    f.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ]);
    f.git(&["pack-refs", "--all", "--prune"]);
    f.git(&["config", "--unset", "user.name"]);
    f.git(&["config", "--unset", "user.email"]);
    f.git(&["config", "core.logAllRefUpdates", "always"]);
    f.run(Request::Create {
        name: "topic".into(),
        target: "HEAD".into(),
    });
    f.run(Request::MoveTo(0));
    assert_eq!(
        f.git(&["symbolic-ref", "refs/remotes/origin/HEAD"]),
        "refs/remotes/origin/main"
    );
    assert_eq!(
        f.git(&["rev-parse", "refs/remotes/origin/main"]),
        f.git(&["rev-parse", "HEAD"])
    );
    f.run(Request::MoveTo(1));
    assert_eq!(
        f.git(&["rev-parse", "topic"]),
        f.git(&["rev-parse", "HEAD"])
    );
}

#[test]
fn ignored_collision_blocks_restore_without_overwriting() {
    let f = Fixture::new();
    f.git(&["switch", "-c", "topic"]);
    f.write("collision", "tracked\n");
    f.commit("add file");
    f.run(Request::Checkout(crate::checkout::Request::Branch(
        crate::branches::Target {
            name: "main".into(),
            remote: None,
            url: None,
            sha: f.git(&["rev-parse", "main"]),
        },
    )));
    f.write(".git/info/exclude", "collision\n");
    f.write("collision", "ignored local data\n");
    assert!(execute(f.root(), Request::MoveTo(0)).result.is_err());
    assert_eq!(
        fs::read_to_string(f.root().join("collision")).unwrap(),
        "ignored local data\n"
    );
    assert_eq!(f.git(&["branch", "--show-current"]), "main");
}

#[test]
fn rebase_skip_and_modify_delete_resolution() {
    let f = Fixture::new();
    f.conflict();
    let out = execute(f.root(), Request::Rebase("topic".into()));
    assert!(out.view.conflict.is_some());
    f.run(Request::Skip);
    assert_eq!(
        f.git(&["rev-parse", "HEAD"]),
        f.git(&["rev-parse", "topic"])
    );
    f.run(Request::MoveTo(0));
    f.git(&["switch", "-c", "deleted", "main~1"]);
    f.git(&["rm", "file.txt"]);
    f.commit("delete");
    f.git(&["switch", "main"]);
    let out = execute(f.root(), Request::Merge("deleted".into()));
    assert!(out.view.conflict.is_some());
    let resolved = f
        .run(Request::Resolve {
            path: "file.txt".into(),
            choice: Resolution::Theirs,
        })
        .conflict
        .unwrap();
    assert!(resolved.files.is_empty());
    assert!(
        resolved
            .states
            .iter()
            .any(|state| state.path == "file.txt" && !state.exists && !state.unmerged)
    );
    f.run(Request::Continue);
    assert!(!f.root().join("file.txt").exists());
}

#[test]
fn autocrlf_restore_refreshes_index_before_next_checkout() {
    let f = Fixture::new();
    f.git(&["config", "core.autocrlf", "true"]);
    f.conflict();
    execute(f.root(), Request::Merge("topic".into()));
    f.run(Request::Resolve {
        path: "file.txt".into(),
        choice: Resolution::Theirs,
    });
    f.run(Request::Continue);
    f.run(Request::MoveTo(0));
    assert!(f.git(&["status", "--porcelain=v1"]).is_empty());
    f.git(&["switch", "topic"]);
}

#[test]
#[ignore = "15 measured disposable mutation samples, with one warm-up"]
fn benchmark_operations() {
    use std::time::Instant;
    let mut samples = Vec::new();
    for sample in 0..16 {
        let f = Fixture::new();
        let start = Instant::now();
        f.run(Request::Create {
            name: "topic".into(),
            target: "HEAD".into(),
        });
        let create = start.elapsed().as_secs_f64() * 1000.;
        let start = Instant::now();
        f.run(Request::MoveTo(0));
        let undo = start.elapsed().as_secs_f64() * 1000.;
        let start = Instant::now();
        f.run(Request::MoveTo(1));
        let redo = start.elapsed().as_secs_f64() * 1000.;
        f.git(&["switch", "topic"]);
        f.write("new", "new");
        f.commit("new");
        f.git(&["switch", "main"]);
        let start = Instant::now();
        f.run(Request::Merge("topic".into()));
        let merge = start.elapsed().as_secs_f64() * 1000.;
        let start = Instant::now();
        f.run(Request::Rename {
            old: "topic".into(),
            new: "renamed".into(),
        });
        let rename = start.elapsed().as_secs_f64() * 1000.;
        let start = Instant::now();
        f.run(Request::Delete {
            name: "renamed".into(),
            force: false,
        });
        let delete = start.elapsed().as_secs_f64() * 1000.;
        let replay = Fixture::new();
        replay.git(&["switch", "-c", "topic"]);
        replay.write("topic", "topic");
        replay.commit("topic");
        replay.git(&["switch", "main"]);
        replay.write("main", "main");
        replay.commit("main");
        let start = Instant::now();
        replay.run(Request::Rebase("topic".into()));
        let rebase = start.elapsed().as_secs_f64() * 1000.;
        let start = Instant::now();
        let plan = replay.run(Request::Plan("topic".into())).plan.unwrap();
        let plan_ms = start.elapsed().as_secs_f64() * 1000.;
        let text = format!("reword {} renamed commit", plan.commits[0]);
        let start = Instant::now();
        replay.run(Request::Interactive { plan, text });
        let interactive = start.elapsed().as_secs_f64() * 1000.;
        let conflict = Fixture::new();
        conflict.conflict();
        let start = Instant::now();
        let stopped = execute(conflict.root(), Request::Merge("topic".into()));
        assert!(stopped.view.conflict.is_some());
        let conflict_ms = start.elapsed().as_secs_f64() * 1000.;
        let start = Instant::now();
        conflict.run(Request::Resolve {
            path: "file.txt".into(),
            choice: Resolution::Theirs,
        });
        let resolve = start.elapsed().as_secs_f64() * 1000.;
        let start = Instant::now();
        conflict.run(Request::Continue);
        let continue_ms = start.elapsed().as_secs_f64() * 1000.;
        let remote = Fixture::new();
        let clone_dir = tempfile::tempdir().unwrap();
        let clone = clone_dir.path().join("clone");
        output(
            clone_dir.path(),
            &[
                "clone",
                remote.root().to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        )
        .unwrap();
        remote.write("upstream", "upstream");
        remote.commit("upstream");
        let start = Instant::now();
        let pulled = execute(&clone, Request::Pull);
        assert!(pulled.result.is_ok(), "{:?}", pulled.result);
        let pull = start.elapsed().as_secs_f64() * 1000.;
        output(&clone, &["switch", "-c", "published"]).unwrap();
        let review = preview_push(&clone).unwrap();
        let start = Instant::now();
        let pushed = execute(&clone, Request::PushReviewed(review));
        assert!(pushed.result.is_ok(), "{:?}", pushed.result);
        let push = start.elapsed().as_secs_f64() * 1000.;
        if sample > 0 {
            samples.push(serde_json::json!({"sample":sample,"create_ms":create,"undo_ms":undo,"redo_ms":redo,"merge_ms":merge,"rename_ms":rename,"delete_ms":delete,"rebase_ms":rebase,"plan_ms":plan_ms,"interactive_ms":interactive,"conflict_ms":conflict_ms,"resolve_ms":resolve,"continue_ms":continue_ms,"pull_local_ms":pull,"push_local_ms":push}));
        }
    }
    println!("{}", serde_json::to_string_pretty(&samples).unwrap());
}

#[test]
fn first_commit_roundtrip_with_no_original_index() {
    let f = Fixture::new();
    f.git(&["update-ref", "-d", "refs/heads/main"]);
    fs::remove_file(f.root().join(".git/index")).unwrap();
    let repo = super::super::open(f.root()).unwrap();
    let status = super::super::worktree::snapshot(&repo).unwrap();
    f.run(Request::Commit(crate::pending::Request {
        head: status.head,
        branch: status.branch,
        new_branch: String::new(),
        description: "initial".into(),
        files: status.files,
    }));
    f.run(Request::MoveTo(0));
    assert!(output(f.root(), &["rev-parse", "--verify", "HEAD"]).is_err());
    assert!(!f.root().join(".git/index").exists());
    assert_eq!(
        fs::read_to_string(f.root().join("file.txt")).unwrap(),
        "base\n"
    );
    f.run(Request::MoveTo(1));
    assert!(f.git(&["status", "--porcelain"]).is_empty());
}

#[test]
#[ignore = "1,001-file clean/dirty checkpoint measurements; one warm-up and 15 samples"]
fn benchmark_large_worktree_operations() {
    use std::time::Instant;
    for dirty in [false, true] {
        let f = Fixture::new();
        for i in 0..1000 {
            f.write(
                &format!("file-{i}.txt"),
                &format!("{i}\n{}", "content\n".repeat(128)),
            );
        }
        f.commit("thousand files");
        if dirty {
            for i in 0..100 {
                f.write(&format!("file-{i}.txt"), "staged\n");
            }
            f.git(&["add", "."]);
            for i in 0..100 {
                f.write(&format!("file-{i}.txt"), "working\n");
            }
            f.write("untracked.txt", "untracked\n");
        }
        for sample in -1..15 {
            let start = Instant::now();
            f.run(Request::Create {
                name: "topic".into(),
                target: "HEAD".into(),
            });
            let create = start.elapsed().as_secs_f64() * 1000.;
            let start = Instant::now();
            f.run(Request::MoveTo(0));
            let undo = start.elapsed().as_secs_f64() * 1000.;
            let start = Instant::now();
            f.run(Request::MoveTo(1));
            let redo = start.elapsed().as_secs_f64() * 1000.;
            f.run(Request::MoveTo(0));
            if dirty {
                assert_eq!(f.git(&["show", ":file-0.txt"]), "staged");
                assert_eq!(
                    fs::read_to_string(f.root().join("file-0.txt")).unwrap(),
                    "working\n"
                );
                assert!(f.root().join("untracked.txt").exists());
            }
            if sample >= 0 {
                println!(
                    "{}",
                    serde_json::json!({"sample": sample, "dirty":dirty,"create_ms":create,"undo_ms":undo,"redo_ms":redo})
                );
            }
        }
    }
}

#[test]
fn split_index_and_corrupt_checkpoint_are_handled_safely() {
    let f = Fixture::new();
    f.git(&["update-index", "--split-index"]);
    f.run(Request::Create {
        name: "topic".into(),
        target: "HEAD".into(),
    });
    f.run(Request::MoveTo(0));
    f.run(Request::MoveTo(1));
    let context = Context::new(f.root()).unwrap();
    let journal = context.load().unwrap();
    let file = context.directory.join(format!(
        "index-{}",
        journal.entries[0].before.index.as_ref().unwrap()
    ));
    fs::write(file, "corrupted checkpoint").unwrap();
    assert!(execute(f.root(), Request::MoveTo(0)).result.is_err());
    assert!(f.git(&["branch", "--list", "topic"]).contains("topic"));
    assert_eq!(
        fs::read_to_string(f.root().join("file.txt")).unwrap(),
        "base\n"
    );
}

#[test]
fn hidden_worktree_edits_cannot_enter_recovery_history() {
    for flag in ["--assume-unchanged", "--skip-worktree"] {
        let f = Fixture::new();
        f.git(&["update-index", flag, "file.txt"]);
        f.write("file.txt", "hidden local changes\n");
        assert!(f.git(&["status", "--porcelain=v1"]).is_empty());
        let result = execute(
            f.root(),
            Request::Create {
                name: "topic".into(),
                target: "HEAD".into(),
            },
        );
        assert!(result.result.unwrap_err().contains("index flags"));
        assert!(f.git(&["branch", "--list", "topic"]).is_empty());
        assert_eq!(
            fs::read_to_string(f.root().join("file.txt")).unwrap(),
            "hidden local changes\n"
        );
    }
}

#[test]
fn capture_keeps_racy_index_timestamp_checks() {
    let f = Fixture::new();
    let timestamp = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let file = f.root().join("file.txt");
    let set_time = |path: &Path| {
        OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(timestamp)
            .unwrap()
    };
    set_time(&file);
    f.git(&["update-index", "--refresh"]);
    set_time(&f.root().join(".git/index"));
    f.write("file.txt", "edit\n");
    set_time(&file);
    assert!(!f.git(&["status", "--porcelain=v1"]).is_empty());
    f.run(Request::Create {
        name: "topic".into(),
        target: "HEAD".into(),
    });
    let context = Context::new(f.root()).unwrap();
    let journal = context.load().unwrap();
    assert_eq!(
        f.git(&[
            "show",
            &format!("{}:file.txt", journal.entries[0].before.tree)
        ]),
        "edit"
    );
    assert_eq!(f.git(&["show", ":file.txt"]), "base");
    f.run(Request::MoveTo(0));
    assert_eq!(fs::read_to_string(file).unwrap(), "edit\n");
}

fn push_review_fixture() -> (Fixture, tempfile::TempDir, PathBuf) {
    let source = Fixture::new();
    source.git(&["config", "receive.denyCurrentBranch", "ignore"]);
    let dir = tempfile::tempdir().unwrap();
    let clone = dir.path().join("clone");
    output(
        dir.path(),
        &[
            "clone",
            source.root().to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    )
    .unwrap();
    output(&clone, &["config", "user.name", "Review Test"]).unwrap();
    output(&clone, &["config", "user.email", "review@example.invalid"]).unwrap();
    output(&clone, &["config", "commit.gpgSign", "false"]).unwrap();
    output(&clone, &["config", "core.hooksPath", "disabled-hooks"]).unwrap();
    (source, dir, clone)
}

#[test]
fn push_review_normal_and_new_branch_only_publish_current_branch() {
    let (source, _dir, clone) = push_review_fixture();
    let review = preview_push(&clone).unwrap();
    assert_eq!(review.risk, PushRisk::Trivial);
    output(&clone, &["switch", "-c", "published"]).unwrap();
    output(&clone, &["commit", "--allow-empty", "-m", "published"]).unwrap();
    output(&clone, &["branch", "unrelated"]).unwrap();
    let review = preview_push(&clone).unwrap();
    assert_eq!(review.risk, PushRisk::Trivial);
    assert!(review.expected.is_none());
    let head = review.head.clone();
    source.git(&["branch", "keep-remote"]);
    output(&clone, &["config", "remote.origin.mirror", "true"]).unwrap();
    let result = execute(&clone, Request::PushReviewed(review));
    assert!(result.result.is_ok(), "{:?}", result.result);
    assert!(result.refresh_remotes);
    assert_eq!(source.git(&["rev-parse", "published"]), head);
    assert_eq!(
        output(&clone, &["rev-parse", "origin/published"]).unwrap(),
        head
    );
    assert!(!source.git(&["branch", "--list", "keep-remote"]).is_empty());
    assert!(source.git(&["branch", "--list", "unrelated"]).is_empty());
    assert_eq!(
        output(&clone, &["config", "branch.published.merge"]).unwrap(),
        "refs/heads/published"
    );
}

#[test]
fn push_review_orange_rewrite_uses_frozen_lease_even_after_fetch() {
    let (source, _dir, clone) = push_review_fixture();
    output(&clone, &["commit", "--amend", "-m", "rewritten"]).unwrap();
    let review = preview_push(&clone).unwrap();
    assert_eq!(review.risk, PushRisk::Rewrite);
    source.write("other", "other person's work");
    source.commit("remote advanced after review");
    let remote_head = source.git(&["rev-parse", "HEAD"]);
    output(&clone, &["fetch", "origin"]).unwrap();
    let result = execute(&clone, Request::PushReviewed(review));
    assert!(result.result.is_err());
    assert_eq!(source.git(&["rev-parse", "HEAD"]), remote_head);
}

#[test]
fn push_review_rewrite_succeeds_with_unchanged_lease() {
    let (source, _dir, clone) = push_review_fixture();
    output(&clone, &["commit", "--amend", "-m", "rewritten"]).unwrap();
    let review = preview_push(&clone).unwrap();
    assert_eq!(review.risk, PushRisk::Rewrite);
    let head = review.head.clone();
    let result = execute(&clone, Request::PushReviewed(review));
    assert!(result.result.is_ok(), "{:?}", result.result);
    assert_eq!(source.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(output(&clone, &["rev-parse", "origin/main"]).unwrap(), head);
}

#[test]
fn push_review_red_never_overwrites_remote_or_seals_history() {
    let (source, _dir, clone) = push_review_fixture();
    execute(
        &clone,
        Request::Create {
            name: "checkpoint".into(),
            target: "HEAD".into(),
        },
    );
    let tracking = output(&clone, &["rev-parse", "origin/main"]).unwrap();
    source.write("other", "unseen work");
    source.commit("unseen remote work");
    let outcome = execute(&clone, Request::PreviewPush);
    assert!(!outcome.refresh_remotes);
    assert!(!outcome.refresh_repository);
    assert_eq!(outcome.view.cursor, 1);
    let review = outcome.view.push.unwrap();
    assert_eq!(review.risk, PushRisk::Blocked);
    assert_eq!(
        output(&clone, &["rev-parse", "origin/main"]).unwrap(),
        tracking
    );
    let remote_head = source.git(&["rev-parse", "HEAD"]);
    let result = execute(&clone, Request::PushReviewed(review));
    assert!(result.result.is_err());
    assert_eq!(result.view.cursor, 1);
    assert!(!result.refresh_remotes);
    assert_eq!(source.git(&["rev-parse", "HEAD"]), remote_head);
}

#[test]
fn push_review_rejects_changed_branch_and_destination() {
    let (source, _dir, clone) = push_review_fixture();
    let review = preview_push(&clone).unwrap();
    output(&clone, &["switch", "-c", "different"]).unwrap();
    assert!(
        execute(&clone, Request::PushReviewed(review.clone()))
            .result
            .is_err()
    );
    output(&clone, &["switch", "main"]).unwrap();
    output(
        &clone,
        &["config", "remote.origin.pushurl", "nonexistent-path"],
    )
    .unwrap();
    assert!(
        execute(&clone, Request::PushReviewed(review.clone()))
            .result
            .is_err()
    );
    assert_eq!(source.git(&["rev-parse", "HEAD"]), review.head);
}

#[test]
fn push_review_honors_upstream_destination_and_push_remote_override() {
    let (source, _dir, clone) = push_review_fixture();
    source.git(&["branch", "destination"]);
    output(
        &clone,
        &["config", "branch.main.merge", "refs/heads/destination"],
    )
    .unwrap();
    assert_eq!(preview_push(&clone).unwrap().branch, "destination");
    output(
        &clone,
        &["remote", "add", "publish", source.root().to_str().unwrap()],
    )
    .unwrap();
    output(&clone, &["config", "branch.main.pushRemote", "publish"]).unwrap();
    let review = preview_push(&clone).unwrap();
    assert_eq!(review.remote, "publish");
    assert_eq!(review.branch, "main");
    output(
        &clone,
        &["config", "--add", "remote.publish.pushurl", "one"],
    )
    .unwrap();
    output(
        &clone,
        &["config", "--add", "remote.publish.pushurl", "two"],
    )
    .unwrap();
    assert!(preview_push(&clone).unwrap_err().contains("exactly one"));
}

#[test]
fn contextual_integration_rejects_branch_change_at_same_commit() {
    let f = Fixture::new();
    let head = f.git(&["rev-parse", "HEAD"]);
    f.git(&["switch", "-c", "other"]);
    let result = execute(
        f.root(),
        Request::OnBranch {
            branch: "main".into(),
            head,
            request: Box::new(Request::Merge("main".into())),
        },
    );
    assert!(result.result.unwrap_err().contains("branch changed"));
    assert!(result.view.labels.is_empty());
}

#[test]
fn interactive_plan_rejects_switch_to_another_branch_at_same_commit() {
    let f = Fixture::new();
    f.write("next", "next");
    f.commit("next");
    let plan = plan(f.root(), "HEAD~1").unwrap();
    f.git(&["switch", "-c", "different"]);
    let result = execute(
        f.root(),
        Request::Interactive {
            text: plan.text.clone(),
            plan,
        },
    );
    assert!(result.result.unwrap_err().contains("HEAD changed"));
}

#[test]
fn push_destination_preserves_branch_case_and_config_value_boundaries() {
    let (source, _dir, clone) = push_review_fixture();
    output(&clone, &["switch", "-c", "Topic.MixedCase"]).unwrap();
    output(
        &clone,
        &["remote", "add", "publish", source.root().to_str().unwrap()],
    )
    .unwrap();
    output(
        &clone,
        &["config", "branch.Topic.MixedCase.pushRemote", "publish"],
    )
    .unwrap();
    output(&clone, &["config", "rustle.unrelated", "one\ntwo"]).unwrap();
    let review = preview_push(&clone).unwrap();
    assert_eq!(review.remote, "publish");
    assert_eq!(review.local_branch, "Topic.MixedCase");
    assert_eq!(review.branch, "Topic.MixedCase");
}

#[test]
fn stash_create_named_and_unnamed_are_listed_and_recoverable() {
    for message in [Some("work in progress".to_string()), None] {
        let f = Fixture::new();
        f.write("file.txt", "staged\n");
        f.git(&["add", "file.txt"]);
        f.write("file.txt", "staged and unstaged\n");
        f.write("new.txt", "untracked\n");
        f.run(Request::CreateStash(message.clone()));
        let entries = f.run(Request::ListStashes).stashes.unwrap();
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0]
                .subject
                .contains(message.as_deref().unwrap_or("WIP on"))
        );
        assert!(f.git(&["status", "--porcelain"]).is_empty());
        assert!(!f.root().join("new.txt").exists());
        f.run(Request::MoveTo(0));
        assert!(stashes::list(f.root()).unwrap().is_empty());
        assert_eq!(
            fs::read_to_string(f.root().join("file.txt")).unwrap(),
            "staged and unstaged\n"
        );
        assert_eq!(
            fs::read_to_string(f.root().join("new.txt")).unwrap(),
            "untracked\n"
        );
        assert_eq!(f.git(&["show", ":file.txt"]), "staged");
        f.run(Request::MoveTo(1));
        assert_eq!(stashes::list(f.root()).unwrap(), entries);
        assert!(f.git(&["status", "--porcelain"]).is_empty());
    }
}

#[test]
fn stash_listing_survives_named_unnamed_and_external_drop() {
    let f = Fixture::new();
    f.write("file.txt", "named\n");
    f.git(&["stash", "push", "-m", "named stash"]);
    f.write("file.txt", "unnamed\n");
    f.git(&["stash", "push"]);
    let entries = f.run(Request::ListStashes).stashes.unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries[0].subject.starts_with("WIP on"));
    assert!(entries[1].subject.ends_with("named stash"));
    f.git(&["stash", "drop", "stash@{0}"]);
    assert_eq!(
        f.run(Request::ListStashes).stashes.unwrap()[0].oid,
        entries[1].oid
    );
}

#[test]
#[ignore = "One warmup and fifteen disposable stash creation/list samples"]
fn benchmark_stash_create() {
    use std::time::Instant;
    for sample in 0..16 {
        let f = Fixture::new();
        f.write("file.txt", "staged\n");
        f.git(&["add", "file.txt"]);
        f.write("file.txt", "unstaged\n");
        f.write("new.txt", "untracked\n");
        let started = Instant::now();
        f.run(Request::CreateStash(Some("benchmark named stash".into())));
        let create_ms = started.elapsed().as_secs_f64() * 1000.;
        let started = Instant::now();
        let entries = f.run(Request::ListStashes).stashes.unwrap();
        let list_ms = started.elapsed().as_secs_f64() * 1000.;
        assert_eq!(entries.len(), 1);
        assert!(f.git(&["status", "--porcelain"]).is_empty());
        if sample > 0 {
            println!(
                "{}",
                serde_json::json!({"sample":sample,"create_ms":create_ms,"list_ms":list_ms})
            );
        }
    }
}

#[test]
fn stash_list_carries_the_version_read_before_listing() {
    let f = Fixture::new();
    f.write("file.txt", "named\n");
    f.git(&["stash", "push", "-m", "named"]);
    f.write("file.txt", "unnamed\n");
    f.git(&["stash", "push"]);
    let context = Context::new(f.root()).unwrap();
    let before = super::super::worktree::stash_stamp(&context.common).unwrap();
    let view = f.run(Request::ListStashes);
    assert_eq!(view.stash_stamp, Some(before));
    assert_eq!(view.stashes.unwrap().len(), 2);
    f.git(&["stash", "drop"]);
    let view = f.run(Request::ListStashes);
    assert_ne!(view.stash_stamp, Some(before));
    assert_eq!(view.stashes.unwrap().len(), 1);
}

#[test]
fn cherry_pick_order_is_frozen_and_recovery_restores_refs_index_and_worktree() {
    let f = Fixture::new();
    let base = f.git(&["rev-parse", "HEAD"]);
    f.git(&["switch", "-c", "topic"]);
    f.write("first.txt", "first\n");
    f.commit("first pick");
    let first = f.git(&["rev-parse", "HEAD"]);
    f.write("second.txt", "depends on first\n");
    f.commit("second pick");
    let second = f.git(&["rev-parse", "HEAD"]);
    f.git(&["switch", "main"]);
    f.write("unrelated.txt", "main only\n");
    f.commit("destination");
    let before = f.git(&["rev-parse", "HEAD"]);
    let index = f.git(&["ls-files", "--stage"]);
    f.run(Request::CherryPick(vec![first.clone(), second.clone()]));
    assert_eq!(
        f.git(&["log", "-2", "--format=%s"]),
        "second pick\nfirst pick"
    );
    assert_eq!(f.git(&["show", "HEAD~2:unrelated.txt"]), "main only");
    assert_eq!(f.git(&["rev-parse", "topic"]), second);
    assert!(f.git(&["status", "--porcelain"]).is_empty());
    let after = f.git(&["rev-parse", "HEAD"]);
    f.run(Request::MoveTo(0));
    assert_eq!(f.git(&["rev-parse", "HEAD"]), before);
    assert_eq!(f.git(&["ls-files", "--stage"]), index);
    assert!(!f.root().join("first.txt").exists());
    f.run(Request::MoveTo(1));
    assert_eq!(f.git(&["rev-parse", "HEAD"]), after);
    assert_eq!(f.git(&["rev-parse", "topic~2"]), base);
}

#[test]
fn cherry_pick_validates_entire_batch_and_destination_before_mutation() {
    let f = Fixture::new();
    f.git(&["switch", "-c", "topic"]);
    f.write("new.txt", "topic\n");
    f.commit("topic");
    let tip = f.git(&["rev-parse", "HEAD"]);
    f.git(&["switch", "main"]);
    let before = f.git(&["rev-parse", "HEAD"]);
    for commits in [
        vec![],
        vec![tip.clone(), tip.clone()],
        vec![tip.clone(), "missing".into()],
        vec![tip.clone(), "1".repeat(40)],
    ] {
        assert!(
            execute(f.root(), Request::CherryPick(commits))
                .result
                .is_err()
        );
        assert_eq!(f.git(&["rev-parse", "HEAD"]), before);
        assert!(!f.root().join("new.txt").exists());
    }
    f.write("pending.txt", "preserve\n");
    assert!(
        execute(f.root(), Request::CherryPick(vec![tip.clone()]))
            .result
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(f.root().join("pending.txt")).unwrap(),
        "preserve\n"
    );
    fs::remove_file(f.root().join("pending.txt")).unwrap();
    assert!(
        execute(
            f.root(),
            Request::OnBranch {
                branch: "topic".into(),
                head: before.clone(),
                request: Box::new(Request::CherryPick(vec![tip]))
            }
        )
        .result
        .is_err()
    );
    assert_eq!(f.git(&["rev-parse", "HEAD"]), before);
}

#[test]
fn cherry_pick_conflict_continue_skip_abort_and_undo_preserve_sequence() {
    for finish in ["continue", "skip", "abort"] {
        let f = Fixture::new();
        f.git(&["switch", "-c", "topic"]);
        f.write("first.txt", "first\n");
        f.commit("first");
        let first = f.git(&["rev-parse", "HEAD"]);
        f.write("file.txt", "incoming\n");
        f.commit("conflict");
        let conflict = f.git(&["rev-parse", "HEAD"]);
        f.write("last.txt", "last\n");
        f.commit("last");
        let last = f.git(&["rev-parse", "HEAD"]);
        f.git(&["switch", "main"]);
        f.write("file.txt", "local\n");
        f.commit("destination");
        let before = f.git(&["rev-parse", "HEAD"]);
        let paused = execute(
            f.root(),
            Request::CherryPick(vec![first, conflict, last.clone()]),
        );
        assert!(paused.result.is_err());
        let state = paused.view.conflict.unwrap();
        assert_eq!(state.kind, "Cherry-pick");
        assert!(execute(f.root(), Request::Continue).result.is_err());
        assert!(
            execute(
                f.root(),
                Request::Create {
                    name: "blocked".into(),
                    target: "HEAD".into()
                }
            )
            .result
            .is_err()
        );
        match finish {
            "continue" => {
                f.write("file.txt", "resolved\n");
                f.git(&["add", "file.txt"]);
                f.run(Request::Continue);
            }
            "skip" => {
                f.run(Request::Skip);
            }
            _ => {
                f.run(Request::Abort);
            }
        }
        assert!(!f.root().join(".git/CHERRY_PICK_HEAD").exists());
        assert!(!f.root().join(".git/sequencer").exists());
        assert_eq!(f.git(&["rev-parse", "topic"]), last);
        if finish == "abort" {
            assert_eq!(f.git(&["rev-parse", "HEAD"]), before);
            assert!(!f.root().join("first.txt").exists());
            assert!(!f.root().join("last.txt").exists());
        } else {
            assert!(f.root().join("first.txt").exists() && f.root().join("last.txt").exists());
            assert_eq!(
                fs::read_to_string(f.root().join("file.txt")).unwrap(),
                if finish == "continue" {
                    "resolved\n"
                } else {
                    "local\n"
                }
            );
            f.run(Request::MoveTo(0));
            assert_eq!(f.git(&["rev-parse", "HEAD"]), before);
            assert!(!f.root().join("first.txt").exists());
        }
    }
}

#[test]
fn cherry_pick_merge_is_rejected_without_mainline_before_any_pick() {
    let f = Fixture::new();
    f.git(&["switch", "-c", "topic"]);
    f.write("topic.txt", "topic\n");
    f.commit("topic");
    let tip = f.git(&["rev-parse", "HEAD"]);
    f.git(&["switch", "main"]);
    f.write("main.txt", "main\n");
    f.commit("main");
    f.git(&["merge", "--no-ff", "--no-edit", "topic"]);
    let merge = f.git(&["rev-parse", "HEAD"]);
    assert!(
        execute(f.root(), Request::CherryPick(vec![tip, merge.clone()]))
            .result
            .is_err()
    );
    assert_eq!(f.git(&["rev-parse", "HEAD"]), merge);
    assert!(!f.root().join(".git/sequencer").exists());
}

#[test]
fn cherry_pick_rechecks_frozen_destination_after_expensive_preparation() {
    let f = Fixture::new();
    f.git(&["switch", "-c", "topic"]);
    f.write("pick.txt", "pick\n");
    f.commit("pick");
    let tip = f.git(&["rev-parse", "HEAD"]);
    f.git(&["switch", "main"]);
    let before = f.git(&["rev-parse", "HEAD"]);
    f.git(&["branch", "other"]);
    let context = Context::new(f.root()).unwrap();
    let mut started = false;
    let result = cherry_pick::apply_with_before_run(
        &context,
        &[tip],
        Some(("main", &before)),
        &mut started,
        || {
            f.git(&["switch", "other"]);
        },
    );
    assert!(
        result
            .unwrap_err()
            .contains("changed during cherry-pick preparation")
    );
    assert!(!started);
    assert_eq!(f.git(&["branch", "--show-current"]), "other");
    assert_eq!(f.git(&["rev-parse", "main"]), before);
    assert_eq!(f.git(&["rev-parse", "other"]), before);
    assert!(!f.root().join("pick.txt").exists());
    assert!(!f.root().join(".git/sequencer").exists());
}

#[test]
fn cherry_pick_preparation_race_does_not_record_or_restore_external_checkout() {
    let f = Fixture::new();
    f.git(&["switch", "-c", "topic"]);
    f.write("pick.txt", "pick\n");
    f.commit("pick");
    let tip = f.git(&["rev-parse", "HEAD"]);
    f.git(&["switch", "main"]);
    let head = f.git(&["rev-parse", "HEAD"]);
    f.git(&["branch", "other"]);
    f.run(Request::Create {
        name: "history-marker".into(),
        target: "HEAD".into(),
    });
    // A rejected invalid batch with unchanged repository preserves prior history.
    let invalid = execute(f.root(), Request::CherryPick(vec!["invalid".into()]));
    assert!(invalid.result.is_err());
    assert_eq!(invalid.view.labels.len(), 1);
    let root = f.root().to_path_buf();
    cherry_pick::set_before_run(move || {
        command::checked(&root, &["switch", "other"]).unwrap();
    });
    let result = execute(
        f.root(),
        Request::OnBranch {
            branch: "main".into(),
            head: head.clone(),
            request: Box::new(Request::CherryPick(vec![tip])),
        },
    );
    assert!(
        result
            .result
            .unwrap_err()
            .contains("changed during cherry-pick preparation")
    );
    assert!(result.view.labels.is_empty());
    assert_eq!(result.view.cursor, 0);
    assert!(result.view.boundary.contains("outside Git Rustle"));
    assert_eq!(f.git(&["branch", "--show-current"]), "other");
    assert_eq!(f.git(&["rev-parse", "main"]), head);
    assert_eq!(f.git(&["rev-parse", "other"]), head);
    assert!(!f.root().join("pick.txt").exists());
    let journal = Context::new(f.root()).unwrap().load().unwrap();
    assert!(journal.pending.is_none());
    assert!(journal.entries.is_empty());
}
