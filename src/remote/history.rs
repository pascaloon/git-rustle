//! Read-only commit previews. Immutable metadata is cached separately from Git objects.
use super::*;
#[cfg(test)]
use crate::git::RefKind;
use crate::git::{CommitDetails, RefSnapshot};
use crate::model::{CommitRec, History};
use gix::ObjectId;
use std::collections::{BinaryHeap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

// Repository tag phases and individual cache-file writes use separate path keys.
// Network waits never hold a cache-file write lock.
fn shared_gate(path: &Path) -> std::sync::Arc<std::sync::Mutex<()>> {
    type Gate = std::sync::Mutex<()>;
    static GATES: std::sync::LazyLock<
        std::sync::Mutex<HashMap<std::path::PathBuf, std::sync::Weak<Gate>>>,
    > = std::sync::LazyLock::new(Default::default);
    let mut gates = GATES.lock().unwrap();
    gates.retain(|_, gate| gate.strong_count() > 0);
    if let Some(gate) = gates.get(path).and_then(std::sync::Weak::upgrade) {
        return gate;
    }
    let gate = std::sync::Arc::new(std::sync::Mutex::new(()));
    gates.insert(path.to_path_buf(), std::sync::Arc::downgrade(&gate));
    gate
}

const MAX_COMMITS: usize = 5000;
const MAX_REQUESTS: usize = 100;

#[derive(Clone, Serialize, Deserialize)]
struct Metadata {
    sha: String,
    parents: Vec<String>,
    author: String,
    email: String,
    author_time: i64,
    author_offset: i32,
    committer: String,
    committer_email: String,
    commit_time: i64,
    commit_offset: i32,
    message: String,
}

impl Metadata {
    fn details(&self) -> Result<CommitDetails, String> {
        if self.parents.len() > 64 || self.message.len() > 65535 || self.author.len() > 1024 {
            return Err("Oversized commit metadata".into());
        }
        let id = oid(&self.sha)?;
        let parents = self
            .parents
            .iter()
            .map(|p| oid(p))
            .collect::<Result<Vec<_>, _>>()?;
        if parents.contains(&id) {
            return Err("Invalid commit ancestry".into());
        }
        Ok(CommitDetails {
            id,
            parents,
            author_name: self.author.clone(),
            author_email: self.email.clone(),
            author_time: self.author_time,
            author_offset: self.author_offset,
            committer_name: self.committer.clone(),
            committer_email: self.committer_email.clone(),
            commit_time: self.commit_time,
            commit_offset: self.commit_offset,
            message: self.message.clone(),
        })
    }
}

fn oid(value: &str) -> Result<ObjectId, String> {
    ObjectId::from_hex(value.as_bytes()).map_err(|_| "Invalid commit ID in API metadata".into())
}

fn parse_commits(provider: Provider, bytes: &[u8]) -> Result<Vec<Metadata>, String> {
    let values: Vec<serde_json::Value> =
        serde_json::from_slice(bytes).map_err(|_| "Invalid commit API response")?;
    if values.len() > 100 {
        return Err("Oversized commit API page".into());
    }
    values
        .iter()
        .map(|v| {
            let string = |v: &serde_json::Value, key: &str| {
                v.get(key)
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
                    .ok_or_else(|| format!("Commit API omitted {key}"))
            };
            let (sha, parents, author, email, adate, committer, committer_email, cdate, message) =
                match provider {
                    Provider::Github => {
                        let c = &v["commit"];
                        let parents = v["parents"]
                            .as_array()
                            .ok_or("Commit API omitted parents")?
                            .iter()
                            .map(|p| string(p, "sha"))
                            .collect::<Result<Vec<_>, _>>()?;
                        (
                            string(v, "sha")?,
                            parents,
                            string(&c["author"], "name")?,
                            string(&c["author"], "email")?,
                            string(&c["author"], "date")?,
                            string(&c["committer"], "name")?,
                            string(&c["committer"], "email")?,
                            string(&c["committer"], "date")?,
                            string(c, "message")?,
                        )
                    }
                    Provider::Gitlab => {
                        let parents = v["parent_ids"]
                            .as_array()
                            .ok_or("Commit API omitted parents")?
                            .iter()
                            .map(|p| {
                                p.as_str()
                                    .map(str::to_owned)
                                    .ok_or("Invalid parent ID".to_string())
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        (
                            string(v, "id")?,
                            parents,
                            string(v, "author_name")?,
                            string(v, "author_email")?,
                            string(v, "authored_date")?,
                            string(v, "committer_name")?,
                            string(v, "committer_email")?,
                            string(v, "committed_date")?,
                            string(v, "message")?,
                        )
                    }
                };
            let author_time = gix::date::parse(&adate, None).map_err(|_| "Invalid author date")?;
            let commit_time = gix::date::parse(&cdate, None).map_err(|_| "Invalid commit date")?;
            let m = Metadata {
                sha,
                parents,
                author,
                email,
                author_time: author_time.seconds,
                author_offset: author_time.offset,
                committer,
                committer_email,
                commit_time: commit_time.seconds,
                commit_offset: commit_time.offset,
                message,
            };
            m.details()?;
            Ok(m)
        })
        .collect()
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Stored {
    version: u32,
    identity: String,
    commits: Vec<Metadata>,
    tags: Vec<Branch>,
    tags_saved: u64,
}

fn read_stored(path: &Path, identity: &str) -> Option<Stored> {
    if std::fs::metadata(path).ok()?.len() > MAX_CACHE {
        return None;
    }
    let stored: Stored = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if stored.version != 1
        || stored.identity != identity
        || stored.commits.len() > MAX_COMMITS
        || stored.tags.len() > 10000
        || stored.commits.iter().any(|c| {
            oid(&c.sha).is_err()
                || c.parents.len() > 64
                || c.message.len() > 65535
                || c.author.len() > 1024
        })
        || stored.tags.iter().any(|t| !valid_branch(t))
    {
        return None;
    }
    Some(stored)
}

fn write_stored(path: &Path, data: &Stored) {
    let gate = shared_gate(path);
    let Ok(_write) = gate.lock() else {
        return;
    };
    write_stored_atomic(path, data);
}

// An older tag job must not replace metadata written by a newer commit overlay,
// and a newer overlay must retain tags completed since its initial cache read.
fn update_history_cache(path: &Path, data: &mut Stored, tags: bool, cancel: &AtomicBool) {
    let gate = shared_gate(path);
    let Ok(_write) = gate.lock() else {
        return;
    };
    if cancel.load(Ordering::Relaxed) {
        return;
    }
    if let Some(latest) = read_stored(path, &data.identity) {
        if tags {
            data.commits = latest.commits;
        } else {
            data.tags = latest.tags;
            data.tags_saved = latest.tags_saved;
        }
    }
    write_stored_atomic(path, data);
}

fn write_stored_atomic(path: &Path, data: &Stored) {
    let Some(parent) = path.parent() else { return };
    let Ok(bytes) = serde_json::to_vec(data) else {
        return;
    };
    if bytes.len() as u64 > MAX_CACHE || std::fs::create_dir_all(parent).is_err() {
        return;
    }
    static TEMP_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("{}.{sequence}.tmp", std::process::id()));
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
    let _ = std::fs::remove_file(tmp);
}

struct Client<'a> {
    remote: &'a Remote,
    path: &'a Path,
    http: Http,
    token: Option<String>,
    helper: bool,
    requests: usize,
    start: Instant,
    cancel: &'a AtomicBool,
}

impl Client<'_> {
    fn get(&mut self, url: &str) -> Result<Response, String> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err("Remote preview cancelled".into());
        }
        if self.requests >= MAX_REQUESTS || self.start.elapsed() > Duration::from_secs(60) {
            return Err("Remote preview limit reached; showing available metadata".into());
        }
        self.requests += 1;
        let mut response = self
            .http
            .get_url(self.remote, url, None, self.token.as_deref())?;
        if matches!(response.status, 401 | 403 | 404) && self.token.is_none() && !self.helper {
            self.helper = true;
            self.token = helper_token(self.path, self.remote);
            if self.token.is_some() {
                response = self
                    .http
                    .get_url(self.remote, url, None, self.token.as_deref())?;
            }
        }
        if response.status != 200 {
            return Err(parse_page(self.remote.provider, response, None)
                .err()
                .unwrap_or_else(|| "Unexpected API status".into()));
        }
        Ok(response)
    }
}

