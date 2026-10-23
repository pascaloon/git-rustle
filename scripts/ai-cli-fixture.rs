//! Deterministic external CLI fixture. Never contacts an AI provider.
use std::{io::{Read, Write}, path::PathBuf};
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let codex = args.first().is_some_and(|s| s == "exec");
    assert!(args.windows(2).any(|a| a == ["--model", "fixture-model"]));
    if codex {
        assert!(args.windows(2).any(|a| a == ["--sandbox", "read-only"]));
        assert!(args.iter().any(|a| a == "--ephemeral"));
    } else {
        assert!(args.iter().any(|a| a == "--print"));
        assert!(args.windows(2).any(|a| a[0] == "--tools" && a[1].is_empty()));
        assert!(args.iter().any(|a| a == "--strict-mcp-config"));
    }
    let mut input = String::new(); std::io::stdin().read_to_string(&mut input).unwrap();
    assert!(input.contains("<git_context>"));
    let log = PathBuf::from(std::env::var_os("GIT_RUSTLE_AI_FIXTURE_LOG").expect("fixture logging required"));
    std::fs::write(&log, &input).unwrap();
    let name = std::env::var("GIT_RUSTLE_SCENARIO").unwrap_or_default();
    if matches!(name.as_str(), "ai-error" | "ai-rebase-error" | "ai-merge-error") { eprintln!("Sign in first (fixture diagnostic)"); std::process::exit(1); }
    if matches!(name.as_str(), "ai-stale" | "ai-cancel" | "ai-rebase-cancel" | "ai-merge-cancel") { std::thread::sleep(std::time::Duration::from_secs(2)); }
    let response: String = if input.contains("complete resolved file") {
        if name == "ai-conflict-rebase" { assert!(input.contains("Operation: Rebase")); }
        assert!(input.contains("--- MERGE BASE ---"));
        if name.starts_with("ai-merge") {
            assert!(input.contains("Operation: Merge"));
            assert!(input.contains("Ours is the current branch; theirs is the incoming branch."));
            assert!(input.contains("Git Rustle will save, stage and continue automatically"));
            r#"{"resolved":true,"content":"resolved AI merge fixture\n","reason":null}"#.into()
        } else if name.starts_with("ai-rebase") {
            assert!(input.contains("Git Rustle will save, stage and continue automatically"));
            if input.contains("local second") { r#"{"resolved":true,"content":"resolved AI fixture second\n","reason":null}"#.into() }
            else { r#"{"resolved":true,"content":"resolved AI fixture first\n","reason":null}"#.into() }
        } else { "resolved AI fixture\n".into() }
    } else if input.contains("Return only a Git commit message") {
        assert!(input.contains("File: README.md")); assert!(!input.contains("File: notes.txt"));
        "Describe selected documentation\n\nExplain the pending README update.".into()
    } else {
        assert!(input.contains("Scope: Entire diff (2 files)"));
        assert!(input.contains("File: README.md"));
        assert!(input.contains("Pending documentation update"));
        assert!(input.contains("File: notes.txt"));
        assert!(input.contains("Untracked fixture notes"));
        assert!(input.len() > 256 * 1024, "Large summaries must reach the CLI");
        assert!(input.contains("@@ -"));
        assert!(!input.contains("--- BEFORE ---"));
        assert!(input.contains("+Expanded AI summary fixture line 9999 with relevant changes"));
        "Summary fixture\n\n- Update the documentation and add supporting notes.\n- Summarize the complete comparison across both files.".into()
    };
    if codex {
        let output = args.windows(2).find(|a| a[0] == "--output-last-message").unwrap();
        std::fs::write(&output[1], &response).unwrap();
        println!("Progress output must never become the final answer");
    } else { std::io::stdout().write_all(response.as_bytes()).unwrap(); }
}
