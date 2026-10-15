//! Read-only provider APIs. Loaded lazily, independently of the local graph.
use std::collections::{BTreeMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::git::command;
use serde::{Deserialize, Serialize};

const TTL: u64 = 300;
const MAX_PAGES: usize = 1000;
const MAX_CACHE: u64 = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Github,
    Gitlab,
}

#[derive(Clone, Debug)]
pub struct Remote {
    pub name: String,
    pub url: String,
    pub provider: Provider,
    pub endpoint: String,
    pub host: String,
    pub project: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    pub sha: String,
}

#[derive(Clone, Debug)]
pub struct Update {
    pub remote: Remote,
    pub branches: Vec<Branch>,
    pub cached: bool,
    pub stale: bool,
}

pub enum Event {
    Configured(Vec<(String, String)>),
    Branches(Update),
    Status(String),
    Done,
}

fn encode(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            use std::fmt::Write;
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

impl Remote {
    pub fn parse(name: &str, value: &str, provider: Option<&str>) -> Option<Self> {
        let normalized;
        let raw = if !value.contains("://") {
            let (host, path) = value.split_once(':')?;
            if host.contains(['/', '\\']) {
                return None;
            }
            normalized = format!("ssh://{host}/{path}");
            &normalized
        } else {
            value
        };
        let url = url::Url::parse(raw).ok()?;
        if !matches!(url.scheme(), "https" | "http" | "ssh" | "git") {
            return None;
        }
        let host = url.host_str()?.to_ascii_lowercase();
        let provider = match (provider, host.as_str()) {
            (Some("github"), _) | (None, "github.com") => Provider::Github,
            (Some("gitlab"), _) | (None, "gitlab.com") => Provider::Gitlab,
            _ => return None,
        };
        // Decode URL path before encoding it as API path segments (including subgroups).
        let project = percent_decode(url.path().trim_matches('/'))?;
        let project = project.strip_suffix(".git").unwrap_or(&project).to_string();
        let parts: Vec<_> = project.split('/').collect();
        if parts.len() < 2
            || parts
                .iter()
                .any(|p| p.is_empty() || *p == "." || *p == "..")
        {
            return None;
        }
        // Preserve HTTPS ports, but an SSH transport port is not an API port.
        let authority = match (url.scheme(), url.port()) {
            ("https" | "http", Some(port)) => format!("{host}:{port}"),
            _ => host.clone(),
        };
        let endpoint = match provider {
            Provider::Github if parts.len() != 2 => return None,
            Provider::Github => {
                let root = if host == "github.com" {
                    "https://api.github.com".to_string()
                } else {
                    format!("https://{authority}/api/v3")
                };
                format!(
                    "{root}/repos/{}/{}/branches",
                    encode(parts[0]),
                    encode(parts[1])
                )
            }
            Provider::Gitlab => format!(
                "https://{authority}/api/v4/projects/{}/repository/branches",
                encode(&project)
            ),
        };
        Some(Self {
            name: name.into(),
            url: value.into(),
            provider,
            endpoint,
            host,
            project,
        })
    }
}

fn percent_decode(s: &str) -> Option<String> {
    let mut bytes = Vec::new();
    let mut iter = s.bytes();
    while let Some(b) = iter.next() {
        if b == b'%' {
            let hi = (iter.next()? as char).to_digit(16)?;
            let lo = (iter.next()? as char).to_digit(16)?;
            bytes.push((hi * 16 + lo) as u8);
        } else {
            bytes.push(b);
        }
    }
    let result = String::from_utf8(bytes).ok()?;
    if result.chars().any(char::is_control) {
        None
    } else {
        Some(result)
    }
}

pub fn configured(path: &Path) -> Result<(Vec<Remote>, Vec<String>), String> {
    let out = command::run(
        path,
        &[
            "config",
            "--null",
            "--get-regexp",
            r"^remote\..*\.(url|rustle-provider)$",
        ],
        None,
        Duration::from_secs(10),
    )?;
    if !out.status.success() && out.status.code() != Some(1) {
        return Err("Could not read remote configuration".into());
    }
    let mut entries: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new();
    for entry in out.stdout.split(|b| *b == 0) {
        let Some((key, value)) = std::str::from_utf8(entry)
            .ok()
            .and_then(|s| s.split_once('\n'))
        else {
            continue;
        };
        let Some((name, field)) = key.strip_prefix("remote.").and_then(|s| s.rsplit_once('.'))
        else {
            continue;
        };
        let item = entries.entry(name.to_string()).or_default();
        match field {
            "url" if item.0.is_none() => item.0 = Some(value.to_string()),
            "rustle-provider" => item.1 = Some(value.to_string()),
            _ => {}
        }
    }
    let mut remotes = Vec::new();
    let mut unsupported = Vec::new();
    for (name, (value, provider)) in entries {
        if let Some(value) = value {
            match Remote::parse(&name, &value, provider.as_deref()) {
                Some(remote) => remotes.push(remote),
                None => unsupported.push(command::clean(&name)),
            }
        }
    }
    Ok((remotes, unsupported))
}

fn env_token(remote: &Remote) -> Option<String> {
    // Cloud tokens must never be sent to a host supplied by a repository.
    let names: &[&str] = match (remote.provider, remote.host.as_str()) {
        (Provider::Github, "github.com") => &["GH_TOKEN", "GITHUB_TOKEN"],
        (Provider::Gitlab, "gitlab.com") => &["GITLAB_TOKEN", "GLAB_TOKEN"],
        _ => &[],
    };
    names
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
}

fn credential(path: &Path, remote: &Remote) -> Option<String> {
    let endpoint = url::Url::parse(&remote.endpoint).ok()?;
    let authority = match endpoint.port() {
        Some(port) => format!("{}:{port}", remote.host),
        None => remote.host.clone(),
    };
    let credential_path = if remote.url.trim_end_matches('/').ends_with(".git") {
        format!("{}.git", remote.project)
    } else {
        remote.project.clone()
    };
    let input = format!("protocol=https\nhost={authority}\npath={credential_path}\n\n");
    let out = command::run(
        path,
        &["-c", "credential.interactive=false", "credential", "fill"],
        Some(input.as_bytes()),
        Duration::from_secs(10),
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout)
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("password=").map(str::to_string))
}

