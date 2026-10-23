//! Automatic merge/rebase using the guarded Git operations and recovery journal of the UI.
use super::*;
use crate::git::operations::{self, Request};

pub struct Target {
    pub task: Task,
    pub branch: String,
    pub head: String,
    pub target: String,
}
impl Target {
    fn kind(&self) -> Result<&'static str, String> {
        match self.task {
            Task::Merge => Ok("Merge"),
            Task::Rebase => Ok("Rebase"),
            _ => Err("Unsupported automatic Git operation".into()),
        }
    }
}

pub fn run(
    root: &Path,
    settings: &Settings,
    target: &Target,
    cancel: Arc<AtomicBool>,
    progress: impl Fn(&str),
) -> Result<String, String> {
    run_with(root, target, &cancel, progress, |context| {
        let response = super::run(settings, target.task, context, cancel.clone())?;
        parse_resolution(&response)
    })
}

fn run_with(
    root: &Path,
    integration: &Target,
    cancel: &AtomicBool,
    progress: impl Fn(&str),
    resolve: impl Fn(&str) -> Result<String, String>,
) -> Result<String, String> {
    let kind = integration.kind()?;
    let name = kind.to_lowercase();
    let branch = &integration.branch;
    let target = &integration.target;
    let check_cancel = || {
        if cancel.load(Ordering::Acquire) {
            Err(format!(
                "AI {name} stopped. Any paused {name} can be continued or aborted in Conflicts."
            ))
        } else {
            Ok(())
        }
    };
    check_cancel()?;
    progress(&format!(
        "AI {name} · integrating changes into the current branch…"
    ));
    let mut outcome = operations::execute(
        root,
        Request::OnBranch {
            branch: branch.into(),
            head: integration.head.clone(),
            request: Box::new(if integration.task == Task::Merge {
                Request::Merge(target.into())
            } else {
                Request::Rebase(target.into())
            }),
        },
    );
    let mut resolved = 0;
    for _ in 0..1024 {
        check_cancel()?;
        let Some(conflict) = outcome.view.conflict else {
            outcome.result?;
            return Ok(format!(
                "AI {name} completed · {resolved} conflict file(s) resolved."
            ));
        };
        if conflict.kind != kind {
            return Err(format!(
                "Another Git operation replaced the AI {name}. Automation stopped."
            ));
        }
        // A failure without unresolved files requires a human decision (e.g. an empty commit).
        if conflict.files.is_empty() {
            outcome.result?;
        }
        for path in &conflict.files {
            check_cancel()?;
            progress(&format!("AI {name} · resolving {path}…"));
            let read = operations::execute(root, Request::ReadConflict(path.clone()));
            read.result?;
            let preview = read.view.preview.ok_or("Cannot load the conflict")?;
            if preview.identity != conflict.identity {
                return Err(
                    "The operation changed while reading a conflict. Automation stopped.".into(),
                );
            }
            let original = preview.original.ok_or_else(|| format!("Cannot automatically resolve binary, deleted, or oversized file {path}. The {name} is paused in Conflicts."))?;
            let roles = if integration.task == Task::Merge {
                "Ours is the current branch; theirs is the incoming branch."
            } else {
                "Ours is the destination branch; theirs is the commit being replayed."
            };
            let mut context = format!(
                "Operation: {kind}\nBranch: {branch}\nTarget: {target}\nFile: {path}\n{roles} Preserve both intents.\n--- OURS ---\n{}\n--- THEIRS ---\n{}\n--- CURRENT DRAFT ---\n{original}",
                preview.ours, preview.theirs
            );
            if let Ok(base) = crate::git::command::checked(root, &["show", &format!(":1:{path}")]) {
                context.push_str("\n--- MERGE BASE ---\n");
                context.push_str(
                    std::str::from_utf8(&base).map_err(|_| "The merge base is not UTF-8")?,
                );
            }
            if context.len() > MAX_CONTEXT {
                return Err(format!(
                    "Conflict {path} exceeds the AI context limit. The {name} is paused in Conflicts."
                ));
            }
            let edited = resolve(&context)?;
            check_cancel()?;
            validate_resolution(&edited)?;
            progress(&format!("AI {name} · saving resolution for {path}…"));
            operations::execute(
                root,
                Request::SaveConflict {
                    path: path.clone(),
                    identity: conflict.identity.clone(),
                    original,
                    edited,
                },
            )
            .result?;
            resolved += 1;
        }
        check_cancel()?;
        progress(&format!("AI {name} · continuing…"));
        outcome = operations::execute(
            root,
            Request::OnConflict {
                identity: conflict.identity,
                request: Box::new(Request::Continue),
            },
        );
    }
    Err(
        "AI integration stopped after 1,024 steps. Continue or abort the paused operation in Conflicts."
            .into(),
    )
}

