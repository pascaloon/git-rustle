//! Read-only branch-base lookup. All reference/object access belongs on workers.
use super::{ObjectId, Path, RefKind, command};

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

pub fn prepare(root: &Path, selected: ObjectId) -> Result<Prepared, String> {
    let repo = super::open(root).map_err(|e| e.to_string())?;
    let refs = super::snapshot_refs(&repo).map_err(|e| e.to_string())?;
    let mut targets: Vec<_> = refs
        .refs
        .into_iter()
        .filter(|r| {
            matches!(r.kind, RefKind::Branch | RefKind::Head | RefKind::Remote)
                && r.name != "HEAD"
                && !r.name.ends_with("/HEAD")
        })
        .map(|r| Target {
            name: r.name,
            id: r.id,
        })
        .collect();
    targets.sort_by(|a, b| a.name.cmp(&b.name));
    targets.dedup_by(|a, b| a.name == b.name && a.id == b.id);
    let symbolic = command::run(
        root,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
        None,
        std::time::Duration::from_secs(10),
    )?;
    if symbolic.status.success() {
        let name = String::from_utf8_lossy(&symbolic.stdout)
            .trim()
            .trim_start_matches("refs/remotes/")
            .to_owned();
        if let Some(target) = targets.iter().find(|t| t.name == name) {
            return Ok(Prepared::Default(target.clone()));
        }
    }
    for name in ["main", "master", "origin/main", "origin/master"] {
        if let Some(target) = targets.iter().find(|t| t.name == name) {
            return Ok(Prepared::Default(target.clone()));
        }
    }
    targets.retain(|target| target.id != selected);
    targets.dedup_by(|a, b| a.id == b.id);
    match targets.len() {
        0 => Err("No base branch is available. Fetch or create a base branch first.".into()),
        1 => Ok(Prepared::Default(targets.remove(0))),
        _ => Ok(Prepared::Choose(targets)),
    }
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
