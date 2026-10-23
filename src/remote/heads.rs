//! Bounded, paginated tip verification for graphs with many tracking refs.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub(super) const BULK_THRESHOLD: usize = 64;

/// A ref advertisement transfers names and object IDs, never objects or local
/// refs. Use Git's configured transport/credentials (including SSH and insteadOf).
/// API-only installations fall back to the provider; don't repeat a failed
/// transport probe on every click while its credentials/network are unavailable.
pub(super) fn advertise(
    path: &Path,
    remote: &Remote,
    names: &HashSet<String>,
    cancel: &AtomicBool,
) -> Option<Vec<Branch>> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static FAILED: OnceLock<Mutex<HashMap<(PathBuf, String), Instant>>> = OnceLock::new();
    let failed = FAILED.get_or_init(Default::default);
    let key = (path.to_path_buf(), remote.url.clone());
    {
        let mut failures = failed.lock().unwrap_or_else(|e| e.into_inner());
        failures.retain(|_, time| time.elapsed() < Duration::from_secs(60));
        if failures.contains_key(&key) || cancel.load(Ordering::Relaxed) {
            return None;
        }
    }
    let result = command::run(
        path,
        &["ls-remote", "--heads", "--refs", "--", &remote.url],
        None,
        Duration::from_secs(3),
    )
    .ok()
    .filter(|out| out.status.success())
    .and_then(|out| parse_advertisement(&out.stdout, names).ok());
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    if result.is_none() {
        failed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, Instant::now());
    }
    result
}