fn parse_resolution(response: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Resolution {
        resolved: bool,
        content: Option<String>,
        reason: Option<String>,
    }
    let resolution: Resolution = serde_json::from_str(response)
        .map_err(|e| format!("AI returned an invalid resolution; the operation is paused. {e}"))?;
    if !resolution.resolved {
        return Err(resolution.reason.unwrap_or_else(|| {
            "AI could not resolve this conflict. The operation is paused.".into()
        }));
    }
    resolution
        .content
        .ok_or_else(|| "AI did not return resolved file content. The operation is paused.".into())
}

fn validate_resolution(text: &str) -> Result<(), String> {
    if text.trim().is_empty()
        || text.starts_with("```")
        || text
            .lines()
            .any(|l| l.starts_with("<<<<<<<") || l == "=======" || l.starts_with(">>>>>>>"))
    {
        return Err("AI returned an empty resolution, a code fence, or conflict markers. The operation remains paused in Conflicts.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    #[test]
    fn uncertain_or_malformed_ai_answers_are_never_written_as_source() {
        assert!(parse_resolution("I cannot resolve this conflict").is_err());
        assert!(
            parse_resolution(
                r#"{"resolved":false,"content":null,"reason":"Conflicting requirements"}"#
            )
            .unwrap_err()
            .contains("Conflicting requirements")
        );
        assert!(parse_resolution(r#"{"resolved":true,"content":null,"reason":null}"#).is_err());
        assert_eq!(
            parse_resolution(r#"{"resolved":true,"content":"resolved\n","reason":null}"#).unwrap(),
            "resolved\n"
        );
    }
    fn git(root: &Path, args: &[&str]) -> String {
        String::from_utf8(crate::git::command::checked(root, args).unwrap())
            .unwrap()
            .trim()
            .into()
    }
    fn fixture() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-b", "main"]);
        for (k, v) in [
            ("user.name", "AI fixture"),
            ("user.email", "ai@example.invalid"),
            ("commit.gpgsign", "false"),
            ("core.autocrlf", "false"),
            ("core.hooksPath", "disabled-fixture-hooks"),
        ] {
            git(root, &["config", k, v]);
        }
        std::fs::write(root.join("file.txt"), "base\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "Base"]);
        git(root, &["switch", "-c", "destination"]);
        std::fs::write(root.join("file.txt"), "destination\n").unwrap();
        git(root, &["commit", "-am", "Destination"]);
        git(root, &["switch", "main"]);
        for (text, subject) in [("first\n", "First local"), ("second\n", "Second local")] {
            std::fs::write(root.join("file.txt"), text).unwrap();
            git(root, &["commit", "-am", subject]);
        }
        std::fs::write(root.join(".git/info/exclude"), "unrelated.txt\n").unwrap();
        std::fs::write(root.join("unrelated.txt"), "preserve untracked\n").unwrap();
        let head = git(root, &["rev-parse", "HEAD"]);
        (dir, head)
    }
    #[test]
    fn automatic_rebase_resolves_multiple_commits_and_supports_undo_redo() {
        let (dir, head) = fixture();
        let root = dir.path();
        let calls = Cell::new(0);
        let result = run_with(
            root,
            &Target {
                task: Task::Rebase,
                branch: "main".into(),
                head: head.clone(),
                target: "destination".into(),
            },
            &AtomicBool::new(false),
            |_| {},
            |context| {
                assert!(context.contains("--- MERGE BASE ---"));
                calls.set(calls.get() + 1);
                Ok(format!("resolved {}\n", calls.get()))
            },
        )
        .unwrap();
        assert!(result.contains("completed"));
        assert_eq!(calls.get(), 2);
        assert_eq!(git(root, &["symbolic-ref", "--short", "HEAD"]), "main");
        assert!(git(root, &["ls-files", "-u"]).is_empty());
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            "resolved 2\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("unrelated.txt")).unwrap(),
            "preserve untracked\n"
        );
        let after = git(root, &["rev-parse", "HEAD"]);
        let inspected = operations::execute(root, Request::Inspect);
        assert_eq!(inspected.view.labels.len(), 1);
        operations::execute(root, Request::MoveTo(0))
            .result
            .unwrap();
        assert_eq!(git(root, &["rev-parse", "HEAD"]), head);
        operations::execute(root, Request::MoveTo(1))
            .result
            .unwrap();
        assert_eq!(git(root, &["rev-parse", "HEAD"]), after);
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            "resolved 2\n"
        );
    }
    #[test]
    fn cancelled_or_invalid_resolution_preserves_paused_rebase() {
        for (task, cancel_during) in [
            (Task::Rebase, false),
            (Task::Rebase, true),
            (Task::Merge, false),
            (Task::Merge, true),
        ] {
            let (dir, head) = fixture();
            let root = dir.path();
            let cancel = AtomicBool::new(false);
            let result = run_with(
                root,
                &Target {
                    task,
                    branch: "main".into(),
                    head: head.clone(),
                    target: "destination".into(),
                },
                &cancel,
                |_| {},
                |_| {
                    cancel.store(cancel_during, Ordering::Release);
                    Ok("<<<<<<< unresolved\n".into())
                },
            );
            assert!(result.is_err());
            assert!(!git(root, &["ls-files", "-u"]).is_empty());
            assert!(
                root.join(if task == Task::Merge {
                    ".git/MERGE_HEAD"
                } else {
                    ".git/rebase-merge"
                })
                .exists()
            );
            assert!(
                std::fs::read_to_string(root.join("file.txt"))
                    .unwrap()
                    .contains("<<<<<<<")
            );
            operations::execute(root, Request::Abort).result.unwrap();
            assert_eq!(git(root, &["rev-parse", "HEAD"]), head);
        }
    }
    #[test]
    fn automatic_merge_resolves_conflicts_and_supports_undo_redo() {
        let (dir, head) = fixture();
        let root = dir.path();
        let incoming = git(root, &["rev-parse", "destination"]);
        run_with(
            root,
            &Target {
                task: Task::Merge,
                branch: "main".into(),
                head: head.clone(),
                target: incoming.clone(),
            },
            &AtomicBool::new(false),
            |_| {},
            |context| {
                assert!(context.contains("Operation: Merge"));
                assert!(
                    context.contains("Ours is the current branch; theirs is the incoming branch.")
                );
                assert!(context.contains("--- MERGE BASE ---\nbase"));
                Ok("merged both intents\n".into())
            },
        )
        .unwrap();
        let after = git(root, &["rev-parse", "HEAD"]);
        assert_eq!(
            git(root, &["rev-list", "--parents", "-n", "1", "HEAD"]),
            format!("{after} {head} {incoming}")
        );
        assert_eq!(git(root, &["symbolic-ref", "--short", "HEAD"]), "main");
        assert!(git(root, &["status", "--porcelain"]).is_empty());
        assert!(!root.join(".git/MERGE_HEAD").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("unrelated.txt")).unwrap(),
            "preserve untracked\n"
        );
        assert_eq!(
            operations::execute(root, Request::Inspect)
                .view
                .labels
                .len(),
            1
        );
        operations::execute(root, Request::MoveTo(0))
            .result
            .unwrap();
        assert_eq!(git(root, &["rev-parse", "HEAD"]), head);
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            "second\n"
        );
        operations::execute(root, Request::MoveTo(1))
            .result
            .unwrap();
        assert_eq!(git(root, &["rev-parse", "HEAD"]), after);
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            "merged both intents\n"
        );
    }
    #[test]
    fn clean_merge_never_requests_ai_resolution() {
        let (dir, _) = fixture();
        let root = dir.path();
        git(root, &["switch", "-c", "clean-target"]);
        std::fs::write(root.join("new.txt"), "clean addition\n").unwrap();
        git(root, &["add", "new.txt"]);
        git(root, &["commit", "-m", "Clean change"]);
        let incoming = git(root, &["rev-parse", "HEAD"]);
        git(root, &["switch", "main"]);
        let head = git(root, &["rev-parse", "HEAD"]);
        let result = run_with(
            root,
            &Target {
                task: Task::Merge,
                branch: "main".into(),
                head,
                target: incoming.clone(),
            },
            &AtomicBool::new(false),
            |_| {},
            |_| panic!("Clean merge should not call AI"),
        )
        .unwrap();
        assert!(result.contains("0 conflict"));
        assert_eq!(git(root, &["rev-parse", "HEAD"]), incoming);
        assert!(git(root, &["status", "--porcelain"]).is_empty());
    }
}