#[derive(Clone, Serialize, Deserialize)]
struct Page {
    etag: Option<String>,
    branches: Vec<Branch>,
    more: bool,
}

#[derive(Serialize, Deserialize)]
struct Cache {
    version: u32,
    identity: String,
    saved: u64,
    pages: Vec<Page>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn cache_file(git_dir: &Path, remote: &Remote, token: Option<&str>) -> Option<(PathBuf, String)> {
    let base = crate::cache::cache_path(git_dir)?.parent()?.join("remotes");
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (git_dir, &remote.name, &remote.url, &remote.endpoint, token).hash(&mut h);
    let identity = format!("{:016x}", h.finish());
    Some((base.join(format!("{identity}.json")), identity))
}

fn read_cache(path: &Path, identity: &str) -> Option<Cache> {
    if std::fs::metadata(path).ok()?.len() > MAX_CACHE {
        return None;
    }
    let cache: Cache = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if cache.version != 1
        || cache.identity != identity
        || cache.pages.is_empty()
        || cache.pages.len() > MAX_PAGES
        || cache.pages.iter().enumerate().any(|(i, p)| {
            p.branches.len() > 100
                || p.more != (i + 1 < cache.pages.len())
                || p.branches.iter().any(|b| !valid_branch(b))
        })
    {
        return None;
    }
    Some(cache)
}

fn write_cache(path: &Path, cache: &Cache) {
    let Some(parent) = path.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(bytes) = serde_json::to_vec(cache) else {
        return;
    };
    if bytes.len() as u64 > MAX_CACHE {
        return;
    }
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    if std::fs::write(&tmp, bytes).is_ok() {
        if std::fs::rename(&tmp, path).is_err() {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::rename(&tmp, path);
        }
        let _ = std::fs::remove_file(tmp);
    }
}

fn valid_branch(branch: &Branch) -> bool {
    !branch.name.is_empty()
        && branch.name.len() <= 1024
        && !branch.name.chars().any(char::is_control)
        && matches!(branch.sha.len(), 40 | 64)
        && branch.sha.bytes().all(|b| b.is_ascii_hexdigit())
}

struct Response {
    status: u16,
    etag: Option<String>,
    next: Option<bool>,
    body: Vec<u8>,
}

struct Http {
    agent: ureq::Agent,
}

impl Http {
    fn new() -> Self {
        Self {
            agent: ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(12)))
                .tls_config(
                    ureq::tls::TlsConfig::builder()
                        .provider(ureq::tls::TlsProvider::NativeTls)
                        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                        .build(),
                )
                // Do not forward tokens through redirects. Report moved projects instead.
                .max_redirects(0)
                .max_redirects_will_error(false)
                .http_status_as_error(false)
                .build()
                .into(),
        }
    }

    fn get(
        &self,
        remote: &Remote,
        page: usize,
        etag: Option<&str>,
        token: Option<&str>,
    ) -> Result<Response, String> {
        let url = format!("{}?per_page=100&page={page}", remote.endpoint);
        let mut req = self
            .agent
            .get(&url)
            .header("User-Agent", "git-rustle/0.1")
            .header("Accept", "application/json");
        if remote.provider == Provider::Github {
            req = req.header("X-GitHub-Api-Version", "2022-11-28");
        }
        if let Some(token) = token {
            // Both providers accept bearer PATs; GitLab OAuth credentials from
            // Git Credential Manager also require this form rather than PRIVATE-TOKEN.
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        if let Some(etag) = etag {
            req = req.header("If-None-Match", etag);
        }
        let mut response = req.call().map_err(|_| {
            "API connection failed or timed out; cached branches remain available".to_string()
        })?;
        let status = response.status().as_u16();
        let etag = response
            .headers()
            .get("etag")
            .and_then(|h| h.to_str().ok())
            .map(str::to_string);
        let next = match remote.provider {
            Provider::Gitlab => response
                .headers()
                .get("x-next-page")
                .and_then(|h| h.to_str().ok())
                .map(|s| !s.is_empty()),
            Provider::Github => response
                .headers()
                .get("link")
                .and_then(|h| h.to_str().ok())
                .map(|s| s.contains("rel=\"next\"")),
        };
        let body = if status == 200 {
            response
                .body_mut()
                .with_config()
                .limit(8 * 1024 * 1024)
                .read_to_vec()
                .map_err(|_| "Invalid or oversized API response".to_string())?
        } else {
            Vec::new()
        };
        Ok(Response {
            status,
            etag,
            next,
            body,
        })
    }
}