fn parse_advertisement(bytes: &[u8], names: &HashSet<String>) -> Result<Vec<Branch>, ()> {
    let text = std::str::from_utf8(bytes).map_err(|_| ())?;
    let mut found = BTreeMap::new();
    for line in text.lines() {
        let (sha, reference) = line.split_once('\t').ok_or(())?;
        let name = reference.strip_prefix("refs/heads/").ok_or(())?;
        // Reject an incomplete/malformed response as a whole: it must never
        // establish deletion of the cached identities missing from that response.
        gix::ObjectId::from_hex(sha.as_bytes()).map_err(|_| ())?;
        if name.is_empty() || name.chars().any(char::is_control) {
            return Err(());
        }
        if names.contains(name) && found.insert(name.to_string(), sha.to_string()).is_some() {
            return Err(());
        }
    }
    Ok(found
        .into_iter()
        .map(|(name, sha)| Branch { name, sha })
        .collect())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn load(
    path: &Path,
    remote: &Remote,
    names: &HashSet<String>,
    token: &mut Option<String>,
    cancel: &AtomicBool,
    found: &mut Vec<Branch>,
    verified: &mut HashSet<String>,
    emit: &(impl Fn(Event) + Sync),
) -> Result<(), String> {
    let started = Instant::now();
    let http = Http::new();
    // Resolve authentication once, before parallel requests. A failed listing
    // cannot establish deletions, including a private repository's 404.
    let mut first = http.get(remote, 1, None, token.as_deref())?;
    if matches!(first.status, 401 | 403 | 404) && token.is_none() {
        *token = helper_token(path, remote);
        if token.is_some() {
            first = http.get(remote, 1, None, token.as_deref())?;
        }
    }
    let first = parse_page(remote.provider, first, None)?;
    let complete = scan(
        first,
        names,
        cancel,
        |page| {
            parse_page(
                remote.provider,
                http.get(remote, page, None, token.as_deref())?,
                None,
            )
        },
        |batch, scope, complete| {
            *found = batch;
            *verified = scope;
            emit(Event::HeadsVerified {
                update: Update {
                    remote: remote.clone(),
                    branches: found.clone(),
                    scoped: true,
                    cached: !complete,
                    stale: !complete,
                },
                scope: verified.clone(),
            });
        },
    )?;
    if complete {
        return Ok(());
    }
    // A remote can have vastly more branches than the graph scope. Limit the
    // page probe, then use exact endpoints for remaining identities instead of
    // scanning an unbounded unrelated tail.
    let mut remaining: Vec<_> = names.difference(verified).cloned().collect();
    remaining.sort();
    for batch in remaining.chunks(4) {
        if cancel.load(Ordering::Relaxed) {
            return Err("Remote head check cancelled".into());
        }
        if started.elapsed() >= Duration::from_secs(30) {
            return Err(
                "Remote head verification time budget reached; cached heads retained".into(),
            );
        }
        let results = std::thread::scope(|scope| {
            let token = &*token;
            let http = &http;
            let workers: Vec<_> = batch
                .iter()
                .map(|name| {
                    scope.spawn(move || {
                        let mut token = token.clone();
                        let result = read_head(path, remote, name, http, &mut token);
                        (name.clone(), result, token)
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| {
                    worker.join().unwrap_or_else(|_| {
                        (String::new(), Err("Remote head worker failed".into()), None)
                    })
                })
                .collect::<Vec<_>>()
        });
        if cancel.load(Ordering::Relaxed) {
            return Err("Remote head check cancelled".into());
        }
        let mut failure = None;
        for (name, result, resolved_token) in results {
            if resolved_token.is_some() {
                *token = resolved_token;
            }
            match result {
                Ok(branch) => {
                    verified.insert(name);
                    found.extend(branch);
                }
                Err(error) => {
                    failure.get_or_insert(error);
                }
            }
        }
        emit(Event::HeadsVerified {
            update: Update {
                remote: remote.clone(),
                branches: found.clone(),
                scoped: true,
                cached: true,
                stale: true,
            },
            scope: verified.clone(),
        });
        if let Some(error) = failure {
            return Err(error);
        }
    }
    Ok(())
}

fn scan(
    first: Page,
    names: &HashSet<String>,
    cancel: &AtomicBool,
    get: impl Fn(usize) -> Result<Page, String> + Sync,
    mut publish: impl FnMut(Vec<Branch>, HashSet<String>, bool),
) -> Result<bool, String> {
    let started = Instant::now();
    let mut found = BTreeMap::new();
    let mut batch = vec![Ok(first)];
    let mut next = 2;
    let page_limit = (names.len().div_ceil(100) * 2).clamp(4, MAX_PAGES);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("Remote head check cancelled".into());
        }
        let mut complete = false;
        let mut failure = None;
        for result in batch {
            match result {
                Ok(page) => {
                    for branch in page.branches {
                        if names.contains(&branch.name) {
                            found.insert(branch.name.clone(), branch);
                        }
                    }
                    if !page.more {
                        complete = failure.is_none();
                        break;
                    }
                }
                Err(error) => {
                    failure.get_or_insert(error);
                }
            }
        }
        // Only a complete enumeration proves that an omitted branch was deleted.
        // On failure publish successes, retaining every unchecked cached head.
        let scope = if complete {
            names.clone()
        } else {
            found.keys().cloned().collect()
        };
        let all_found = found.len() == names.len();
        publish(
            found.values().cloned().collect(),
            scope,
            complete || all_found,
        );
        if let Some(error) = failure {
            return Err(error);
        }
        if complete || all_found {
            return Ok(true);
        }
        if started.elapsed() >= Duration::from_secs(30) || next > MAX_PAGES {
            return Err(
                "Remote head verification time budget reached; cached heads retained".into(),
            );
        }
        if next > page_limit {
            return Ok(false);
        }
        // Ordered batches avoid mistaking an out-of-order empty page for a
        // complete listing. At most four HTTP requests are in flight.
        let end = (next + 4).min(page_limit + 1);
        batch = std::thread::scope(|scope| {
            let workers: Vec<_> = (next..end)
                .map(|page| {
                    let get = &get;
                    scope.spawn(move || {
                        if cancel.load(Ordering::Relaxed) {
                            Err("Remote head check cancelled".into())
                        } else {
                            get(page)
                        }
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| {
                    worker
                        .join()
                        .unwrap_or_else(|_| Err("Remote head worker failed".into()))
                })
                .collect()
        });
        next = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn advertisement_is_scoped_and_rejects_incomplete_or_ambiguous_output() {
        let names = ["topic/slash".into(), "deleted".into()]
            .into_iter()
            .collect();
        let valid = format!(
            "{}\trefs/heads/topic/slash\n{}\trefs/heads/unrelated\n",
            "a".repeat(40),
            "b".repeat(40)
        );
        let result = parse_advertisement(valid.as_bytes(), &names).unwrap();
        assert_eq!(
            result,
            vec![Branch {
                name: "topic/slash".into(),
                sha: "a".repeat(40)
            }]
        );
        for invalid in [
            format!("{valid}truncated"),
            format!("{valid}{valid}"),
            format!("{}\trefs/tags/tag\n", "a".repeat(40)),
            "xyz\trefs/heads/topic/slash\n".into(),
        ] {
            assert!(parse_advertisement(invalid.as_bytes(), &names).is_err());
        }
        assert!(parse_advertisement(&[255], &names).is_err());
        assert!(parse_advertisement(b"", &names).unwrap().is_empty());
    }

    #[test]
    fn git_advertisement_detects_changes_and_deletions_without_fetching() {
        let source = tempfile::tempdir().unwrap();
        let client = tempfile::tempdir().unwrap();
        for path in [source.path(), client.path()] {
            command::checked(path, &["init", "-q"]).unwrap();
        }
        let commit = || {
            command::checked(
                source.path(),
                &[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "--allow-empty",
                    "-qm",
                    "fixture",
                ],
            )
            .unwrap();
            String::from_utf8(command::checked(source.path(), &["rev-parse", "HEAD"]).unwrap())
                .unwrap()
                .trim()
                .to_string()
        };
        let first = commit();
        command::checked(
            source.path(),
            &["update-ref", "refs/heads/topic/slash", &first],
        )
        .unwrap();
        command::checked(source.path(), &["update-ref", "refs/heads/deleted", &first]).unwrap();
        let mut remote = Remote::parse(
            "origin",
            "https://example.invalid/team/repo",
            Some("github"),
        )
        .unwrap();
        remote.url = source.path().to_string_lossy().into_owned();
        let names = ["topic/slash".into(), "deleted".into()]
            .into_iter()
            .collect();
        let cancel = AtomicBool::new(false);
        let before = command::checked(client.path(), &["count-objects", "-v"]).unwrap();
        assert_eq!(
            advertise(client.path(), &remote, &names, &cancel)
                .unwrap()
                .len(),
            2
        );
        let second = commit();
        command::checked(
            source.path(),
            &["update-ref", "refs/heads/topic/slash", &second],
        )
        .unwrap();
        command::checked(source.path(), &["update-ref", "-d", "refs/heads/deleted"]).unwrap();
        assert_eq!(
            advertise(client.path(), &remote, &names, &cancel).unwrap(),
            vec![Branch {
                name: "topic/slash".into(),
                sha: second
            }]
        );
        assert_eq!(
            command::checked(client.path(), &["count-objects", "-v"]).unwrap(),
            before
        );
        assert!(
            command::checked(client.path(), &["for-each-ref"])
                .unwrap()
                .is_empty()
        );
        assert!(!client.path().join(".git/FETCH_HEAD").exists());
        cancel.store(true, Ordering::Relaxed);
        assert!(advertise(client.path(), &remote, &names, &cancel).is_none());
    }

    #[test]
    fn unrelated_remote_tail_has_a_bounded_page_probe() {
        let names: HashSet<_> = (10000..10100).map(|i| format!("branch-{i}")).collect();
        let calls = std::sync::atomic::AtomicUsize::new(1);
        let complete = scan(
            page(0, 100, true),
            &names,
            &AtomicBool::new(false),
            |n| {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(page((n - 1) * 100, n * 100, true))
            },
            |b, s, c| {
                assert!(b.is_empty());
                assert!(s.is_empty());
                assert!(!c);
            },
        )
        .unwrap();
        assert!(!complete);
        assert_eq!(calls.load(Ordering::Relaxed), 4);
    }
    #[test]
    fn provider_bulk_endpoints_filter_scope_and_keep_missing_heads_on_http_error() {
        for provider in [Provider::Github, Provider::Gitlab] {
            let dir = tempfile::tempdir().unwrap();
            let values: Vec<_> = (0..65)
                .map(|i| {
                    let commit = if provider == Provider::Github {
                        serde_json::json!({"sha": "a".repeat(40)})
                    } else {
                        serde_json::json!({"id": "a".repeat(40)})
                    };
                    serde_json::json!({"name": format!("branch-{i}"), "commit": commit})
                })
                .collect();
            let (url, server) = super::super::tests::server(vec![
                (200, String::new(), serde_json::to_vec(&values).unwrap()),
                (503, String::new(), vec![]),
            ]);
            let mut remote = Remote::parse(
                "origin",
                "https://example.invalid/team/repo",
                Some("github"),
            )
            .unwrap();
            remote.provider = provider;
            remote.endpoint = format!("{url}/branches");
            let names = (0..64)
                .map(|i| format!("branch-{i}"))
                .chain(["deleted".into()])
                .collect();
            let mut token = Some("fixture-token".into());
            let mut found = Vec::new();
            let mut scope = HashSet::new();
            load(
                dir.path(),
                &remote,
                &names,
                &mut token,
                &AtomicBool::new(false),
                &mut found,
                &mut scope,
                &|_| {},
            )
            .unwrap();
            assert_eq!(found.len(), 64);
            assert_eq!(scope, names);
            found.clear();
            scope.clear();
            assert!(
                load(
                    dir.path(),
                    &remote,
                    &names,
                    &mut token,
                    &AtomicBool::new(false),
                    &mut found,
                    &mut scope,
                    &|_| {}
                )
                .is_err()
            );
            assert!(scope.is_empty());
            let requests = server.join().unwrap();
            assert_eq!(requests.len(), 2);
            assert!(
                requests
                    .iter()
                    .all(|r| r.starts_with("GET /branches?per_page=100&page=1 "))
            );
        }
    }

    fn page(start: usize, end: usize, more: bool) -> Page {
        Page {
            etag: None,
            more,
            branches: (start..end)
                .map(|i| Branch {
                    name: format!("branch-{i}"),
                    sha: "a".repeat(40),
                })
                .collect(),
        }
    }

    #[test]
    fn large_scope_uses_pages_and_filters_unrelated_heads_and_proves_deletions() {
        let names: HashSet<_> = (0..1370).map(|i| format!("branch-{i}")).collect();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let mut final_batch = None;
        scan(
            page(0, 100, true),
            &names,
            &AtomicBool::new(false),
            |n| {
                calls.fetch_add(1, Ordering::Relaxed);
                // Missing branch-1369 plus 1,000 unrelated server-only branches.
                let mut p = page((n - 1) * 100, (n * 100).min(2370), n * 100 < 2370);
                p.branches.retain(|b| b.name != "branch-1369");
                Ok(p)
            },
            |b, s, c| final_batch = Some((b, s, c)),
        )
        .unwrap();
        let (branches, scope, complete) = final_batch.unwrap();
        assert!(complete);
        assert_eq!(branches.len(), 1369);
        assert_eq!(scope, names);
        assert!(calls.load(Ordering::Relaxed) <= 24);
    }

    #[test]
    fn partial_failure_never_deletes_unchecked_heads() {
        let names = (0..400).map(|i| format!("branch-{i}")).collect();
        let mut last = None;
        assert!(
            scan(
                page(0, 100, true),
                &names,
                &AtomicBool::new(false),
                |n| {
                    if n == 2 {
                        Err("offline".into())
                    } else {
                        Ok(page((n - 1) * 100, n * 100, n < 4))
                    }
                },
                |b, s, c| last = Some((b, s, c))
            )
            .is_err()
        );
        let (branches, scope, complete) = last.unwrap();
        assert!(!complete);
        assert!(!scope.contains("branch-150"));
        assert_eq!(scope.len(), branches.len());
        assert!(scope.contains("branch-250"));
    }

    #[test]
    fn cancellation_publishes_nothing_and_stops_requests() {
        scan(
            page(0, 100, true),
            &HashSet::new(),
            &AtomicBool::new(true),
            |_| panic!("cancelled request"),
            |_, _, _| panic!("cancelled publication"),
        )
        .unwrap_err();
    }

    #[test]
    fn all_requested_heads_stop_before_unrelated_tail() {
        let names = (0..100).map(|i| format!("branch-{i}")).collect();
        scan(
            page(0, 100, true),
            &names,
            &AtomicBool::new(false),
            |_| panic!("unnecessary page"),
            |b, s, c| {
                assert_eq!(b.len(), 100);
                assert_eq!(s.len(), 100);
                assert!(c);
            },
        )
        .unwrap();
    }
}
