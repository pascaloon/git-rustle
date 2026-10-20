//! Conservative request-target suggestions from local commit parents. Worker-only.
use crate::git::RefKind;
use crate::remote::Remote;
use gix::ObjectId;
use gix::objs::commit::ref_iter::Token;
use gix::prelude::FindExt;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::Path;

const MAX_SOURCE_PARENTS: usize = 4096;
const MAX_CANDIDATES: usize = 4096;
const MAX_VISITS: usize = 65_536;

/// Git does not retain branch origins in commit objects. Use a unique nearest
/// shared first-parent ancestor; ambiguous, shallow or bounded-out walks fall
/// back to the provider's actual default, never an assumed `main`.
pub fn infer(path: &Path, source: &str, remote: &Remote, default: &str) -> String {
    let fallback = || {
        if source == default {
            String::new()
        } else {
            default.into()
        }
    };
    let Ok(repo) = crate::git::open(path) else {
        return fallback();
    };
    let Ok(refs) = crate::git::snapshot_refs(&repo) else {
        return fallback();
    };
    let prefix = format!("{}/", remote.name);
    let source_id = refs
        .refs
        .iter()
        .find(|r| matches!(r.kind, RefKind::Head | RefKind::Branch) && r.name == source)
        .or_else(|| {
            refs.refs
                .iter()
                .find(|r| r.kind == RefKind::Remote && r.name.strip_prefix(&prefix) == Some(source))
        })
        .map(|r| r.id);
    let Some(source_id) = source_id else {
        return fallback();
    };
    let mut candidates = BTreeMap::<String, Vec<ObjectId>>::new();
    for reference in refs.refs {
        let name = match reference.kind {
            RefKind::Head | RefKind::Branch => reference.name.as_str(),
            RefKind::Remote => {
                let Some(name) = reference.name.strip_prefix(&prefix) else {
                    continue;
                };
                if name == "HEAD" {
                    continue;
                }
                name
            }
            _ => continue,
        };
        if name == source || reference.id == source_id {
            continue;
        }
        let tips = candidates.entry(name.into()).or_default();
        if !tips.contains(&reference.id) {
            tips.push(reference.id);
        }
    }
    if candidates.len() > MAX_CANDIDATES {
        return fallback();
    }
    let mut cached = HashMap::<ObjectId, Option<Vec<ObjectId>>>::new();
    let mut buffer = Vec::new();
    infer_from_parents(source_id, &candidates, default, MAX_VISITS, |id| {
        cached
            .entry(*id)
            .or_insert_with(|| {
                let iter = repo.objects.find_commit_iter(id, &mut buffer).ok()?;
                let mut parents = Vec::new();
                for token in iter {
                    if let Token::Parent { id: parent } = token.ok()? {
                        parents.push(parent);
                    }
                }
                Some(parents)
            })
            .clone()
    })
    .unwrap_or_else(fallback)
}