fn parse_page(
    provider: Provider,
    response: Response,
    previous: Option<&Page>,
) -> Result<Page, String> {
    if response.status == 304 {
        return previous
            .cloned()
            .ok_or("API returned 304 without a cached page".into());
    }
    if response.status != 200 {
        return Err(match response.status {
        401 | 404 => "API authentication required or repository not found; set GH_TOKEN / GITLAB_TOKEN or configure Git credentials".into(),
        403 | 429 => "API access denied or rate limited; cached branches remain available; retry later with F5".into(),
        301 | 302 | 307 | 308 => "Repository moved; update the remote URL in Git configuration".into(),
        status => format!("API returned HTTP {status}; cached branches remain available"),
    });
    }
    #[derive(Deserialize)]
    struct WireBranch {
        name: String,
        commit: WireCommit,
    }
    #[derive(Deserialize)]
    struct WireCommit {
        sha: Option<String>,
        id: Option<String>,
    }
    let wire: Vec<WireBranch> = serde_json::from_slice(&response.body)
        .map_err(|_| "Invalid branch API response".to_string())?;
    if wire.len() > 100 {
        return Err("API returned more branches than requested".into());
    }
    let mut branches = Vec::new();
    for b in wire {
        let sha = match provider {
            Provider::Github => b.commit.sha,
            Provider::Gitlab => b.commit.id,
        }
        .ok_or("API branch has no commit ID")?;
        let branch = Branch { name: b.name, sha };
        if !valid_branch(&branch) {
            return Err("API branch has an invalid name or commit ID".into());
        }
        branches.push(branch);
    }
    // Probe after a full page even if it currently has no `next` link. Otherwise
    // an unchanged full page (304) could hide newly appended branches on page N+1.
    let more = response.next.unwrap_or(false) || branches.len() == 100;
    Ok(Page {
        etag: response.etag,
        branches,
        more,
    })
}

