//! Fetched/local branch choices and bounded, worker-only target ordering.
use crate::remote::Remote;
use gix::{ObjectId, objs::commit::ref_iter::Token, prelude::FindExt};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::Path,
};

const MAX_VISITS: usize = 65_536;
const MAX_SOURCE_PARENTS: usize = 4096;

#[derive(Clone, Default)]
pub struct Choices {
    pub sources: Vec<String>,
    pub targets: Vec<String>,
    pub incomplete: bool,
}

/// Prefer the selected project's fetched tip when its local namesake differs.
/// Other remotes, tags and symbolic aliases are never request branches.
pub fn load(
    path: &Path,
    source: &str,
    remote: Option<&Remote>,
    default: &str,
) -> Result<Choices, String> {
    let repo = crate::git::open(path).map_err(|e| e.to_string())?;
    let refs = crate::git::refs::raw_refs(&repo)?;
    let mut branches = BTreeMap::new();
    for r in &refs {
        if r.symbolic.is_none()
            && let (Some(name), Some(id)) = (r.name.strip_prefix("refs/heads/"), r.id)
        {
            branches.insert(name.to_owned(), id);
        }
    }
    let mut symbolic_default = None;
    if let Some(remote) = remote {
        let prefix = format!("refs/remotes/{}/", remote.name);
        for r in &refs {
            if let Some(name) = r.name.strip_prefix(&prefix) {
                if name == "HEAD" {
                    symbolic_default = r.symbolic.as_deref().and_then(|n| n.strip_prefix(&prefix));
                } else if r.symbolic.is_none()
                    && let Some(id) = r.id
                {
                    branches.insert(name.to_owned(), id);
                }
            }
        }
    }
    let default = if default.is_empty() {
        symbolic_default.unwrap_or("")
    } else {
        default
    };
    let mut cache = HashMap::<ObjectId, Option<Vec<ObjectId>>>::new();
    let mut buffer = Vec::new();
    Ok(order(&branches, source, default, MAX_VISITS, |id| {
        cache
            .entry(*id)
            .or_insert_with(|| {
                let iter = repo.objects.find_commit_iter(id, &mut buffer).ok()?;
                let mut parents = Vec::new();
                for token in iter {
                    if let Token::Parent { id } = token.ok()? {
                        parents.push(id);
                    }
                }
                Some(parents)
            })
            .clone()
    }))
}

fn order(
    branches: &BTreeMap<String, ObjectId>,
    source: &str,
    default: &str,
    mut remaining: usize,
    mut parents: impl FnMut(&ObjectId) -> Option<Vec<ObjectId>>,
) -> Choices {
    let mut choices = Choices {
        sources: branches.keys().cloned().collect(),
        ..Choices::default()
    };
    let mut ancestry = HashMap::new();
    if let Some(mut current) = branches.get(source).copied() {
        for depth in 0..MAX_SOURCE_PARENTS {
            if remaining == 0 || ancestry.contains_key(&current) {
                choices.incomplete = true;
                break;
            }
            remaining -= 1;
            ancestry.insert(current, depth);
            let Some(next) = parents(&current) else {
                choices.incomplete = true;
                break;
            };
            let Some(first) = next.first() else {
                break;
            };
            current = *first;
            if depth == MAX_SOURCE_PARENTS - 1 {
                choices.incomplete = true;
            }
        }
    }
    // Multiple names at the same tip share both the ancestry walk and score.
    let mut scores = HashMap::new();
    let mut ranked = Vec::new();
    for (name, tip) in branches {
        if name == source {
            continue;
        }
        let score = *scores.entry(*tip).or_insert_with(|| {
            let mut seen = HashSet::new();
            let mut pending = VecDeque::from([(*tip, 0usize)]);
            let mut nearest = (usize::MAX, usize::MAX);
            let mut incomplete = false;
            let mut descendant = false;
            while let Some((id, distance)) = pending.pop_front() {
                if !seen.insert(id) {
                    continue;
                }
                if remaining == 0 {
                    incomplete = true;
                    break;
                }
                remaining -= 1;
                if let Some(&depth) = ancestry.get(&id) {
                    if depth == 0 {
                        descendant = true;
                        break;
                    }
                    nearest = nearest.min((depth, distance));
                    continue;
                }
                if let Some(next) = parents(&id) {
                    pending.extend(next.into_iter().map(|p| (p, distance + 1)));
                } else {
                    incomplete = true;
                }
            }
            choices.incomplete |= incomplete;
            // Known bases, then descendants, then unknown/unrelated history.
            if incomplete {
                (2, usize::MAX, usize::MAX)
            } else if descendant {
                (1, usize::MAX, usize::MAX)
            } else if nearest.0 != usize::MAX {
                (0, nearest.0, nearest.1)
            } else {
                (2, usize::MAX, usize::MAX)
            }
        });
        ranked.push((score.0, score.1, score.2, name != default, name.clone()));
    }
    ranked.sort();
    choices.targets = ranked.into_iter().map(|(_, _, _, _, name)| name).collect();
    choices
}