fn infer_from_parents(
    source: ObjectId,
    candidates: &BTreeMap<String, Vec<ObjectId>>,
    default: &str,
    mut remaining: usize,
    mut parents: impl FnMut(&ObjectId) -> Option<Vec<ObjectId>>,
) -> Option<String> {
    let mut ancestry = HashMap::new();
    let mut current = source;
    for depth in 0..MAX_SOURCE_PARENTS {
        if remaining == 0 {
            return None;
        }
        remaining -= 1;
        if ancestry.insert(current, depth).is_some() {
            return None;
        }
        let next = parents(&current)?;
        let Some(first) = next.first() else { break };
        current = *first;
    }
    let mut best_depth = usize::MAX;
    let mut best = Vec::new();
    for (name, tips) in candidates {
        let mut seen = HashSet::new();
        let mut pending = VecDeque::from(tips.clone());
        let mut nearest = usize::MAX;
        let mut descendant = false;
        while let Some(id) = pending.pop_front() {
            if !seen.insert(id) {
                continue;
            }
            if remaining == 0 {
                return None;
            }
            remaining -= 1;
            if let Some(&depth) = ancestry.get(&id) {
                if depth == 0 {
                    descendant = true;
                    break;
                }
                nearest = nearest.min(depth);
                // Older ancestors through this path cannot be a closer fork.
                continue;
            }
            pending.extend(parents(&id)?);
        }
        if descendant || nearest == usize::MAX {
            continue;
        }
        if nearest < best_depth {
            best_depth = nearest;
            best.clear();
        }
        if nearest == best_depth {
            best.push(name.clone());
        }
    }
    if best.len() == 1 {
        return best.pop();
    }
    best.into_iter().find(|name| name == default)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository(extra_branches: usize) -> (tempfile::TempDir, Remote) {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            String::from_utf8(crate::git::command::checked(dir.path(), args).unwrap())
                .unwrap()
                .trim()
                .to_string()
        };
        git(&["init", "-b", "main"]);
        let tree = git(&["write-tree"]);
        let commit = |message: &str, parent: Option<&str>| {
            let mut args = vec![
                "-c",
                "user.name=Target fixture",
                "-c",
                "user.email=target@example.invalid",
                "commit-tree",
                tree.as_str(),
                "-m",
                message,
            ];
            if let Some(parent) = parent {
                args.extend(["-p", parent]);
            }
            git(&args)
        };
        let main = commit("root", None);
        let fork = commit("develop base", Some(&main));
        let feature = commit("feature", Some(&fork));
        let develop = commit("develop advanced", Some(&fork));
        let mut commands = format!(
            "update refs/heads/main {main}\nupdate refs/heads/develop {develop}\nupdate refs/heads/feature {feature}\n"
        );
        for n in 0..extra_branches {
            commands.push_str(&format!("update refs/heads/old-{n} {main}\n"));
        }
        let output = crate::git::command::run(
            dir.path(),
            &["update-ref", "--stdin"],
            Some(commands.as_bytes()),
            std::time::Duration::from_secs(10),
        )
        .unwrap();
        assert!(output.status.success());
        git(&["symbolic-ref", "HEAD", "refs/heads/feature"]);
        let remote = Remote::parse(
            "origin",
            "https://gitlab.example/team/project.git",
            Some("gitlab"),
        )
        .unwrap();
        (dir, remote)
    }

    fn id(n: usize) -> ObjectId {
        ObjectId::from_hex(format!("{n:040x}").as_bytes()).unwrap()
    }
    fn graph() -> HashMap<ObjectId, Vec<ObjectId>> {
        [
            (1, vec![]),
            (2, vec![1]),
            (3, vec![2]),
            (4, vec![2]),
            (5, vec![3]),
            (6, vec![2]),
            (7, vec![4, 2]),
        ]
        .into_iter()
        .map(|(n, p)| (id(n), p.into_iter().map(id).collect()))
        .collect()
    }
    fn infer(source: usize, branches: &[(&str, usize)], default: &str) -> Option<String> {
        let graph = graph();
        let branches = branches
            .iter()
            .map(|(n, i)| (n.to_string(), vec![id(*i)]))
            .collect();
        infer_from_parents(id(source), &branches, default, MAX_VISITS, |id| {
            graph.get(id).cloned()
        })
    }

    #[test]
    fn stacked_and_advanced_parent_branches_beat_the_provider_default() {
        assert_eq!(
            infer(3, &[("main", 1), ("develop", 2)], "main").as_deref(),
            Some("develop")
        );
        assert_eq!(
            infer(3, &[("main", 1), ("develop", 4)], "main").as_deref(),
            Some("develop")
        );
        assert_eq!(
            infer(7, &[("main", 1), ("release", 4)], "main").as_deref(),
            Some("release")
        );
    }

    #[test]
    fn descendants_and_ambiguous_siblings_do_not_invent_a_parent() {
        assert_eq!(
            infer(3, &[("develop", 2), ("child", 5)], "main").as_deref(),
            Some("develop")
        );
        assert_eq!(
            infer(3, &[("develop", 4), ("sibling", 6), ("main", 1)], "main"),
            None
        );
        assert_eq!(
            infer(3, &[("develop", 4), ("sibling", 6)], "develop").as_deref(),
            Some("develop")
        );
    }

    #[test]
    fn missing_objects_and_visit_limits_leave_target_to_provider_fallback() {
        let branches = [("develop".into(), vec![id(99)])].into_iter().collect();
        let graph = graph();
        assert!(
            infer_from_parents(id(3), &branches, "main", MAX_VISITS, |id| graph
                .get(id)
                .cloned())
            .is_none()
        );
        assert!(
            infer_from_parents(id(3), &branches, "main", 1, |id| graph.get(id).cloned()).is_none()
        );
    }

    #[test]
    fn repository_snapshot_finds_non_default_parent_and_fallback_without_mutation() {
        let (dir, remote) = repository(0);
        let before = crate::git::snapshot_refs(&crate::git::open(dir.path()).unwrap()).unwrap();
        assert_eq!(
            super::infer(dir.path(), "feature", &remote, "main"),
            "develop"
        );
        assert_eq!(super::infer(dir.path(), "missing", &remote, "main"), "main");
        assert!(super::infer(dir.path(), "main", &remote, "main").is_empty());
        let after = crate::git::snapshot_refs(&crate::git::open(dir.path()).unwrap()).unwrap();
        assert_eq!(before.fingerprint, after.fingerprint);
    }

    #[test]
    #[ignore = "bounded MR target inference with 1000 named candidate branches"]
    fn benchmark_target_inference() {
        let (repository, remote) = repository(1000);
        let mut graph = graph();
        let mut branches = BTreeMap::new();
        branches.insert("develop".into(), vec![id(4)]);
        for n in 100..1100 {
            graph.insert(id(n), vec![id(1)]);
            branches.insert(format!("branch-{n}"), vec![id(n)]);
        }
        for sample in -1..15 {
            let started = std::time::Instant::now();
            let target = infer_from_parents(id(3), &branches, "main", MAX_VISITS, |id| {
                graph.get(id).cloned()
            });
            let parent_walk_ms = started.elapsed().as_secs_f64() * 1000.;
            assert_eq!(target.as_deref(), Some("develop"));
            let started = std::time::Instant::now();
            let target = super::infer(repository.path(), "feature", &remote, "main");
            let inference_ms = started.elapsed().as_secs_f64() * 1000.;
            assert_eq!(target, "develop");
            if sample >= 0 {
                println!(
                    "{}",
                    serde_json::json!({"sample":sample,"parent_walk_ms":parent_walk_ms,"inference_ms":inference_ms})
                );
            }
        }
    }
}