/// Only actual local branches qualify, including differently named upstreams.
pub(super) fn eligible(
    path: &Path,
    _refs: &RefSnapshot,
    remote: &Remote,
) -> Result<HashSet<String>, String> {
    let (_, upstreams) = configuration_data(path)?;
    eligible_from_config(path, remote, &upstreams)
}

pub(super) fn eligible_from_config(
    path: &Path,
    remote: &Remote,
    upstreams: &Upstreams,
) -> Result<HashSet<String>, String> {
    use gix::bstr::ByteSlice;
    // Reuse the effective configuration already read during discovery. Only the
    // local ref names remain to read, without spawning another git.exe process.
    let repo = crate::git::open(path).map_err(|e| e.to_string())?;
    if crate::git::refs::is_reftable(&repo) {
        let mut names = HashSet::new();
        for branch in crate::git::refs::raw_refs(&repo)? {
            let Some(name) = branch.name.strip_prefix("refs/heads/") else {
                continue;
            };
            match upstreams.get(name) {
                Some((Some(upstream), Some(merge))) if upstream == &remote.name => {
                    if let Some(name) = merge.strip_prefix("refs/heads/") {
                        names.insert(name.to_owned());
                    }
                }
                None | Some((None, _)) => {
                    names.insert(name.to_owned());
                }
                _ => {}
            }
        }
        return Ok(names);
    }
    let platform = repo.references().map_err(|e| e.to_string())?;
    let iter = platform.local_branches().map_err(|e| e.to_string())?;
    let mut names = HashSet::new();
    for branch in iter {
        let branch = branch.map_err(|e| e.to_string())?;
        let name = branch.name().shorten().to_str_lossy();
        match upstreams.get(name.as_ref()) {
            Some((Some(upstream), Some(merge))) if upstream == &remote.name => {
                if let Some(name) = merge.strip_prefix("refs/heads/") {
                    names.insert(name.to_string());
                }
            }
            None | Some((None, _)) => {
                names.insert(name.into_owned());
            }
            _ => {}
        }
    }
    Ok(names)
}

pub struct Preview {
    pub history: Option<History>,
    pub ghosts: HashMap<ObjectId, CommitDetails>,
    pub sources: HashMap<ObjectId, Remote>,
    pub branches: Vec<(ObjectId, String)>,
    pub tags: Vec<(ObjectId, String)>,
    pub errors: Vec<String>,
}

/// Browser enrichment uses at most one immutable commit-list request per tip.
/// Reuse preview metadata and local objects before asking the provider.
pub fn branch_activity(
    repo: &gix::Repository,
    remote: &Remote,
    tip: &str,
    now: i64,
    no_cache: bool,
) -> Result<crate::branches::Activity, String> {
    let path = repo.workdir().unwrap_or_else(|| repo.git_dir());
    let token = cached_token(path, &crate::git::git_dir(repo), remote)?;
    let location = if no_cache {
        None
    } else {
        cache_file(&crate::git::git_dir(repo), remote, token.as_deref())
            .map(|(p, id)| (p.with_extension("history.json"), id))
    };
    let mut stored = location
        .as_ref()
        .and_then(|(p, id)| read_stored(p, id))
        .unwrap_or_default();
    let mut known: HashMap<_, _> = stored
        .commits
        .drain(..)
        .map(|m| (m.sha.clone(), m))
        .collect();
    if let Some((p, id)) = &location
        && let Some(cached) = read_stored(&p.with_extension("activity.json"), id)
    {
        for m in cached.commits {
            if known.len() < MAX_COMMITS {
                known.insert(m.sha.clone(), m);
            }
        }
    }
    let cancel = AtomicBool::new(false);
    let mut client = Client {
        remote,
        path,
        token,
        http: Http::new(),
        helper: false,
        requests: 0,
        start: Instant::now(),
        cancel: &cancel,
    };
    let root = remote
        .endpoint
        .strip_suffix("/branches")
        .ok_or("Invalid API endpoint")?;
    let mut next = Some(oid(tip)?);
    let mut first = None;
    let mut recent = 0;
    let mut visited = HashSet::new();
    for _ in 0..100 {
        let Some(id) = next else { break };
        if !visited.insert(id) {
            break;
        }
        let details = if let Ok(d) = crate::git::commit_details(repo, &id) {
            d
        } else {
            if !known.contains_key(&id.to_string()) && client.requests == 0 {
                let param = if remote.provider == Provider::Github {
                    "sha"
                } else {
                    "ref_name"
                };
                let response =
                    match client.get(&format!("{root}/commits?{param}={id}&per_page=100&page=1")) {
                        Ok(response) => response,
                        Err(_) if first.is_some() => break,
                        Err(error) => return Err(error),
                    };
                let page = parse_commits(remote.provider, &response.body)?;
                if known.len() + page.len() > MAX_COMMITS {
                    known.clear();
                }
                for m in page {
                    if known.len() < MAX_COMMITS {
                        known.insert(m.sha.clone(), m);
                    }
                }
            }
            let Some(m) = known.get(&id.to_string()) else {
                break;
            };
            m.details()?
        };
        recent += u32::from(
            details.commit_time >= now - crate::branches::WEEK && details.commit_time <= now,
        );
        next = details.parents.first().copied();
        first.get_or_insert(details);
    }
    let first = first.ok_or("Commit API did not return the branch tip")?;
    if let Some((file, identity)) = if no_cache {
        None
    } else {
        cache_file(&crate::git::git_dir(repo), remote, client.token.as_deref())
            .map(|(p, id)| (p.with_extension("history.json"), id))
    } {
        stored.version = 1;
        stored.identity = identity;
        stored.commits = known.into_values().collect();
        write_stored(&file.with_extension("activity.json"), &stored);
    }
    Ok(crate::branches::Activity {
        author: first.author_name,
        author_time: first.author_time,
        commit_time: first.commit_time,
        recent,
        limited: next.is_some(),
    })
}