#[cfg(test)]
mod tests {
    use super::super::target;
    use super::*;
    fn id(n: usize) -> ObjectId {
        ObjectId::from_hex(format!("{n:040x}").as_bytes()).unwrap()
    }
    #[test]
    fn closest_base_then_distance_default_and_name() {
        let graph: HashMap<_, _> = [
            (1, vec![]),
            (2, vec![1]),
            (3, vec![2]),
            (4, vec![2]),
            (5, vec![3]),
        ]
        .into_iter()
        .map(|(n, p)| (id(n), p.into_iter().map(id).collect::<Vec<_>>()))
        .collect();
        let branches = [
            ("Main", 1),
            ("Dev", 2),
            ("FeatureA", 3),
            ("Sibling", 4),
            ("Child", 5),
        ]
        .into_iter()
        .map(|(n, i)| (n.to_string(), id(i)))
        .collect();
        let choices = order(&branches, "FeatureA", "Main", MAX_VISITS, |id| {
            graph.get(id).cloned()
        });
        assert_eq!(choices.targets, ["Dev", "Sibling", "Main", "Child"]);
        assert!(!choices.incomplete);
        let advanced = [
            ("Main".to_string(), id(1)),
            ("Dev".into(), id(4)),
            ("FeatureA".into(), id(3)),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            order(&advanced, "FeatureA", "Main", MAX_VISITS, |id| graph
                .get(id)
                .cloned())
            .targets,
            ["Dev", "Main"]
        );
        let changed = order(&branches, "Dev", "Main", MAX_VISITS, |id| {
            graph.get(id).cloned()
        });
        assert_eq!(changed.targets[0], "Main");
        let aliases = [
            ("Zeta".to_string(), id(2)),
            ("Dev".into(), id(2)),
            ("FeatureA".into(), id(3)),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            order(&aliases, "FeatureA", "Zeta", MAX_VISITS, |id| graph
                .get(id)
                .cloned())
            .targets,
            ["Zeta", "Dev"]
        );
    }
    #[test]
    fn feature_prefers_dev_when_both_dev_and_main_have_advanced_after_the_forks() {
        // Main: 1--2--3--4; Dev: 2--5--6--7; Feature: 6--8--9.
        let graph: HashMap<_, _> = [
            (1, vec![]),
            (2, vec![1]),
            (3, vec![2]),
            (4, vec![3]),
            (5, vec![2]),
            (6, vec![5]),
            (7, vec![6]),
            (8, vec![6]),
            (9, vec![8]),
        ]
        .into_iter()
        .map(|(n, p)| (id(n), p.into_iter().map(id).collect::<Vec<_>>()))
        .collect();
        let branches = [
            ("Main".to_string(), id(4)),
            ("Dev".into(), id(7)),
            ("Feature".into(), id(9)),
        ]
        .into_iter()
        .collect();
        let choices = order(&branches, "Feature", "Main", MAX_VISITS, |id| {
            graph.get(id).cloned()
        });
        assert_eq!(choices.targets, ["Dev", "Main"]);
        assert!(!choices.incomplete);
    }

    #[test]
    fn partial_history_and_budget_keep_every_choice_except_source() {
        let branches = [
            ("FeatureA".into(), id(3)),
            ("Dev".into(), id(2)),
            ("Main".into(), id(1)),
        ]
        .into_iter()
        .collect();
        let choices = order(&branches, "FeatureA", "Main", 0, |_| None);
        assert!(choices.incomplete);
        assert_eq!(choices.targets, ["Main", "Dev"]);
        assert_eq!(choices.sources, ["Dev", "FeatureA", "Main"]);
    }
    #[test]
    fn fetched_tips_and_remote_names_are_project_scoped() {
        let (dir, remote) = target::tests::repository(0);
        let root = dir.path();
        for args in [
            vec![
                "update-ref",
                "refs/remotes/origin/develop",
                "refs/heads/develop",
            ],
            vec![
                "update-ref",
                "refs/remotes/origin/remote-only",
                "refs/heads/main",
            ],
            vec![
                "update-ref",
                "refs/remotes/other/wrong-project",
                "refs/heads/main",
            ],
            vec![
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/develop",
            ],
            vec!["update-ref", "refs/heads/develop", "refs/heads/main"],
        ] {
            crate::git::command::checked(root, &args).unwrap();
        }
        let result = load(root, "feature", Some(&remote), "main").unwrap();
        assert_eq!(result.targets[0], "develop");
        assert!(result.sources.contains(&"remote-only".into()));
        assert!(!result.sources.contains(&"HEAD".into()));
        assert!(!result.sources.contains(&"wrong-project".into()));
    }
    #[test]
    #[ignore = "one warmup and 15 worker branch ordering samples with 1000 names"]
    fn benchmark_branch_ordering() {
        let (dir, remote) = target::tests::repository(1000);
        for sample in -1..15 {
            let start = std::time::Instant::now();
            let result = load(dir.path(), "feature", Some(&remote), "main").unwrap();
            let ms = start.elapsed().as_secs_f64() * 1000.;
            assert_eq!(result.targets[0], "develop");
            if sample >= 0 {
                println!(
                    "{}",
                    serde_json::json!({"sample":sample,"branch_order_ms":ms,"sources":result.sources.len()})
                );
            }
        }
    }
}