fn pages(
    provider: Provider,
    cached: &[Page],
    mut get: impl FnMut(usize, Option<&str>) -> Result<Response, String>,
) -> Result<Vec<Page>, String> {
    let mut pages = Vec::new();
    let start = std::time::Instant::now();
    for n in 1..=MAX_PAGES {
        if start.elapsed() > Duration::from_secs(60) {
            return Err("Branch listing exceeded 60 seconds; cached branches retained".into());
        }
        let old = cached.get(n - 1);
        let response = get(n, old.and_then(|p| p.etag.as_deref()))?;
        let page = parse_page(provider, response, old)?;
        let more = page.more;
        pages.push(page);
        if !more {
            return Ok(pages);
        }
    }
    Err("Branch listing exceeded 100,000 branches; cached branches retained".into())
}

fn branches(pages: &[Page]) -> Vec<Branch> {
    let mut seen = HashSet::new();
    pages
        .iter()
        .flat_map(|p| &p.branches)
        .filter(|b| seen.insert(b.name.as_str()))
        .cloned()
        .collect()
}

fn load_one(
    path: &Path,
    git_dir: &Path,
    remote: &Remote,
    force: bool,
    no_cache: bool,
    emit: &(impl Fn(Event) + Sync),
) {
    let token = env_token(remote);
    let location = if no_cache {
        None
    } else {
        cache_file(git_dir, remote, token.as_deref())
    };
    load_cached(path, remote, force, location, token, emit);
}

fn load_cached(
    path: &Path,
    remote: &Remote,
    force: bool,
    location: Option<(PathBuf, String)>,
    mut token: Option<String>,
    emit: &(impl Fn(Event) + Sync),
) {
    let cached = location.as_ref().and_then(|(p, id)| read_cache(p, id));
    if let Some(cached) = &cached {
        let age = now().checked_sub(cached.saved);
        let fresh = age.is_some_and(|a| a < TTL);
        emit(Event::Branches(Update {
            remote: remote.clone(),
            branches: branches(&cached.pages),
            cached: true,
            stale: !fresh,
        }));
        if fresh && !force {
            return;
        }
    }
    let http = Http::new();
    let old = cached
        .as_ref()
        .map(|c| c.pages.as_slice())
        .unwrap_or_default();
    let mut used_helper = false;
    let result = pages(remote.provider, old, |n, etag| {
        let response = http.get(remote, n, etag, token.as_deref())?;
        if matches!(response.status, 401 | 403 | 404) && token.is_none() && !used_helper {
            used_helper = true;
            token = credential(path, remote);
            if token.is_some() {
                return http.get(remote, n, None, token.as_deref());
            }
        }
        Ok(response)
    });
    match result {
        Ok(pages) => {
            emit(Event::Branches(Update {
                remote: remote.clone(),
                branches: branches(&pages),
                cached: false,
                stale: false,
            }));
            // Helper credentials may change accounts. Do not persist their private data
            // into the anonymous cache; env-token caches are partitioned by token.
            if !used_helper && let Some((path, identity)) = location {
                write_cache(
                    &path,
                    &Cache {
                        version: 1,
                        identity,
                        saved: now(),
                        pages,
                    },
                );
            }
        }
        Err(error) => emit(Event::Status(format!(
            "{}: {error}",
            command::clean(&remote.name)
        ))),
    }
}

