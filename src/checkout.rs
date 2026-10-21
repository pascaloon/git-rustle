//! Frozen checkout choices shared by the graph and branch browser.
use crate::branches::Target;
use crate::remote::Remote;
use gix::ObjectId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Branch(Target),
    /// A remote-only graph tip: fetch its branch and fast-forward its tracker.
    FetchBranch(Target),
    Commit {
        id: ObjectId,
        remote: Option<Remote>,
    },
}

impl Request {
    pub fn label(&self) -> String {
        match self {
            Self::Branch(t) | Self::FetchBranch(t) => t.label(),
            Self::Commit { id, .. } => format!("{} (detached HEAD)", id.to_hex_with_len(12)),
        }
    }

    pub fn is_remote(&self) -> bool {
        match self {
            Self::Branch(t) | Self::FetchBranch(t) => t.remote.is_some(),
            Self::Commit { remote, .. } => remote.is_some(),
        }
    }
}

/// Resolve the logical branch choices at a graph commit. A configured upstream
/// is an alias only when its exact identity and tip match the local branch.
pub fn branch_choices(
    mut targets: Vec<Target>,
    refs: Option<&crate::git::RefSnapshot>,
) -> Vec<Target> {
    targets.sort_by_key(|t| (t.remote.is_some(), t.label()));
    targets.dedup_by(|a, b| a.name == b.name && a.remote == b.remote && a.sha == b.sha);
    use std::collections::{BTreeMap, HashMap, HashSet};
    let upstreams = refs.map(|r| &r.upstreams);
    let mut remote_names = HashMap::<(String, String), (usize, String)>::new();
    for target in &targets {
        if let Some(remote) = &target.remote {
            let entry = remote_names
                .entry((target.name.clone(), target.sha.clone()))
                .or_insert((0, remote.clone()));
            entry.0 += 1;
        }
    }
    let mut aliases = BTreeMap::<String, BTreeMap<String, HashSet<String>>>::new();
    for local in targets.iter().filter(|target| target.remote.is_none()) {
        let alias = match upstreams.and_then(|map| map.get(&local.name)) {
            Some((remote, branch)) => Some((remote.as_str(), branch.as_str())),
            None => remote_names
                .get(&(local.name.clone(), local.sha.clone()))
                .filter(|entry| entry.0 == 1)
                .map(|entry| (entry.1.as_str(), local.name.as_str())),
        };
        if let Some((remote, branch)) = alias {
            aliases
                .entry(remote.into())
                .or_default()
                .entry(branch.into())
                .or_default()
                .insert(local.sha.clone());
        }
    }
    targets.retain(|target| {
        target.remote.as_ref().is_none_or(|remote| {
            !aliases
                .get(remote)
                .and_then(|branches| branches.get(&target.name))
                .is_some_and(|shas| shas.contains(&target.sha))
        })
    });
    targets
}

pub struct Confirmation {
    pub choices: Vec<Request>,
    pub selected: usize,
    /// Enter only confirms after a readable popup has actually been drawn.
    pub ready: bool,
}

impl Confirmation {
    pub fn new(choices: Vec<Request>) -> Self {
        debug_assert!(!choices.is_empty());
        Self {
            choices,
            selected: 0,
            ready: false,
        }
    }

    pub fn chosen(&self) -> &Request {
        &self.choices[self.selected]
    }

    pub fn move_by(&mut self, delta: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.choices.len() - 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target(name: &str, remote: Option<&str>) -> Target {
        Target {
            name: name.into(),
            remote: remote.map(str::to_owned),
            url: None,
            sha: "abc".into(),
        }
    }
    #[test]
    fn exact_upstream_aliases_collapse_but_other_branches_and_remotes_remain() {
        let mut refs = crate::git::RefSnapshot::default();
        refs.upstreams
            .insert("local".into(), ("origin".into(), "remote-name".into()));
        let choices = branch_choices(
            vec![
                target("local", None),
                target("remote-name", Some("origin")),
                target("remote-name", Some("other")),
                target("different", None),
            ],
            Some(&refs),
        );
        assert_eq!(choices.len(), 3);
        assert!(
            !choices
                .iter()
                .any(|t| t.remote.as_deref() == Some("origin"))
        );
        assert!(choices.iter().any(|t| t.remote.as_deref() == Some("other")));
        assert_eq!(
            branch_choices(
                vec![target("foo", None), target("foo", Some("origin"))],
                None
            )
            .len(),
            1
        );
        assert_eq!(
            branch_choices(
                vec![
                    target("foo", None),
                    target("foo", Some("origin")),
                    target("foo", Some("other"))
                ],
                None
            )
            .len(),
            3
        );
    }

    #[test]
    #[ignore = "one warmup plus15 logical checkout choices samples at1k/10k branches"]
    fn benchmark_branch_choices() {
        for count in [1000, 10000] {
            let mut refs = crate::git::RefSnapshot::default();
            let mut targets = Vec::new();
            for index in 0..count {
                let name = format!("topic/{index}");
                refs.upstreams
                    .insert(name.clone(), ("origin".into(), name.clone()));
                targets.push(target(&name, None));
                targets.push(target(&name, Some("origin")));
            }
            for sample in -1..15 {
                let start = std::time::Instant::now();
                let choices = branch_choices(targets.clone(), Some(&refs));
                assert_eq!(choices.len(), count);
                println!(
                    "{}",
                    serde_json::json!({"sample":sample,"branches":count,"choicesMs":start.elapsed().as_secs_f64()*1000.})
                );
            }
        }
    }
    #[test]
    #[ignore = "writes a disposable native checkout benchmark fixture"]
    fn prepare_native_fixture() {
        let path = std::path::PathBuf::from(
            std::env::var_os("GIT_RUSTLE_CHECKOUT_FIXTURE").expect("fixture path required"),
        );
        assert!(!path.exists(), "fixture must be fresh");
        std::fs::create_dir_all(&path).unwrap();
        let git = |args: &[&str]| crate::git::command::checked(&path, args).unwrap();
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(
            path.join(".git/rustle-checkout-benchmark"),
            "git-rustle checkout benchmark fixture v1",
        )
        .unwrap();
        git(&["config", "core.hooksPath", "disabled-test-hooks"]);
        for n in 0..3 {
            std::fs::write(path.join("file.txt"), format!("version {n}\n")).unwrap();
            git(&["add", "file.txt"]);
            git(&[
                "-c",
                "user.name=Checkout Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgSign=false",
                "commit",
                "-qm",
                &format!("Commit {n}"),
            ]);
        }
        git(&["branch", "topic", "HEAD~1"]);
        println!("Native checkout fixture: {}", path.display());
    }
}