/// Only workers call this function. The existing graph remains drawable throughout.
#[cfg(test)]
pub fn load(
    repo: &gix::Repository,
    local: &History,
    refs: &RefSnapshot,
    updates: &[Update],
    force: bool,
    no_cache: bool,
    cancel: &AtomicBool,
) -> Preview {
    load_stream(
        repo,
        local,
        refs,
        updates,
        &[],
        force,
        no_cache,
        cancel,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn load_stream(
    repo: &gix::Repository,
    local: &History,
    refs: &RefSnapshot,
    updates: &[Update],
    requested: &[crate::branches::Target],
    force: bool,
    no_cache: bool,
    cancel: &AtomicBool,
    mut publish: Option<&mut dyn FnMut(Preview)>,
) -> Preview {
    let mut span = crate::diagnostics::Span::new("remote_ghosts");
    span.field("providers", updates.len() as u64);
    span.field("force", force as u64);
    span.outcome("cancelled");
    let mut commits_span = crate::diagnostics::Span::new("remote_ghost_commit_phase");
    let path = repo.workdir().unwrap_or_else(|| repo.git_dir());
    let git_dir = crate::git::git_dir(repo);
    let local_ids: HashSet<_> = local.commits.iter().map(|c| c.id).collect();
    let mut extra: HashMap<ObjectId, CommitDetails> = HashMap::new();
    let mut ghosts = HashMap::new();
    let mut tags = Vec::new();
    let mut errors = Vec::new();
    let mut sources = HashMap::new();
    let mut branch_labels = Vec::new();
    let mut tag_jobs = Vec::new();
    let mut partitions = Vec::new();
    for update in updates {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let result = (|| -> Result<(), String> {
            let remote = &update.remote;
            let mut names = if update.scoped {
                update.branches.iter().map(|b| b.name.clone()).collect()
            } else {
                eligible(path, refs, remote)?
            };
            names.extend(
                requested
                    .iter()
                    .filter(|t| {
                        t.remote.as_ref() == Some(&remote.name)
                            && t.url.as_ref().is_none_or(|url| url == &remote.url)
                    })
                    .map(|t| t.name.clone()),
            );
            crate::diagnostics::event("remote_ghost_eligible", &[("heads", names.len() as u64)]);
            let tips: Vec<_> = update
                .branches
                .iter()
                .filter(|b| names.contains(&b.name))
                .map(|b| oid(&b.sha))
                .collect::<Result<_, _>>()?;
            if tips.is_empty() {
                return Ok(());
            }
            for b in update.branches.iter().filter(|b| names.contains(&b.name)) {
                branch_labels.push((oid(&b.sha)?, format!("{}/{}", remote.name, b.name)));
            }
            let token = cached_token(path, &git_dir, remote)?;
            let location = if no_cache {
                None
            } else {
                cache_file(&git_dir, remote, token.as_deref())
                    .map(|(p, id)| (p.with_extension("history.json"), id))
            };
            let mut stored = location
                .as_ref()
                .and_then(|(p, id)| read_stored(p, id))
                .unwrap_or_default();
            crate::diagnostics::event(
                "remote_ghost_cache",
                &[
                    ("commits", stored.commits.len() as u64),
                    ("tags", stored.tags.len() as u64),
                ],
            );
            let mut known: HashMap<_, _> = stored
                .commits
                .drain(..)
                .map(|m| (oid(&m.sha).unwrap(), m))
                .collect();
            let mut client = Client {
                remote,
                path,
                http: Http::new(),
                token,
                helper: false,
                requests: 0,
                start: Instant::now(),
                cancel,
            };
            let root = remote
                .endpoint
                .strip_suffix("/branches")
                .ok_or("Invalid branch API endpoint")?;
            let mut pending = tips;
            let mut visited = HashSet::new();
            let mut failure = None;
            while let Some(id) = pending.pop() {
                if local_ids.contains(&id) || !visited.insert(id) {
                    continue;
                }
                if extra.len() >= MAX_COMMITS {
                    failure = Some("Remote preview limited to 5,000 additional commits".into());
                    break;
                }
                if cancel.load(Ordering::Relaxed) {
                    return Err("Remote preview cancelled".into());
                }
                let details = if let Some(d) = extra.get(&id) {
                    d.clone()
                } else if let Ok(d) = crate::git::commit_details(repo, &id) {
                    d
                } else {
                    if !known.contains_key(&id) {
                        let param = match remote.provider {
                            Provider::Github => "sha",
                            Provider::Gitlab => "ref_name",
                        };
                        let url = format!("{root}/commits?{param}={id}&per_page=100&page=1");
                        let page = client
                            .get(&url)
                            .and_then(|r| parse_commits(remote.provider, &r.body));
                        match page {
                            Ok(page) => {
                                if known.len() + page.len() > MAX_COMMITS {
                                    known.retain(|id, _| visited.contains(id));
                                }
                                for m in page {
                                    let id = oid(&m.sha)?;
                                    if known.len() < MAX_COMMITS && !local_ids.contains(&id) {
                                        known.insert(id, m);
                                    }
                                }
                            }
                            Err(error) => {
                                failure = Some(error);
                                break;
                            }
                        }
                    }
                    let Some(m) = known.get(&id) else {
                        failure = Some("Commit API did not return the requested commit".into());
                        break;
                    };
                    let d = m.details()?;
                    sources.insert(id, remote.clone());
                    ghosts.insert(id, d.clone());
                    d
                };
                pending.extend(details.parents.iter().copied());
                extra.insert(id, details);
            }
            for tag in &stored.tags {
                let id = oid(&tag.sha)?;
                if local_ids.contains(&id) || extra.contains_key(&id) {
                    tags.push((id, format!("{}/{}", remote.name, tag.name)));
                }
            }
            let location = if no_cache {
                None
            } else {
                cache_file(&git_dir, remote, client.token.as_deref())
                    .map(|(p, id)| (p.with_extension("history.json"), id))
            };
            stored.commits = known.into_values().collect();
            if let Some((file, identity)) = &location {
                stored.version = 1;
                partitions.push(identity.clone());
                stored.identity = identity.clone();
                let _cache_write = crate::diagnostics::Span::new("remote_history_cache_write");
                update_history_cache(file, &mut stored, false, cancel);
            }
            tag_jobs.push((remote.clone(), stored, location, client.token));
            if let Some(error) = failure {
                return Err(error);
            }
            Ok(())
        })();
        if let Err(error) = result {
            errors.push(format!("{}: {error}", command::clean(&update.remote.name)));
        }
    }
    commits_span.field("ghosts", ghosts.len() as u64);
    drop(commits_span);
    let merge_span = crate::diagnostics::Span::new("remote_ghost_merge");
    let history = if extra.is_empty() {
        None
    } else {
        match merge_cached(
            local,
            &extra,
            refs.fingerprint,
            &git_dir,
            &partitions,
            no_cache,
        ) {
            Ok(mut h) => {
                h.remote_cells = crate::graph::dimming::build(&h, |id| ghosts.contains_key(id));
                Some(h)
            }
            Err(e) => {
                errors.push(e);
                ghosts.clear();
                None
            }
        }
    };
    drop(merge_span);
    let mut preview = Preview {
        history,
        ghosts,
        sources,
        branches: branch_labels,
        tags,
        errors,
    };
    // Publish commits and cached tags before any tag request can delay the graph.
    if let Some(publish) = publish.as_mut() {
        publish(Preview {
            history: preview.history.take(),
            ghosts: preview.ghosts.clone(),
            sources: preview.sources.clone(),
            branches: preview.branches.clone(),
            tags: preview.tags.clone(),
            errors: preview.errors.clone(),
        });
    }
    let gate = shared_gate(&git_dir);
    let tag_wait_span = crate::diagnostics::Span::new("remote_tag_gate_wait");
    let _tag_phase = loop {
        if cancel.load(Ordering::Relaxed) {
            return preview;
        }
        match gate.try_lock() {
            Ok(permit) => break permit,
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            Err(std::sync::TryLockError::Poisoned(_)) => return preview,
        }
    };
    drop(tag_wait_span);
    let mut tags_span = crate::diagnostics::Span::new("remote_tags");
    tags_span.field("providers", tag_jobs.len() as u64);
    for (remote, mut stored, location, token) in tag_jobs {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if !force && stored.tags_saved != 0 && now().saturating_sub(stored.tags_saved) < TTL {
            crate::diagnostics::event("remote_tags_cache_hit", &[]);
            continue;
        }
        let mut client = Client {
            remote: &remote,
            path,
            http: Http::new(),
            token,
            helper: false,
            requests: 0,
            start: Instant::now(),
            cancel,
        };
        let root = remote.endpoint.strip_suffix("/branches").unwrap();
        let mut fresh = Vec::new();
        let mut failure = None;
        let mut complete = false;
        for page in 1..=100 {
            crate::diagnostics::event("remote_tag_page", &[("page", page as u64)]);
            match client
                .get(&format!("{root}/tags?per_page=100&page={page}"))
                .and_then(|r| parse_tags(remote.provider, r))
            {
                Ok(p) => {
                    fresh.extend(p.branches);
                    if !p.more {
                        complete = true;
                        break;
                    }
                }
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if complete {
            stored.tags = fresh;
            stored.tags_saved = now();
            // Tags may be the first authenticated endpoint (all commits already local).
            // Never persist helper-authenticated tags in an anonymous partition.
            if location.is_some()
                && let Some((file, identity)) =
                    cache_file(&git_dir, &remote, client.token.as_deref())
            {
                stored.identity = identity;
                update_history_cache(
                    &file.with_extension("history.json"),
                    &mut stored,
                    true,
                    cancel,
                );
            }
            let prefix = format!("{}/", remote.name);
            preview.tags.retain(|(_, name)| !name.starts_with(&prefix));
            for tag in &stored.tags {
                if let Ok(id) = oid(&tag.sha)
                    && (local_ids.contains(&id) || extra.contains_key(&id))
                {
                    preview
                        .tags
                        .push((id, format!("{}/{}", remote.name, tag.name)));
                }
            }
        } else {
            preview.errors.push(format!(
                "{} tags: {}; {}",
                command::clean(&remote.name),
                failure.unwrap_or_else(|| "pagination limit reached; use Refresh to retry".into()),
                if stored.tags.is_empty() {
                    "tag preview unavailable"
                } else {
                    "cached tags retained"
                }
            ));
        }
    }
    span.field("ghosts", preview.ghosts.len() as u64);
    span.field("tags", preview.tags.len() as u64);
    span.field("errors", preview.errors.len() as u64);
    span.outcome(if cancel.load(Ordering::Relaxed) {
        "cancelled"
    } else if preview.errors.is_empty() {
        "success"
    } else {
        "error"
    });
    preview
}

/// The overlay cache is separate from the local Git graph and partitioned by credentials.
/// Its fingerprint covers the local graph and immutable extra commit IDs.
fn merge_cached(
    local: &History,
    extra: &HashMap<ObjectId, CommitDetails>,
    fingerprint: u64,
    git_dir: &Path,
    partitions: &[String],
    no_cache: bool,
) -> Result<History, String> {
    if no_cache {
        return merge(local, extra);
    }
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    partitions.hash(&mut hash);
    let location = crate::cache::cache_path(git_dir)
        .map(|p| p.with_extension(format!("{:016x}.preview.bin", hash.finish())));
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    ("local-heads-v2", fingerprint, local.len()).hash(&mut hash);
    let mut ids: Vec<_> = extra.keys().collect();
    ids.sort_unstable();
    ids.hash(&mut hash);
    let key = hash.finish();
    if let Some(path) = &location
        && let Ok(cached) = crate::cache::read(path)
        && cached.fingerprint == key
        && cached.history.len() == local.len() + extra.len()
    {
        return Ok(cached.history);
    }
    let history = merge(local, extra)?;
    if let Some(path) = location {
        let _ = crate::cache::write(&path, git_dir, key, &history);
    }
    Ok(history)
}

/// Re-layout only when extra commits exist. Parent order wins over clock skew.
fn merge(local: &History, extra: &HashMap<ObjectId, CommitDetails>) -> Result<History, String> {
    let mut source = local.clone();
    let mut extras: Vec<_> = extra.values().collect();
    extras.sort_by_key(|d| d.id);
    for d in extras {
        let parents = source.push_parents(&d.parents);
        let author = source.push_text(d.author_name.as_bytes());
        let subject = source.push_text(d.message.lines().next().unwrap_or_default().as_bytes());
        source.commits.push(CommitRec {
            id: d.id,
            parents,
            author_time: d.author_time,
            author_offset: d.author_offset,
            commit_time: d.commit_time,
            author,
            subject,
            row: Default::default(),
            node_lane: 0,
        });
    }
    let index: HashMap<_, _> = source
        .commits
        .iter()
        .enumerate()
        .map(|(i, c)| (c.id, i))
        .collect();
    let mut children = vec![0usize; source.len()];
    for i in 0..source.len() {
        for p in source.parents(i) {
            if let Some(&j) = index.get(p) {
                children[j] += 1;
            }
        }
    }
    let mut ready = BinaryHeap::new();
    for (i, c) in source.commits.iter().enumerate() {
        if children[i] == 0 {
            ready.push((c.commit_time, std::cmp::Reverse(i)));
        }
    }
    let mut output = History {
        text: std::mem::take(&mut source.text),
        parents: source.parents.clone(),
        complete: true,
        ..History::default()
    };
    let mut layout = crate::graph::layout::Layout::new();
    while let Some((_, std::cmp::Reverse(i))) = ready.pop() {
        let mut c = source.commits[i];
        let parents: Vec<_> = source
            .parents(i)
            .iter()
            .filter(|p| index.contains_key(*p))
            .copied()
            .collect();
        let row = layout.push(&c.id, &parents);
        c.row = output.push_cells(&row.cells);
        c.node_lane = row.node_lane;
        output.commits.push(c);
        for p in parents {
            let j = index[&p];
            children[j] -= 1;
            if children[j] == 0 {
                ready.push((source.commits[j].commit_time, std::cmp::Reverse(j)));
            }
        }
    }
    if output.len() != source.len() {
        return Err("Invalid cycle in remote commit ancestry; keeping local graph".into());
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> (tempfile::TempDir, gix::Repository, RefSnapshot, History) {
        let dir = tempfile::tempdir().unwrap();
        command::checked(dir.path(), &["init", "-q", "-b", "main"]).unwrap();
        command::checked(
            dir.path(),
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--allow-empty",
                "-qm",
                "local root",
            ],
        )
        .unwrap();
        let repo = crate::git::open(dir.path()).unwrap();
        let refs = crate::git::snapshot_refs(&repo).unwrap();
        let mut local = History::default();
        crate::git::walk(&repo, &refs.tips, &mut |h| {
            local.append(h);
            true
        })
        .unwrap();
        (dir, repo, refs, local)
    }

    fn wire(provider: Provider, sha: &str, parents: &[String]) -> serde_json::Value {
        match provider {
            Provider::Github => {
                json!({"sha":sha, "parents": parents.iter().map(|p| json!({"sha":p})).collect::<Vec<_>>(),
                "commit":{"author":{"name":"Author", "email":"a@example.invalid", "date":"2026-10-15T12:00:00Z"},
                "committer":{"name":"Committer", "email":"c@example.invalid", "date":"2026-10-15T12:00:00Z"}, "message":"Missing commit\n\nFull message"}})
            }
            Provider::Gitlab => {
                json!({"id":sha, "parent_ids":parents, "author_name":"Author", "author_email":"a@example.invalid",
                "committer_name":"Committer", "committer_email":"c@example.invalid", "authored_date":"2026-10-15T08:00:00.123-04:00",
                "committed_date":"2026-10-15T12:00:00.123Z", "message":"Missing commit\n\nFull message"})
            }
        }
    }

    #[test]
    fn explicit_remote_only_branch_is_previewed_and_tab_navigates_without_fetching() {
        for provider in [Provider::Github, Provider::Gitlab] {
            let (dir, repo, refs, local) = fixture();
            let tip = "f".repeat(40);
            let root = refs.head_id.unwrap().to_string();
            let (url, server) = super::super::tests::server(vec![
                (
                    200,
                    String::new(),
                    serde_json::to_vec(&json!([wire(provider, &tip, &[root])])).unwrap(),
                ),
                (200, String::new(), b"[]".to_vec()),
            ]);
            let mut remote = Remote::parse(
                "origin",
                "https://example.invalid/team/repo",
                Some(provider.key()),
            )
            .unwrap();
            remote.endpoint = format!("{url}/branches");
            let update = Update {
                remote: remote.clone(),
                branches: vec![Branch {
                    name: "only-server".into(),
                    sha: tip.clone(),
                }],
                scoped: false,
                cached: false,
                stale: false,
            };
            let mut app = crate::app::App::new();
            app.set_cached(local.clone(), true);
            app.local_history = Some(std::sync::Arc::new(local.clone()));
            app.set_refs(refs.clone());
            app.branches.update(update.clone(), Some(&refs));
            app.branches.show(Some(&refs));
            app.branches.selected = app
                .branches
                .visible
                .iter()
                .position(|&i| app.branches.entries[i].target.name == "only-server")
                .unwrap();
            app.handle_key(crate::term::KeyEvent {
                code: crate::term::KeyCode::Tab,
                ctrl: false,
                shift: false,
                alt: false,
            });
            assert_eq!(app.branches.pending_jump, Some(oid(&tip).unwrap()));
            assert!(app.branches.preview_requested);
            let before = command::checked(dir.path(), &["show-ref"]).unwrap();
            let preview = load_stream(
                &repo,
                &local,
                &refs,
                &[update],
                &app.branches.preview_targets,
                false,
                true,
                &AtomicBool::new(false),
                None,
            );
            assert!(preview.errors.is_empty(), "{:?}", preview.errors);
            app.install_preview(preview);
            app.finish_branch_jump();
            assert_eq!(app.selected_id(), Some(oid(&tip).unwrap()));
            assert!(!app.branches.open);
            app.toggle_mark(oid(&tip).unwrap());
            app.toggle_mark(refs.head_id.unwrap());
            app.open_diff();
            assert!(matches!(app.mode, crate::app::Mode::Diff(_)));
            assert_eq!(app.marks.len(), 2);
            assert_eq!(before, command::checked(dir.path(), &["show-ref"]).unwrap());
            assert!(crate::git::commit_details(&repo, &oid(&tip).unwrap()).is_err());
            assert_eq!(server.join().unwrap().len(), 2);
        }
    }

    #[test]
    fn branch_activity_uses_first_parent_metadata_and_persists_offline() {
        for provider in [Provider::Github, Provider::Gitlab] {
            let (_dir, repo, _refs, _local) = fixture();
            let tip = "a".repeat(40);
            let parent = "b".repeat(40);
            let side = "c".repeat(40);
            let (url, server) = super::super::tests::server(vec![(
                200,
                String::new(),
                serde_json::to_vec(&json!([
                    wire(provider, &tip, &[parent.clone(), side.clone()]),
                    wire(provider, &parent, &[]),
                    wire(provider, &side, &[])
                ]))
                .unwrap(),
            )]);
            let mut remote = Remote::parse(
                "origin",
                "https://example.invalid/team/repo",
                Some(provider.key()),
            )
            .unwrap();
            remote.endpoint = format!("{url}/branches");
            let now = gix::date::parse("2026-10-15T12:00:01Z", None)
                .unwrap()
                .seconds;
            let activity = branch_activity(&repo, &remote, &tip, now, false).unwrap();
            assert_eq!(activity.recent, 2);
            assert_eq!(activity.author, "Author");
            assert!(!activity.limited);
            assert_eq!(server.join().unwrap().len(), 1);
            let cached = branch_activity(&repo, &remote, &tip, now, false).unwrap();
            assert_eq!(cached.recent, 2);
            let expired =
                branch_activity(&repo, &remote, &tip, now + crate::branches::WEEK, false).unwrap();
            assert_eq!(expired.recent, 0);
        }
    }

    #[test]
    fn providers_merge_all_missing_parents_tags_and_reuse_cache_without_fetching() {
        for provider in [Provider::Github, Provider::Gitlab] {
            let (dir, repo, refs, local) = fixture();
            let root = refs.head_id.unwrap().to_string();
            let tip = "a".repeat(40);
            let left = "b".repeat(40);
            let right = "c".repeat(40);
            let commits = json!([
                wire(provider, &tip, &[left.clone(), right.clone()]),
                wire(provider, &left, std::slice::from_ref(&root)),
                wire(provider, &right, std::slice::from_ref(&root))
            ]);
            let tags = match provider {
                Provider::Github => {
                    json!([{"name":"v-next", "commit":{"sha":tip}}, {"name":"unrelated", "commit":{"sha":"d".repeat(40)}}])
                }
                // Annotated tag target differs from its peeled commit: use commit.id.
                Provider::Gitlab => {
                    json!([{"name":"non-commit", "commit":{"id":""}}, {"name":"blob", "commit":null}, {"name":"v-next", "target":"e".repeat(40), "commit":{"id":tip}}, {"name":"unrelated", "commit":{"id":"d".repeat(40)}}])
                }
            };
            let (url, server) = super::super::tests::server(vec![
                (200, String::new(), serde_json::to_vec(&commits).unwrap()),
                (200, String::new(), serde_json::to_vec(&tags).unwrap()),
            ]);
            let mut remote = Remote::parse(
                "origin",
                "https://example.invalid/team/repo",
                Some(provider.key()),
            )
            .unwrap();
            remote.endpoint = format!("{url}/repository/branches");
            let update = Update {
                remote: remote.clone(),
                branches: vec![
                    Branch {
                        name: "main".into(),
                        sha: tip.clone(),
                    },
                    Branch {
                        name: "not-local".into(),
                        sha: "f".repeat(40),
                    },
                ],
                scoped: false,
                cached: false,
                stale: false,
            };
            let (cache, _) = cache_file(&crate::git::git_dir(&repo), &remote, None).unwrap();
            struct Cleanup(PathBuf);
            impl Drop for Cleanup {
                fn drop(&mut self) {
                    let _ = std::fs::remove_file(&self.0);
                }
            }
            let _cleanup = Cleanup(cache.with_extension("history.json"));
            let before_refs = command::checked(dir.path(), &["show-ref"]).unwrap();
            let cancel = AtomicBool::new(false);
            let preview = load(
                &repo,
                &local,
                &refs,
                std::slice::from_ref(&update),
                false,
                false,
                &cancel,
            );
            assert!(preview.errors.is_empty(), "{:?}", preview.errors);
            assert_eq!(preview.ghosts.len(), 3);
            let mut app = crate::app::App::new();
            app.history = local.clone();
            app.list_rows = 10;
            app.local_history = Some(std::sync::Arc::new(local.clone()));
            app.set_refs(refs.clone());
            let mut labels = update.clone();
            labels.branches.push(Branch {
                name: "unrelated-same-tip".into(),
                sha: tip.clone(),
            });
            app.branches.update(labels, Some(&refs));
            app.install_preview(Preview {
                history: preview.history.clone(),
                ghosts: preview.ghosts.clone(),
                sources: preview.sources.clone(),
                branches: preview.branches.clone(),
                tags: preview.tags.clone(),
                errors: Vec::new(),
            });
            assert_eq!(app.selected_id(), refs.head_id); // new rows do not move the user's selection
            assert_eq!(app.scroll, 0); // show nearby incoming rows when already at the top
            app.select(0);
            app.sync_details_changes();
            assert!(app.details_changes.is_some());
            app.toggle_mark(oid(&tip).unwrap());
            app.open_diff();
            assert_eq!(app.marks, vec![oid(&tip).unwrap()]);
            assert!(matches!(app.mode, crate::app::Mode::Diff(_)));
            app.mode = crate::app::Mode::Graph;
            let mut screen = crate::term::Screen::new(110, 25);
            crate::ui::list::draw(
                &mut app,
                &mut screen,
                crate::term::Rect::new(0, 0, 110, 25),
                0,
            );
            assert_eq!(screen.test_cells()[0].ch, '○'); // ordinary merge glyph
            assert_ne!(screen.test_cells()[0].style.fg, crate::ui::theme::FG_DATE);
            let text: String = screen.test_cells().iter().map(|c| c.ch).collect();
            assert!(!text.contains("[remote]"));
            assert!(text.contains("origin/v-next"));
            assert!(!text.contains("unrelated-same-tip"));
            assert!(
                app.labels[&oid(&tip).unwrap()]
                    .iter()
                    .all(|label| !label.name.contains("unrelated"))
            );
            assert!(
                screen
                    .test_cells()
                    .iter()
                    .any(|c| c.style.fg == crate::ui::theme::FG_HASH)
            );
            assert!(
                screen
                    .test_cells()
                    .iter()
                    .any(|c| c.style.fg == crate::ui::theme::FG_AUTHOR)
            );
            let history = preview.history.unwrap();
            assert_eq!(history.len(), 4);
            assert_eq!(history.commits[0].id, oid(&tip).unwrap());
            assert_eq!(history.commits[3].id, refs.head_id.unwrap());
            assert_eq!(history.parents(0).len(), 2);
            assert_eq!(
                preview.tags,
                vec![(oid(&tip).unwrap(), "origin/v-next".into())]
            );
            assert!(crate::git::commit_details(&repo, &oid(&tip).unwrap()).is_err());
            assert_eq!(
                command::checked(dir.path(), &["show-ref"]).unwrap(),
                before_refs
            );
            let requests = server.join().unwrap();
            assert_eq!(requests.len(), 2);
            assert!(requests[0].contains(&tip));
            assert!(!requests[0].contains(&"f".repeat(40)));
            assert!(requests[1].contains("/tags?"));
            // Server is gone: immutable commits and fresh tags must require no network.
            let cached = load(
                &repo,
                &local,
                &refs,
                std::slice::from_ref(&update),
                false,
                false,
                &cancel,
            );
            assert!(cached.errors.is_empty(), "{:?}", cached.errors);
            assert_eq!(cached.ghosts.len(), 3);
            assert_eq!(cached.tags.len(), 1);
            assert_eq!(
                cached.history.as_ref().unwrap().remote_cells,
                history.remote_cells
            );
            assert!(history.remote_cells.iter().any(|&flags| flags != 0));
            // Forced refresh while offline retains complete cached tags and commits.
            let offline = load(&repo, &local, &refs, &[update], true, false, &cancel);
            assert!(!offline.errors.is_empty());
            assert_eq!(offline.ghosts.len(), 3);
            assert_eq!(offline.tags.len(), 1);
        }
    }

    // Each forced refresh makes exactly one tag request. The same path covers
    // initial failure, a repeated failure, recovery and a fresh disk-cache hit.
    fn tag_refresh_samples(provider: Provider, rounds: usize) -> Vec<serde_json::Value> {
        let (dir, repo, refs, local) = fixture();
        let head = refs.head_id.unwrap();
        let tags = match provider {
            Provider::Github => json!([{"name":"v-next", "commit":{"sha":head.to_string()}}]),
            Provider::Gitlab => json!([{"name":"v-next", "commit":{"id":head.to_string()}}]),
        };
        let body = serde_json::to_vec(&tags).unwrap();
        let mut responses = vec![(200, String::new(), body.clone())];
        for _ in 0..rounds {
            responses.extend([
                (502, String::new(), Vec::new()),
                (502, String::new(), Vec::new()),
                (200, String::new(), body.clone()),
            ]);
        }
        let (url, server) = super::super::tests::server(responses);
        let mut remote = Remote::parse(
            "origin",
            "https://example.invalid/team/repo",
            Some(provider.key()),
        )
        .unwrap();
        remote.endpoint = format!("{url}/repository/branches");
        let update = Update {
            remote: remote.clone(),
            branches: vec![Branch {
                name: "main".into(),
                sha: head.to_string(),
            }],
            scoped: true,
            cached: false,
            stale: false,
        };
        let (cache, _) = cache_file(&crate::git::git_dir(&repo), &remote, None).unwrap();
        let cache = cache.with_extension("history.json");
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _cleanup = Cleanup(cache.clone());
        let before_refs = command::checked(dir.path(), &["show-ref"]).unwrap();
        let cancel = AtomicBool::new(false);
        let updates = std::slice::from_ref(&update);
        let first = load(&repo, &local, &refs, updates, false, false, &cancel);
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        assert_eq!(first.tags, vec![(head, "origin/v-next".into())]);
        let mut samples = Vec::new();
        for sample in 0..rounds {
            let start = Instant::now();
            let cached = load(&repo, &local, &refs, updates, false, false, &cancel);
            let cached_ms = start.elapsed().as_secs_f64() * 1000.;
            assert!(cached.errors.is_empty(), "{:?}", cached.errors);
            assert_eq!(cached.tags, first.tags);
            let before_cache = std::fs::read(&cache).unwrap();
            let mut failed_ms = [0.; 2];
            for elapsed in &mut failed_ms {
                let mut published = false;
                let start = Instant::now();
                let failed = load_stream(
                    &repo,
                    &local,
                    &refs,
                    updates,
                    &[],
                    true,
                    false,
                    &cancel,
                    Some(&mut |preview| {
                        assert_eq!(preview.tags, first.tags);
                        assert!(preview.errors.is_empty());
                        published = true;
                    }),
                );
                *elapsed = start.elapsed().as_secs_f64() * 1000.;
                assert!(published);
                assert_eq!(failed.tags, first.tags);
                assert_eq!(failed.errors.len(), 1);
                assert!(failed.errors[0].contains("502"));
                assert_eq!(std::fs::read(&cache).unwrap(), before_cache);
            }
            let start = Instant::now();
            let recovered = load(&repo, &local, &refs, updates, true, false, &cancel);
            let recovered_ms = start.elapsed().as_secs_f64() * 1000.;
            assert!(recovered.errors.is_empty(), "{:?}", recovered.errors);
            assert_eq!(recovered.tags, first.tags);
            if sample > 0 {
                samples.push(json!({"sample":sample, "cached_ms":cached_ms,
                    "failed_502_ms":failed_ms[0], "repeated_502_ms":failed_ms[1],
                    "recovered_200_ms":recovered_ms}));
            }
        }
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 1 + rounds * 3);
        assert!(requests.iter().all(|r| r.contains("/tags?")));
        assert_eq!(
            command::checked(dir.path(), &["show-ref"]).unwrap(),
            before_refs
        );
        samples
    }

    #[test]
    fn tag_refresh_retains_cache_through_repeated_502_and_recovers() {
        for provider in [Provider::Github, Provider::Gitlab] {
            tag_refresh_samples(provider, 1);
        }
    }

    #[test]
    fn first_tag_failure_explains_missing_preview_and_retry() {
        let (_dir, repo, refs, local) = fixture();
        let (url, server) = super::super::tests::server(vec![(502, String::new(), Vec::new())]);
        let mut remote = Remote::parse(
            "origin",
            "https://example.invalid/team/repo",
            Some("github"),
        )
        .unwrap();
        remote.endpoint = format!("{url}/repository/branches");
        let update = Update {
            remote,
            branches: vec![Branch {
                name: "main".into(),
                sha: refs.head_id.unwrap().to_string(),
            }],
            scoped: true,
            cached: false,
            stale: false,
        };
        let preview = load(
            &repo,
            &local,
            &refs,
            &[update],
            false,
            true,
            &AtomicBool::new(false),
        );
        assert!(preview.tags.is_empty());
        assert_eq!(
            preview.errors,
            vec![
                "origin tags: Remote server temporarily unavailable (HTTP 502); use Refresh to retry; tag preview unavailable"
            ]
        );
        assert_eq!(server.join().unwrap().len(), 1);
    }

    #[test]
    #[ignore = "explicit controlled-loopback tag failure and recovery performance measurement"]
    fn benchmark_tag_refresh() {
        for provider in [Provider::Github, Provider::Gitlab] {
            println!("{}", serde_json::to_string_pretty(&json!({
                "provider":provider.key(), "warmups":1, "samples":tag_refresh_samples(provider, 16)
            })).unwrap());
        }
    }

    #[test]
    fn concurrent_history_parts_preserve_new_metadata_and_completed_tags_and_cancel_under_lock() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("history.json");
        let metadata = |sha: &str| {
            parse_commits(
                Provider::Github,
                &serde_json::to_vec(&json!([wire(Provider::Github, sha, &[])])).unwrap(),
            )
            .unwrap()
        };
        let old = Stored {
            version: 1,
            identity: "fixture".into(),
            commits: metadata(&"a".repeat(40)),
            tags: vec![],
            tags_saved: 0,
        };
        write_stored(&file, &old);
        let cancel = AtomicBool::new(false);
        let mut newer = old.clone();
        newer.commits.extend(metadata(&"b".repeat(40)));
        update_history_cache(&file, &mut newer, false, &cancel);
        let mut stale_tag_job = old.clone();
        stale_tag_job.tags = vec![Branch {
            name: "v1".into(),
            sha: "b".repeat(40),
        }];
        stale_tag_job.tags_saved = 10;
        update_history_cache(&file, &mut stale_tag_job, true, &cancel);
        let latest = read_stored(&file, "fixture").unwrap();
        assert_eq!(latest.commits.len(), 2);
        assert_eq!(latest.tags_saved, 10);
        let mut stale_commit_job = old.clone();
        stale_commit_job.commits.extend(metadata(&"c".repeat(40)));
        update_history_cache(&file, &mut stale_commit_job, false, &cancel);
        let latest = read_stored(&file, "fixture").unwrap();
        assert_eq!(latest.tags.len(), 1);
        assert_eq!(latest.tags_saved, 10);
        let before = std::fs::read(&file).unwrap();
        let gate = shared_gate(&file);
        let guard = gate.lock().unwrap();
        let cancelled = std::sync::Arc::new(AtomicBool::new(false));
        let worker_cancel = cancelled.clone();
        let worker_file = file.clone();
        let (ready, receive) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut data = old;
            ready.send(()).unwrap();
            update_history_cache(&worker_file, &mut data, true, &worker_cancel);
        });
        receive.recv().unwrap();
        cancelled.store(true, Ordering::Relaxed);
        drop(guard);
        worker.join().unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), before);
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "owned temporary files cleaned"
        );
    }
    #[test]
    fn busy_tag_phase_never_blocks_new_commit_overlay_or_starts_another_tag_request() {
        let (_dir, repo, refs, local) = fixture();
        let git_dir = crate::git::git_dir(&repo);
        let gate = shared_gate(&git_dir);
        let permit = gate.lock().unwrap();
        assert!(std::sync::Arc::ptr_eq(&gate, &shared_gate(&git_dir)));
        let tip = "a".repeat(40);
        // Exactly one response is supplied: the new immutable commit metadata.
        // Attempting a second (tag) request fails this test's no-errors assertion.
        let (url, server) = super::super::tests::server(vec![(
            200,
            String::new(),
            serde_json::to_vec(&json!([wire(
                Provider::Github,
                &tip,
                &[refs.head_id.unwrap().to_string()]
            )]))
            .unwrap(),
        )]);
        let mut remote = Remote::parse(
            "origin",
            "https://example.invalid/team/repo",
            Some("github"),
        )
        .unwrap();
        remote.endpoint = format!("{url}/branches");
        let update = Update {
            remote,
            branches: vec![Branch {
                name: "main".into(),
                sha: tip,
            }],
            scoped: true,
            cached: false,
            stale: false,
        };
        let mut published = false;
        let cancel = AtomicBool::new(false);
        let result = load_stream(
            &repo,
            &local,
            &refs,
            &[update],
            &[],
            true,
            true,
            &cancel,
            Some(&mut |preview| {
                published = true;
                assert_eq!(preview.ghosts.len(), 1);
                cancel.store(true, Ordering::Relaxed);
            }),
        );
        assert!(published);
        assert!(result.errors.is_empty());
        assert_eq!(server.join().unwrap().len(), 1);
        assert!(shared_gate(&git_dir).try_lock().is_err());
        drop(permit);
        assert!(shared_gate(&git_dir).try_lock().is_ok());
    }

    #[test]
    fn latest_tag_phase_runs_once_after_obsolete_permit_is_released_without_rehydrating_commits() {
        let (_dir, repo, refs, local) = fixture();
        let gate = shared_gate(&crate::git::git_dir(&repo));
        let (ready, ready_receive) = std::sync::mpsc::channel();
        let (release, release_receive) = std::sync::mpsc::channel();
        let owner = std::thread::spawn(move || {
            let _permit = gate.lock().unwrap();
            ready.send(()).unwrap();
            release_receive
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
        });
        ready_receive
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let tip = "a".repeat(40);
        let (url, server) = super::super::tests::server(vec![
            (
                200,
                String::new(),
                serde_json::to_vec(&json!([wire(
                    Provider::Github,
                    &tip,
                    &[refs.head_id.unwrap().to_string()]
                )]))
                .unwrap(),
            ),
            (200, String::new(), b"[]".to_vec()),
        ]);
        let mut remote = Remote::parse(
            "origin",
            "https://example.invalid/team/repo",
            Some("github"),
        )
        .unwrap();
        remote.endpoint = format!("{url}/branches");
        let update = Update {
            remote,
            branches: vec![Branch {
                name: "main".into(),
                sha: tip,
            }],
            scoped: true,
            cached: false,
            stale: false,
        };
        let mut published = 0;
        let result = load_stream(
            &repo,
            &local,
            &refs,
            &[update],
            &[],
            true,
            true,
            &AtomicBool::new(false),
            Some(&mut |preview| {
                published += 1;
                assert_eq!(preview.ghosts.len(), 1);
                release.send(()).unwrap();
            }),
        );
        assert_eq!(published, 1);
        assert!(result.errors.is_empty());
        owner.join().unwrap();
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].contains("/commits?"));
        assert!(requests[1].contains("/tags?"));
    }
    #[test]
    fn commits_are_published_before_tag_requests() {
        let (_dir, repo, refs, local) = fixture();
        let tip = "a".repeat(40);
        let commits = json!([wire(
            Provider::Github,
            &tip,
            &[refs.head_id.unwrap().to_string()]
        )]);
        // No tag response is provided: publishing the graph must happen before tag I/O.
        let (url, server) = super::super::tests::server(vec![(
            200,
            String::new(),
            serde_json::to_vec(&commits).unwrap(),
        )]);
        let mut remote = Remote::parse(
            "origin",
            "https://example.invalid/team/repo",
            Some("github"),
        )
        .unwrap();
        remote.endpoint = format!("{url}/branches");
        let update = Update {
            remote,
            branches: vec![Branch {
                name: "main".into(),
                sha: tip,
            }],
            scoped: true,
            cached: false,
            stale: false,
        };
        let cancel = AtomicBool::new(false);
        let mut published = false;
        load_stream(
            &repo,
            &local,
            &refs,
            &[update],
            &[],
            false,
            true,
            &cancel,
            Some(&mut |preview| {
                assert_eq!(preview.ghosts.len(), 1);
                assert!(preview.tags.is_empty());
                published = true;
                cancel.store(true, Ordering::Relaxed);
            }),
        );
        assert!(published);
        assert_eq!(server.join().unwrap().len(), 1);
    }

    #[test]
    fn only_local_heads_and_their_upstreams_define_eligibility() {
        let (dir, _repo, mut refs, _) = fixture();
        command::checked(
            dir.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://example.invalid/team/repo",
            ],
        )
        .unwrap();
        command::checked(dir.path(), &["config", "branch.main.remote", "origin"]).unwrap();
        command::checked(
            dir.path(),
            &[
                "config",
                "branch.main.merge",
                "refs/heads/renamed-on-server",
            ],
        )
        .unwrap();
        refs.refs.push(crate::git::RefEntry {
            name: "origin/release".into(),
            id: refs.head_id.unwrap(),
            kind: RefKind::Remote,
        });
        let remote = Remote::parse(
            "origin",
            "https://example.invalid/team/repo",
            Some("gitlab"),
        )
        .unwrap();
        let names = eligible(dir.path(), &refs, &remote).unwrap();
        assert_eq!(names, HashSet::from(["renamed-on-server".into()]));
    }

    #[test]
    fn local_objects_are_not_ghosts_and_cancellation_is_non_destructive() {
        let (_dir, repo, refs, local) = fixture();
        let id = refs.head_id.unwrap();
        let mut remote = Remote::parse(
            "origin",
            "https://example.invalid/team/repo",
            Some("github"),
        )
        .unwrap();
        let (url, server) = super::super::tests::server(vec![(200, String::new(), b"[]".to_vec())]);
        remote.endpoint = format!("{url}/branches");
        let update = Update {
            remote,
            branches: vec![Branch {
                name: "main".into(),
                sha: id.to_string(),
            }],
            scoped: false,
            cached: false,
            stale: false,
        };
        // Object exists, even if it is absent from the displayed history.
        let preview = load(
            &repo,
            &History::default(),
            &refs,
            std::slice::from_ref(&update),
            false,
            true,
            &AtomicBool::new(false),
        );
        assert!(preview.errors.is_empty());
        assert!(preview.ghosts.is_empty());
        assert_eq!(preview.history.unwrap().commits[0].id, id);
        assert_eq!(server.join().unwrap().len(), 1); // tags only; no commit API request
        let cancelled = load(
            &repo,
            &local,
            &refs,
            &[update],
            false,
            true,
            &AtomicBool::new(true),
        );
        assert!(cancelled.history.is_none());
        assert!(cancelled.ghosts.is_empty());
    }

    #[test]
    fn ghost_becomes_local_after_objects_and_refs_arrive() {
        let (dir, repo, refs, local) = fixture();
        let tree = command::checked(dir.path(), &["rev-parse", "HEAD^{tree}"]).unwrap();
        let raw = format!(
            "tree {}\nparent {}\nauthor Test <test@example.invalid> 1792065600 +0000\ncommitter Test <test@example.invalid> 1792065600 +0000\n\nArrived from remote\n",
            String::from_utf8_lossy(&tree).trim(),
            refs.head_id.unwrap()
        );
        let hash = command::run(
            dir.path(),
            &["hash-object", "-t", "commit", "--stdin"],
            Some(raw.as_bytes()),
            Duration::from_secs(10),
        )
        .unwrap();
        assert!(hash.status.success());
        let sha = String::from_utf8_lossy(&hash.stdout).trim().to_string();
        let commits = serde_json::to_vec(&json!([wire(
            Provider::Github,
            &sha,
            &[refs.head_id.unwrap().to_string()]
        )]))
        .unwrap();
        let (url, server) = super::super::tests::server(vec![
            (200, String::new(), commits),
            (200, String::new(), b"[]".to_vec()),
            (200, String::new(), b"[]".to_vec()),
        ]);
        let mut remote = Remote::parse(
            "origin",
            "https://example.invalid/team/repo",
            Some("github"),
        )
        .unwrap();
        remote.endpoint = format!("{url}/branches");
        let update = Update {
            remote,
            branches: vec![Branch {
                name: "main".into(),
                sha: sha.clone(),
            }],
            scoped: false,
            cached: false,
            stale: false,
        };
        let cancel = AtomicBool::new(false);
        let preview = load(
            &repo,
            &local,
            &refs,
            std::slice::from_ref(&update),
            false,
            true,
            &cancel,
        );
        assert!(preview.errors.is_empty());
        assert_eq!(preview.ghosts.len(), 1);
        let wrote = command::run(
            dir.path(),
            &["hash-object", "-w", "-t", "commit", "--stdin"],
            Some(raw.as_bytes()),
            Duration::from_secs(10),
        )
        .unwrap();
        assert!(wrote.status.success());
        command::checked(dir.path(), &["update-ref", "refs/heads/main", &sha]).unwrap();
        let repo = crate::git::open(dir.path()).unwrap();
        let refs = crate::git::snapshot_refs(&repo).unwrap();
        let mut local = History::default();
        crate::git::walk(&repo, &refs.tips, &mut |h| {
            local.append(h);
            true
        })
        .unwrap();
        let preview = load(&repo, &local, &refs, &[update], false, true, &cancel);
        assert!(preview.errors.is_empty());
        assert!(preview.ghosts.is_empty());
        assert!(preview.history.is_none());
        assert_eq!(local.subject(0), "Arrived from remote");
        assert_eq!(server.join().unwrap().len(), 3); // no second commit request
    }

    #[test]
    fn merge_preserves_topology_despite_clock_skew_and_rejects_cycles() {
        let (_, _, refs, local) = fixture();
        let tip = "a".repeat(40);
        let wire = serde_json::to_vec(&json!([wire(
            Provider::Github,
            &tip,
            &[refs.head_id.unwrap().to_string()]
        )]))
        .unwrap();
        let mut d = parse_commits(Provider::Github, &wire).unwrap()[0]
            .details()
            .unwrap();
        d.commit_time = 1; // child older than parent
        let mut extra = HashMap::from([(d.id, d.clone())]);
        assert_eq!(merge(&local, &extra).unwrap().commits[0].id, d.id);
        let mut other = d.clone();
        other.id = oid(&"b".repeat(40)).unwrap();
        other.parents = vec![d.id];
        d.parents = vec![other.id];
        extra.insert(d.id, d);
        extra.insert(other.id, other);
        assert!(merge(&local, &extra).is_err());
    }

    /// Reproducible native-console fixture; no provider requests or real credentials.
    #[test]
    #[ignore = "writes a persistent fixture only when explicitly requested"]
    fn prepare_native_fixture() {
        let path = PathBuf::from(
            std::env::var_os("GIT_RUSTLE_GHOST_FIXTURE").expect("fixture directory required"),
        );
        std::fs::create_dir_all(&path).unwrap();
        command::checked(&path, &["init", "-q", "-b", "main"]).unwrap();
        command::checked(
            &path,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--allow-empty",
                "-qm",
                "Local base",
            ],
        )
        .unwrap();
        command::checked(
            &path,
            &[
                "config",
                "remote.origin.url",
                "https://ghost-fixture.invalid/team/repo",
            ],
        )
        .unwrap();
        command::checked(
            &path,
            &["config", "remote.origin.rustle-provider", "github"],
        )
        .unwrap();
        let repo = crate::git::open(&path).unwrap();
        let refs = crate::git::snapshot_refs(&repo).unwrap();
        let remote = Remote::parse(
            "origin",
            "https://ghost-fixture.invalid/team/repo",
            Some("github"),
        )
        .unwrap();
        let root = refs.head_id.unwrap().to_string();
        let tip = "a".repeat(40);
        let mid = "b".repeat(40);
        let metadata = parse_commits(
            Provider::Github,
            &serde_json::to_vec(&json!([
                wire(Provider::Github, &tip, std::slice::from_ref(&mid)),
                wire(Provider::Github, &mid, &[root]),
                wire(
                    Provider::Github,
                    &"f".repeat(40),
                    &[refs.head_id.unwrap().to_string()]
                )
            ]))
            .unwrap(),
        )
        .unwrap();
        let (file, identity) = cache_file(&crate::git::git_dir(&repo), &remote, None).unwrap();
        super::super::write_cache(
            &file,
            &Cache {
                version: 1,
                identity: identity.clone(),
                saved: now(),
                pages: vec![Page {
                    etag: None,
                    branches: vec![
                        Branch {
                            name: "main".into(),
                            sha: tip.clone(),
                        },
                        Branch {
                            name: "not-local".into(),
                            sha: "f".repeat(40),
                        },
                    ],
                    more: false,
                }],
            },
        );
        write_stored(
            &file.with_extension("history.json"),
            &Stored {
                version: 1,
                identity,
                commits: metadata,
                tags: vec![Branch {
                    name: "v-next".into(),
                    sha: tip,
                }],
                tags_saved: now(),
            },
        );
        super::super::files::seed_native_fixture(&repo, &remote, &"a".repeat(40));
        super::super::files::seed_native_fixture(&repo, &remote, &"f".repeat(40));
        println!("Fixture: {}", path.display());
    }

    #[test]
    #[ignore = "explicit large-repository performance measurement"]
    fn benchmark_overlay() {
        let path =
            PathBuf::from(std::env::var_os("GIT_RUSTLE_BENCH_REPO").expect("repository required"));
        let repo = crate::git::open(&path).unwrap();
        let refs = crate::git::snapshot_refs(&repo).unwrap();
        let mut local = History::default();
        crate::git::walk(&repo, &refs.tips, &mut |h| {
            local.append(h);
            true
        })
        .unwrap();
        for count in [100, 1000] {
            let mut extra = HashMap::new();
            let mut parent = refs.head_id.unwrap();
            for n in 1..=count {
                let sha = format!("{n:040x}");
                let mut d = parse_commits(
                    Provider::Github,
                    &serde_json::to_vec(&json!([wire(
                        Provider::Github,
                        &sha,
                        &[parent.to_string()]
                    )]))
                    .unwrap(),
                )
                .unwrap()
                .remove(0)
                .details()
                .unwrap();
                d.commit_time += n as i64;
                parent = d.id;
                extra.insert(d.id, d);
            }
            let mut samples = Vec::new();
            for n in 0..16 {
                let start = Instant::now();
                let merged = merge(&local, &extra).unwrap();
                let elapsed = start.elapsed().as_secs_f64() * 1000.0;
                assert_eq!(merged.len(), local.len() + count);
                if n > 0 {
                    samples.push(elapsed);
                }
            }
            let mut cached_samples = Vec::new();
            for n in 0..16 {
                let start = Instant::now();
                let merged = merge_cached(
                    &local,
                    &extra,
                    refs.fingerprint,
                    &crate::git::git_dir(&repo),
                    &["benchmark".into()],
                    false,
                )
                .unwrap();
                let elapsed = start.elapsed().as_secs_f64() * 1000.0;
                assert_eq!(merged.len(), local.len() + count);
                if n > 0 {
                    cached_samples.push(elapsed);
                }
            }
            let merged = merge(&local, &extra).unwrap();
            let mut dim_samples = Vec::new();
            let mut dim_bytes = 0;
            for n in 0..16 {
                let start = Instant::now();
                let flags = crate::graph::dimming::build(&merged, |id| extra.contains_key(id));
                let elapsed = start.elapsed().as_secs_f64() * 1000.0;
                assert!(flags.len() <= merged.cells.len());
                dim_bytes = flags.len();
                std::hint::black_box(&flags);
                if n > 0 {
                    dim_samples.push(elapsed);
                }
            }
            dim_samples.sort_by(f64::total_cmp);
            println!(
                "Edge dimming {count}: median {:.3} ms, p95 {:.3} ms; {} bytes; samples {:?}",
                dim_samples[7], dim_samples[14], dim_bytes, dim_samples
            );
            cached_samples.sort_by(f64::total_cmp);
            println!(
                "Cached overlay {count}: median {:.3} ms, p95 {:.3} ms; samples {:?}",
                cached_samples[7], cached_samples[14], cached_samples
            );
            samples.sort_by(f64::total_cmp);
            println!(
                "{} local + {count} ghost commits: median {:.3} ms, p95 {:.3} ms; samples {:?}",
                local.len(),
                samples[7],
                samples[14],
                samples
            );
        }
    }
}