/// At most four providers concurrently, and only one scan is scheduled by the UI.
pub fn discover(
    path: &Path,
    git_dir: &Path,
    force: bool,
    no_cache: bool,
    emit: impl Fn(Event) + Sync,
) {
    match configured(path) {
        Ok((remotes, unsupported)) => {
            emit(Event::Configured(
                remotes
                    .iter()
                    .map(|r| (r.name.clone(), r.url.clone()))
                    .collect(),
            ));
            if !unsupported.is_empty() {
                emit(Event::Status(format!(
                    "Local tracking refs only: {} (provider not configured)",
                    unsupported.join(", ")
                )));
            }
            let next = std::sync::atomic::AtomicUsize::new(0);
            std::thread::scope(|scope| {
                for _ in 0..remotes.len().min(4) {
                    scope.spawn(|| {
                        loop {
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let Some(remote) = remotes.get(i) else { break };
                            load_one(path, git_dir, remote, force, no_cache, &emit);
                        }
                    });
                }
            });
        }
        Err(error) => emit(Event::Status(error)),
    }
    emit(Event::Done);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_and_nested_gitlab_projects() {
        let gh = Remote::parse("origin", "git@github.com:git/git.git", None).unwrap();
        assert_eq!(gh.endpoint, "https://api.github.com/repos/git/git/branches");
        let gl = Remote::parse(
            "upstream",
            "ssh://git@gitlab.com:2222/group/sub/repo.git",
            None,
        )
        .unwrap();
        assert_eq!(
            gl.endpoint,
            "https://gitlab.com/api/v4/projects/group%2Fsub%2Frepo/repository/branches"
        );
        let custom = Remote::parse(
            "origin",
            "https://code.example:8443/team/repo.git",
            Some("gitlab"),
        )
        .unwrap();
        assert!(
            custom
                .endpoint
                .starts_with("https://code.example:8443/api/v4/")
        );
        assert!(Remote::parse("origin", "https://github.com.evil.test/a/b", None).is_none());
        assert!(Remote::parse("origin", "C:\\repos\\x", None).is_none());
        assert!(Remote::parse("origin", "https://gitlab.com/a/%0Ab", None).is_none());
    }

    fn response(names: &[&str], next: bool) -> Response {
        Response {
            status: 200,
            etag: Some("etag".into()),
            next: Some(next),
            body: serde_json::to_vec(
                &names
                    .iter()
                    .map(|n| serde_json::json!({"name": n, "commit": {"sha": "a".repeat(40)}}))
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
        }
    }

    #[test]
    fn validates_each_page_even_when_first_is_unchanged() {
        let old = vec![
            parse_page(Provider::Github, response(&["main"], true), None).unwrap(),
            parse_page(Provider::Github, response(&["old"], false), None).unwrap(),
        ];
        let result = pages(Provider::Github, &old, |n, etag| {
            assert_eq!(etag, Some("etag"));
            Ok(if n == 1 {
                Response {
                    status: 304,
                    etag: None,
                    next: None,
                    body: Vec::new(),
                }
            } else {
                response(&["new"], false)
            })
        })
        .unwrap();
        assert_eq!(
            branches(&result)
                .iter()
                .map(|b| b.name.as_str())
                .collect::<Vec<_>>(),
            ["main", "new"]
        );
    }

    #[test]
    fn page_failure_does_not_publish_partial_list() {
        let result = pages(Provider::Github, &[], |n, _| {
            if n == 1 {
                Ok(response(&["main"], true))
            } else {
                Err("offline".into())
            }
        });
        assert!(result.is_err());
        assert!(parse_page(Provider::Github, response(&["bad\u{1b}name"], false), None).is_err());
        let gl = Response {
            status: 200,
            etag: None,
            next: None,
            body: serde_json::to_vec(
                &serde_json::json!([{"name":"topic/é", "commit":{"id":"b".repeat(40)}}]),
            )
            .unwrap(),
        };
        assert_eq!(
            parse_page(Provider::Gitlab, gl, None).unwrap().branches[0].name,
            "topic/é"
        );
    }

    #[test]
    fn full_last_page_is_probed_for_appended_branches() {
        let names: Vec<_> = (0..100).map(|n| format!("branch-{n}")).collect();
        let page = parse_page(
            Provider::Github,
            response(&names.iter().map(String::as_str).collect::<Vec<_>>(), false),
            None,
        )
        .unwrap();
        assert!(page.more);
        let old = vec![
            page,
            parse_page(Provider::Github, response(&[], false), None).unwrap(),
        ];
        let result = pages(Provider::Github, &old, |n, _| {
            Ok(if n == 1 {
                Response {
                    status: 304,
                    etag: None,
                    next: None,
                    body: Vec::new(),
                }
            } else {
                response(&["new-branch"], false)
            })
        })
        .unwrap();
        assert_eq!(branches(&result).len(), 101);
    }

    fn server(
        responses: Vec<(u16, String, Vec<u8>)>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, headers, body) in responses {
                let start = std::time::Instant::now();
                let mut stream = loop {
                    if let Ok((stream, _)) = listener.accept() {
                        break stream;
                    }
                    assert!(
                        start.elapsed() < Duration::from_secs(5),
                        "HTTP request did not arrive"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                requests.push(String::from_utf8(request).unwrap());
                write!(stream, "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n", body.len()).unwrap();
                stream.write_all(&body).unwrap();
            }
            requests
        });
        (url, handle)
    }

    #[test]
    fn http_cache_ttl_etag_and_failed_refresh_keep_complete_results() {
        let first = response(&["main"], false);
        let (url, server) = server(vec![
            (200, "ETag: \"v1\"\r\n".into(), first.body),
            (304, String::new(), Vec::new()),
            (503, String::new(), Vec::new()),
        ]);
        let mut remote = Remote::parse(
            "origin",
            "https://example.invalid/group/repo",
            Some("github"),
        )
        .unwrap();
        remote.endpoint = format!("{url}/branches");
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("branches.json");
        let location = Some((file.clone(), "test-id".into()));
        let events = std::sync::Mutex::new(Vec::new());
        let emit = |event| events.lock().unwrap().push(event);
        load_cached(dir.path(), &remote, false, location.clone(), None, &emit);
        assert!(matches!(
            events.lock().unwrap().last(),
            Some(Event::Branches(Update { cached: false, .. }))
        ));
        // This invocation must use the fresh cache and issue no HTTP request.
        load_cached(dir.path(), &remote, false, location.clone(), None, &emit);
        assert!(matches!(
            events.lock().unwrap().last(),
            Some(Event::Branches(Update {
                cached: true,
                stale: false,
                ..
            }))
        ));
        load_cached(dir.path(), &remote, true, location.clone(), None, &emit);
        let before = std::fs::read(&file).unwrap();
        load_cached(dir.path(), &remote, true, location, None, &emit);
        assert!(matches!(
            events.lock().unwrap().last(),
            Some(Event::Status(_))
        ));
        assert_eq!(std::fs::read(&file).unwrap(), before);
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(
            requests[1]
                .to_ascii_lowercase()
                .contains("if-none-match: \"v1\"")
        );
        assert!(requests[0].contains("per_page=100&page=1"));
        assert!(read_cache(&file, "different-remote").is_none());
        std::fs::write(&file, b"{corrupt").unwrap();
        assert!(read_cache(&file, "test-id").is_none());
    }

    #[test]
    fn gitlab_http_pagination_and_tokens_stay_on_original_host() {
        let body = serde_json::to_vec(
            &serde_json::json!([{"name":"main","commit":{"id":"a".repeat(40)}}]),
        )
        .unwrap();
        let (url, server) = server(vec![
            (200, "X-Next-Page: 2\r\n".into(), body.clone()),
            (200, "X-Next-Page: \r\n".into(), body),
            (
                302,
                "Location: https://elsewhere.invalid/\r\n".into(),
                Vec::new(),
            ),
        ]);
        let mut remote = Remote::parse(
            "origin",
            "https://example.invalid/group/repo",
            Some("gitlab"),
        )
        .unwrap();
        remote.endpoint = format!("{url}/branches");
        let http = Http::new();
        let result = pages(Provider::Gitlab, &[], |n, etag| {
            http.get(&remote, n, etag, Some("test-token"))
        })
        .unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(branches(&result).len(), 1); // API pagination can repeat a branch during updates.
        let redirect = http.get(&remote, 1, None, Some("test-token")).unwrap();
        assert_eq!(redirect.status, 302);
        let requests = server.join().unwrap();
        assert!(requests.iter().all(|r| {
            r.to_ascii_lowercase()
                .contains("authorization: bearer test-token")
        }));
    }
}
