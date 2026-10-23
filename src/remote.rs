//! Provider APIs. Loaded lazily, independently of the local graph.
use std::collections::{BTreeMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::git::command;
use serde::{Deserialize, Serialize};

mod advertisement;
mod avatars;
pub mod files;
mod heads;
pub mod history;
pub mod requests;

const TTL: u64 = 300;
const MAX_PAGES: usize = 1000;
const MAX_CACHE: u64 = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Github,
    Gitlab,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub url: String,
    pub provider: Provider,
    pub endpoint: String,
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
    pub scoped: bool,
    pub cached: bool,
    pub stale: bool,
}

pub enum Event {
    Configured(Vec<(String, String)>),
    Branches(Update),
    /// Cache-only graph seed; never overwrites fresher in-memory verified heads.
    HeadsCached {
        update: Update,
        scope: HashSet<String>,
    },
    /// A bounded batch of verified heads; omitted identities are still pending.
    #[cfg(test)]
    BranchesPartial {
        update: Update,
        scope: HashSet<String>,
    },
    HeadsVerified {
        update: Update,
        scope: HashSet<String>,
    },
    Status(String),
    Done,
}

pub fn graph_names(refs: &crate::git::RefSnapshot, remote: &Remote) -> HashSet<String> {
    use crate::git::RefKind;
    let mut names = HashSet::new();
    let prefix = format!("{}/", remote.name);
    for reference in &refs.refs {
        match reference.kind {
            RefKind::Remote => {
                if let Some(name) = reference.name.strip_prefix(&prefix)
                    && name != "HEAD"
                {
                    names.insert(name.to_owned());
                }
            }
            RefKind::Branch | RefKind::Head
                if reference.kind == RefKind::Branch
                    || refs.head_branch.as_ref() == Some(&reference.name) =>
            {
                match refs.upstreams.get(&reference.name) {
                    Some((upstream, name)) if upstream == &remote.name => {
                        names.insert(name.clone());
                    }
                    None => {
                        names.insert(reference.name.clone());
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    names
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
        Self::parse_options(name, value, provider, None, None)
    }

    fn parse_options(
        name: &str,
        value: &str,
        provider: Option<&str>,
        api_url: Option<&str>,
        project_override: Option<&str>,
    ) -> Option<Self> {
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
        let project = project_override
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| project.strip_suffix(".git").unwrap_or(&project))
            .to_string();
        if project.chars().any(char::is_control) {
            return None;
        }
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
        let root = api_url
            .filter(|s| !s.is_empty())
            .map(|s| api_base(provider, s))
            .transpose()
            .ok()?;
        let endpoint = match provider {
            Provider::Github if parts.len() != 2 => return None,
            Provider::Github => {
                let root = root.unwrap_or_else(|| {
                    if host == "github.com" {
                        "https://api.github.com".to_string()
                    } else {
                        format!("https://{authority}/api/v3")
                    }
                });
                format!(
                    "{root}/repos/{}/{}/branches",
                    encode(parts[0]),
                    encode(parts[1])
                )
            }
            Provider::Gitlab => format!(
                "{}/projects/{}/repository/branches",
                root.unwrap_or_else(|| format!("https://{authority}/api/v4")),
                encode(&project)
            ),
        };
        Some(Self {
            name: name.into(),
            url: value.into(),
            provider,
            endpoint,
            project,
        })
    }
}

/// Accept either a server URL (including an installation prefix) or an API base.
pub fn api_base(provider: Provider, value: &str) -> Result<String, String> {
    let url = url::Url::parse(value.trim())
        .map_err(|_| "Enter a valid HTTPS server or API URL".to_string())?;
    // The private-desktop diagnostic owns this loopback server. Keep production
    // configuration HTTPS-only, including every non-diagnostic launch.
    let diagnostic_loopback = std::env::var("GIT_RUSTLE_SCENARIO").as_deref()
        == Ok("refresh-native")
        && url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.port().is_some()
        && url.path() == "/";
    if (url.scheme() != "https" && !diagnostic_loopback)
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Use an HTTPS URL without credentials, a query, or a fragment".into());
    }
    let base = url.as_str().trim_end_matches('/');
    if diagnostic_loopback {
        return Ok(base.into());
    }
    if provider == Provider::Github
        && matches!(base, "https://github.com" | "https://api.github.com")
    {
        return Ok("https://api.github.com".into());
    }
    let suffix = match provider {
        Provider::Github => "/api/v3",
        Provider::Gitlab => "/api/v4",
    };
    Ok(if base.ends_with(suffix) {
        base.into()
    } else {
        format!("{base}{suffix}")
    })
}

#[derive(Clone, Default)]
pub struct Configuration {
    pub name: String,
    pub url: String,
    pub provider: Option<String>,
    pub api_url: Option<String>,
    pub project: Option<String>,
}

impl Configuration {
    pub fn remote(&self) -> Result<Remote, String> {
        if let Some(api) = &self.api_url {
            let provider = if self.provider.as_deref() == Some("github") {
                Provider::Github
            } else {
                Provider::Gitlab
            };
            api_base(provider, api)?;
        }
        Remote::parse_options(
            &self.name,
            &self.url,
            self.provider.as_deref(),
            self.api_url.as_deref(),
            self.project.as_deref(),
        )
        .ok_or_else(|| {
            "Check the provider, remote URL, and project path (group/project or owner/repository)"
                .into()
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

type Upstreams = BTreeMap<String, (Option<String>, Option<String>)>;

pub fn configurations(path: &Path) -> Result<Vec<Configuration>, String> {
    configuration_data(path).map(|(config, _)| config)
}

fn configuration_data(path: &Path) -> Result<(Vec<Configuration>, Upstreams), String> {
    let out = command::run(
        path,
        &[
            "config",
            "--null",
            "--get-regexp",
            r"^(remote\..*\.(url|rustle-provider|rustle-api-url|rustle-project)|branch\..*\.(remote|merge))$",
        ],
        None,
        Duration::from_secs(10),
    )?;
    if !out.status.success() && out.status.code() != Some(1) {
        return Err("Could not read remote configuration".into());
    }
    let mut entries: BTreeMap<String, Configuration> = BTreeMap::new();
    let mut upstreams = Upstreams::new();
    for entry in out.stdout.split(|b| *b == 0) {
        let Some((key, value)) = std::str::from_utf8(entry)
            .ok()
            .and_then(|s| s.split_once('\n'))
        else {
            continue;
        };
        if let Some((name, field)) = key.strip_prefix("branch.").and_then(|s| s.rsplit_once('.')) {
            let entry = upstreams.entry(name.to_string()).or_default();
            match field {
                "remote" => entry.0 = Some(value.to_string()),
                "merge" => entry.1 = Some(value.to_string()),
                _ => {}
            }
            continue;
        }
        let Some((name, field)) = key.strip_prefix("remote.").and_then(|s| s.rsplit_once('.'))
        else {
            continue;
        };
        let item = entries.entry(name.to_string()).or_default();
        item.name = name.to_string();
        match field {
            "url" if item.url.is_empty() => item.url = value.to_string(),
            "rustle-provider" => item.provider = (!value.is_empty()).then(|| value.to_string()),
            "rustle-api-url" => item.api_url = (!value.is_empty()).then(|| value.to_string()),
            "rustle-project" => item.project = (!value.is_empty()).then(|| value.to_string()),
            _ => {}
        }
    }
    Ok((
        entries
            .into_values()
            .filter(|entry| !entry.url.is_empty())
            .collect(),
        upstreams,
    ))
}

pub(crate) fn configured(path: &Path) -> Result<(Vec<Remote>, Vec<String>, Upstreams), String> {
    let (configurations, upstreams) = configuration_data(path)?;
    let providers = crate::provider_settings::load()?;
    let mut remotes = Vec::new();
    let mut unsupported = Vec::new();
    for mut config in configurations {
        providers.apply(&mut config);
        match config.remote() {
            Ok(remote) => remotes.push(remote),
            Err(_) => unsupported.push(command::clean(&config.name)),
        }
    }
    Ok((remotes, unsupported, upstreams))
}

fn env_token(remote: &Remote) -> Option<String> {
    // Cloud tokens must never be sent to a host supplied by a repository.
    let origin = url::Url::parse(&remote.endpoint)
        .ok()?
        .origin()
        .ascii_serialization();
    let names: &[&str] = match (remote.provider, origin.as_str()) {
        (Provider::Github, "https://api.github.com") => &["GH_TOKEN", "GITHUB_TOKEN"],
        (Provider::Gitlab, "https://gitlab.com") => &["GITLAB_TOKEN", "GLAB_TOKEN"],
        _ => &[],
    };
    names
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
}

fn credential(path: &Path, remote: &Remote) -> Option<String> {
    let endpoint = url::Url::parse(&remote.endpoint).ok()?;
    let host = if endpoint.host_str()? == "api.github.com" {
        "github.com"
    } else {
        endpoint.host_str()?
    };
    let authority = match endpoint.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
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

// Helper-authenticated data uses the same credential-partitioned caches as saved tokens.
// The marker contains no credential: it only avoids another anonymous request next launch.
type HelperKey = (PathBuf, String);
static HELPER_TOKENS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<HelperKey, String>>,
> = std::sync::LazyLock::new(Default::default);

pub fn clear_auth_cache() {
    HELPER_TOKENS.lock().unwrap().clear();
}

fn helper_token(path: &Path, remote: &Remote) -> Option<String> {
    let key = (path.to_path_buf(), remote.endpoint.clone());
    if let Some(token) = HELPER_TOKENS.lock().unwrap().get(&key).cloned() {
        return Some(token);
    }
    let token = credential(path, remote)?;
    HELPER_TOKENS.lock().unwrap().insert(key, token.clone());
    if let Ok(repo) = crate::git::open(path)
        && let Some((file, _)) = cache_file(&crate::git::git_dir(&repo), remote, None)
    {
        let _ = std::fs::create_dir_all(file.parent().unwrap());
        let _ = std::fs::write(file.with_extension("helper"), b"1");
    }
    Some(token)
}

fn cached_token(path: &Path, git_dir: &Path, remote: &Remote) -> Result<Option<String>, String> {
    if let Some(token) = crate::provider_settings::token(remote)?
        .or(crate::credentials::read(&remote.endpoint)?)
        .or_else(|| env_token(remote))
    {
        return Ok(Some(token));
    }
    let key = (path.to_path_buf(), remote.endpoint.clone());
    if let Some(token) = HELPER_TOKENS.lock().unwrap().get(&key).cloned() {
        return Ok(Some(token));
    }
    Ok(
        if cache_file(git_dir, remote, None)
            .is_some_and(|(p, _)| p.with_extension("helper").exists())
        {
            helper_token(path, remote)
        } else {
            None
        },
    )
}

/// Tags can refer to trees/blobs, and GitLab may omit a peeled commit for those.
/// A tag without a valid commit target cannot label a commit graph; skip it alone.
fn parse_tags(provider: Provider, response: Response) -> Result<Page, String> {
    if response.status != 200 {
        return parse_page(provider, response, None);
    }
    let values: Vec<serde_json::Value> =
        serde_json::from_slice(&response.body).map_err(|_| "Invalid tags API response")?;
    if values.len() > 100 {
        return Err("Oversized tags API page".into());
    }
    let more = response.next.unwrap_or(values.len() == 100);
    let branches = values
        .iter()
        .filter_map(|v| {
            let name = v.get("name")?.as_str()?.to_string();
            let key = if provider == Provider::Github {
                "sha"
            } else {
                "id"
            };
            let sha = v.get("commit")?.get(key)?.as_str()?.to_string();
            let tag = Branch { name, sha };
            valid_branch(&tag).then_some(tag)
        })
        .collect();
    Ok(Page {
        etag: response.etag,
        branches,
        more,
    })
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
        self.get_url(remote, &url, etag, token)
    }

    fn get_url(
        &self,
        remote: &Remote,
        url: &str,
        etag: Option<&str>,
        token: Option<&str>,
    ) -> Result<Response, String> {
        self.get_url_accept(remote, url, etag, token, "application/json")
    }

    fn get_url_accept(
        &self,
        remote: &Remote,
        url: &str,
        etag: Option<&str>,
        token: Option<&str>,
        accept: &str,
    ) -> Result<Response, String> {
        let mut req = self
            .agent
            .get(url)
            .header("User-Agent", "git-rustle/0.1")
            .header("Accept", accept);
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
        let mut response = req.call().map_err(|error| {
            match error {
                ureq::Error::HostNotFound => "DNS lookup failed; check the server URL and VPN",
                ureq::Error::Timeout(_) => "API request timed out; check VPN, firewall, and proxy",
                ureq::Error::NativeTls(_) | ureq::Error::Tls(_) => "TLS connection failed; check the server certificate and Windows certificate trust",
                ureq::Error::ConnectProxyFailed(_) | ureq::Error::InvalidProxyUrl => "Proxy connection failed; check HTTPS_PROXY configuration",
                _ => "API connection failed; check the server URL, network, VPN, and proxy",
            }.to_string()
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
        403 | 429 => "API access denied or rate limited; use Refresh to retry later".into(),
        301 | 302 | 307 | 308 => "Repository moved; update the remote URL in Git configuration".into(),
        500..=599 => format!("Remote server temporarily unavailable (HTTP {}); use Refresh to retry", response.status),
        status => format!("API returned HTTP {status}; use Refresh to retry"),
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
    let token = match cached_token(path, git_dir, remote) {
        Ok(token) => token,
        Err(error) => {
            emit(Event::Status(error));
            env_token(remote)
        }
    };
    let location = if no_cache {
        None
    } else {
        cache_file(git_dir, remote, token.as_deref())
    };
    load_cached(path, remote, force, location, token, emit);
}

pub fn test_connection(
    path: &Path,
    remote: &Remote,
    replacement: Option<&str>,
    forget: bool,
) -> Result<String, String> {
    let saved = if forget || replacement.is_some() {
        None
    } else {
        crate::provider_settings::token(remote)?.or(crate::credentials::read(&remote.endpoint)?)
    };
    let mut token = replacement
        .map(str::to_string)
        .or(saved)
        .or_else(|| env_token(remote));
    let http = Http::new();
    let mut response = http.get(remote, 1, None, token.as_deref())?;
    if matches!(response.status, 401 | 403 | 404) && token.is_none() {
        token = helper_token(path, remote);
        if token.is_some() {
            response = http.get(remote, 1, None, token.as_deref())?;
        }
    }
    let page = parse_page(remote.provider, response, None)?;
    Ok(format!(
        "Connected: {} branches on the first page{}",
        page.branches.len(),
        if page.more { " (more available)" } else { "" }
    ))
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
            scoped: false,
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
            token = helper_token(path, remote);
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
                scoped: false,
                cached: false,
                stale: false,
            }));
            // Partition helper data by the resolved token, including when accounts change.
            let location = if used_helper && location.is_some() {
                crate::git::open(path).ok().and_then(|repo| {
                    cache_file(&crate::git::git_dir(&repo), remote, token.as_deref())
                })
            } else {
                location
            };
            if let Some((path, identity)) = location {
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

/// Read one eligible graph head; the full server branch list is lazy (b).
#[allow(clippy::too_many_arguments)]
fn read_head(
    path: &Path,
    remote: &Remote,
    name: &str,
    http: &Http,
    token: &mut Option<String>,
) -> Result<Option<Branch>, String> {
    let url = format!("{}/{}", remote.endpoint, encode(name));
    let mut response = http.get_url(remote, &url, None, token.as_deref())?;
    if matches!(response.status, 401 | 403 | 404) && token.is_none() {
        *token = helper_token(path, remote);
        if token.is_some() {
            response = http.get_url(remote, &url, None, token.as_deref())?;
        }
    }
    if response.status == 404 {
        // Distinguish a missing branch from an inaccessible private repository.
        let root = remote.endpoint.strip_suffix("/branches").unwrap();
        let root = if remote.provider == Provider::Gitlab {
            root.strip_suffix("/repository").unwrap_or(root)
        } else {
            root
        };
        let access = http.get_url(remote, root, None, token.as_deref())?;
        if access.status != 200 {
            return Err(parse_page(remote.provider, access, None)
                .err()
                .unwrap_or_else(|| "Repository unavailable".into()));
        }
        return Ok(None);
    }
    if response.status == 200 {
        let value: serde_json::Value =
            serde_json::from_slice(&response.body).map_err(|_| "Invalid branch API response")?;
        response.body = serde_json::to_vec(&vec![value]).unwrap();
    }
    Ok(parse_page(remote.provider, response, None)?
        .branches
        .into_iter()
        .find(|b| b.name == name))
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn load_heads(
    path: &Path,
    git_dir: &Path,
    remote: &Remote,
    force: bool,
    no_cache: bool,
    emit: &(impl Fn(Event) + Sync),
    upstreams: &Upstreams,
) -> Result<(), String> {
    load_heads_scoped(
        path,
        git_dir,
        remote,
        force,
        no_cache,
        emit,
        upstreams,
        &[],
        &std::sync::atomic::AtomicBool::new(false),
    )
}
#[allow(clippy::too_many_arguments)]
fn load_heads_scoped(
    path: &Path,
    git_dir: &Path,
    remote: &Remote,
    force: bool,
    no_cache: bool,
    emit: &(impl Fn(Event) + Sync),
    upstreams: &Upstreams,
    requested: &[crate::branches::Target],
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<(), String> {
    let mut names = history::eligible_from_config(path, remote, upstreams)?;
    let priority: HashSet<_> = requested
        .iter()
        .filter(|t| {
            t.remote.as_ref() == Some(&remote.name)
                && t.url.as_ref().is_none_or(|u| u == &remote.url)
        })
        .map(|t| t.name.as_str())
        .collect();
    names.extend(priority.iter().map(|s| s.to_string()));
    let mut token = cached_token(path, git_dir, remote)?;
    let mut sorted: Vec<_> = names.iter().collect();
    sorted.sort();
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    sorted.hash(&mut hash);
    let extension = format!("{:016x}.heads.json", hash.finish());
    sorted.sort_by(|a, b| {
        (!priority.contains(a.as_str()), a.as_str())
            .cmp(&(!priority.contains(b.as_str()), b.as_str()))
    });
    let location = if no_cache {
        None
    } else {
        cache_file(git_dir, remote, token.as_deref())
    };
    let cached = location.as_ref().and_then(|(p, id)| {
        read_cache(&p.with_extension(&extension), id).or_else(|| read_cache(p, id))
    });
    if let Some(cached) = &cached {
        let fresh = now().saturating_sub(cached.saved) < TTL;
        emit(Event::HeadsCached {
            update: Update {
                remote: remote.clone(),
                branches: branches(&cached.pages)
                    .into_iter()
                    .filter(|b| names.contains(&b.name))
                    .collect(),
                scoped: true,
                cached: true,
                stale: !fresh,
            },
            scope: names.clone(),
        });
        if fresh && !force {
            return Ok(());
        }
    }
    let mut found = Vec::new();
    let mut verified = HashSet::new();
    // Individual endpoints are cheap for a handful of heads, but thousands of
    // tracking refs turn that into minutes of round trips. Enumerate in pages
    // above this crossover, publishing only identities in the graph's scope.
    let bulk = names.len() >= heads::BULK_THRESHOLD;
    // Bound API fanout independently of branch count. Publish completed batches
    // while slow branches are still being checked; no Git objects/refs are fetched.
    let next = std::sync::atomic::AtomicUsize::new(0);
    let (send, receive) = std::sync::mpsc::channel();
    let initial_token = token.clone();
    let started = std::time::Instant::now();
    let advertised = (names.len() >= advertisement::MIN_HEADS)
        .then(|| advertisement::read(remote, token.as_deref(), &names, cancel))
        .flatten()
        .or_else(|| {
            bulk.then(|| heads::advertise(path, remote, &names, cancel))
                .flatten()
        });
    let failure = if let Some(branches) = advertised {
        found = branches;
        verified = names.clone();
        None
    } else if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        Some("Remote head check cancelled".into())
    } else if bulk {
        heads::load(
            path,
            remote,
            &names,
            &mut token,
            cancel,
            &mut found,
            &mut verified,
            emit,
        )
        .err()
    } else {
        std::thread::scope(|scope| {
            for _ in 0..sorted.len().min(4) {
                let send = send.clone();
                let sorted = &sorted;
                let next = &next;
                let initial_token = &initial_token;
                scope.spawn(move || {
                let http = Http::new();
                let mut worker_token = initial_token.clone();
                loop {
                    if cancel.load(std::sync::atomic::Ordering::Relaxed) { break; }
                    if started.elapsed() >= Duration::from_secs(30) {
                        let _=send.send((String::new(),Err("Remote head verification time budget reached; cached heads retained".into()),worker_token.clone()));
                        break;
                    }
                    let index = next.fetch_add(1,std::sync::atomic::Ordering::Relaxed);
                    let Some(name) = sorted.get(index) else { break; };
                    let result = read_head(path, remote, name, &http, &mut worker_token);
                    if send.send(((*name).clone(),result,worker_token.clone())).is_err() { break; }
                }
            });
            }
            drop(send);
            let mut first_error = None;
            let mut completed = 0;
            let mut last_publish = std::time::Instant::now();
            for (name, result, resolved_token) in receive {
                completed += 1;
                if resolved_token.is_some() {
                    token = resolved_token;
                }
                match result {
                    Ok(Some(branch)) => {
                        verified.insert(name);
                        found.push(branch);
                    }
                    Ok(None) => {
                        verified.insert(name);
                    }
                    Err(error) => {
                        first_error.get_or_insert(error);
                    }
                }
                // Throttle partial publication. The engine retains unchecked in-memory
                // heads until the full set succeeds; this payload contains verified heads only.
                if (completed == 1 || last_publish.elapsed() >= Duration::from_millis(80))
                    && completed < sorted.len()
                    && !cancel.load(std::sync::atomic::Ordering::Relaxed)
                {
                    last_publish = std::time::Instant::now();
                    // Only verified heads belong in a network batch. Cache-only
                    // rows were seeded separately and must not overwrite newer memory.
                    let mut partial = found.clone();
                    partial.sort_by(|a, b| a.name.cmp(&b.name));
                    emit(Event::HeadsVerified {
                        update: Update {
                            remote: remote.clone(),
                            branches: partial,
                            scoped: true,
                            cached: true,
                            stale: true,
                        },
                        scope: verified.clone(),
                    });
                }
            }
            first_error
        })
    };
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Err("Remote head check cancelled".into());
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    emit(Event::HeadsVerified {
        update: Update {
            remote: remote.clone(),
            branches: found.clone(),
            scoped: true,
            cached: false,
            stale: failure.is_some(),
        },
        scope: verified,
    });
    if let Some(error) = failure {
        return Err(error);
    }
    if !no_cache && let Some((path, identity)) = cache_file(git_dir, remote, token.as_deref()) {
        let mut pages: Vec<_> = found
            .chunks(100)
            .map(|b| Page {
                etag: None,
                branches: b.to_vec(),
                more: true,
            })
            .collect();
        if let Some(last) = pages.last_mut() {
            last.more = false;
        } else {
            pages.push(Page {
                etag: None,
                branches: Vec::new(),
                more: false,
            });
        }
        write_cache(
            &path.with_extension(extension),
            &Cache {
                version: 1,
                identity,
                saved: now(),
                pages,
            },
        );
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscoveryScope {
    Graph,
    All,
}

/// At most four providers concurrently, and only one scan is scheduled by the UI.
#[allow(clippy::too_many_arguments)]
pub fn discover_scoped(
    path: &Path,
    git_dir: &Path,
    force: bool,
    no_cache: bool,
    discovery_scope: DiscoveryScope,
    refs: Option<&crate::git::RefSnapshot>,
    requested: &[crate::branches::Target],
    cancel: &std::sync::atomic::AtomicBool,
    emit: impl Fn(Event) + Sync,
) {
    match configured(path) {
        Ok((remotes, unsupported, upstreams)) => {
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
            let mut requested = requested.to_vec();
            if discovery_scope == DiscoveryScope::Graph
                && let Some(refs) = refs
            {
                for remote in &remotes {
                    let prefix = format!("{}/", remote.name);
                    for name in refs
                        .refs
                        .iter()
                        .filter(|r| r.kind == crate::git::RefKind::Remote)
                        .filter_map(|r| r.name.strip_prefix(&prefix))
                        .filter(|name| *name != "HEAD")
                    {
                        requested.push(crate::branches::Target {
                            name: name.into(),
                            remote: Some(remote.name.clone()),
                            url: Some(remote.url.clone()),
                            sha: String::new(),
                        });
                    }
                }
            }
            let requested = &requested;
            let next = std::sync::atomic::AtomicUsize::new(0);
            std::thread::scope(|scope| {
                for _ in 0..remotes.len().min(4) {
                    scope.spawn(|| {
                        loop {
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let Some(remote) = remotes.get(i) else { break };
                            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                                break;
                            }
                            if discovery_scope != DiscoveryScope::All {
                                if let Err(error) = load_heads_scoped(
                                    path, git_dir, remote, force, no_cache, &emit, &upstreams,
                                    requested, cancel,
                                ) {
                                    emit(Event::Status(format!(
                                        "{}: {error}",
                                        command::clean(&remote.name)
                                    )));
                                }
                            } else {
                                load_one(path, git_dir, remote, force, no_cache, &emit);
                            }
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
    fn graph_scope_includes_exact_upstreams_tracking_refs_and_excludes_browser_only_heads() {
        use crate::git::{RefEntry, RefKind, RefSnapshot};
        let remote = Remote::parse(
            "origin",
            "https://example.invalid/team/repo",
            Some("github"),
        )
        .unwrap();
        let id = gix::ObjectId::from_hex("a".repeat(40).as_bytes()).unwrap();
        let mut refs = RefSnapshot::default();
        for (name, kind) in [
            ("local", RefKind::Branch),
            ("fallback", RefKind::Branch),
            ("on-other", RefKind::Branch),
            ("local-upstream", RefKind::Branch),
            ("origin/fetched-only", RefKind::Remote),
            ("origin/HEAD", RefKind::Remote),
            ("other/unrelated", RefKind::Remote),
        ] {
            refs.refs.push(RefEntry {
                name: name.into(),
                id,
                kind,
            });
        }
        refs.upstreams
            .insert("local".into(), ("origin".into(), "renamed/server".into()));
        refs.upstreams
            .insert("on-other".into(), ("other".into(), "other-name".into()));
        refs.upstreams
            .insert("local-upstream".into(), (".".into(), "local".into()));
        assert_eq!(
            graph_names(&refs, &remote),
            [
                "renamed/server".into(),
                "fallback".into(),
                "fetched-only".into()
            ]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn scoped_checks_publish_successes_and_deletions_even_when_another_head_fails() {
        let dir = tempfile::tempdir().unwrap();
        command::checked(dir.path(), &["init", "-q"]).unwrap();
        let repo = crate::git::open(dir.path()).unwrap();
        let (url, server) = server(vec![
            (
                200,
                String::new(),
                br#"{"name":"ok","commit":{"sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}"#
                    .to_vec(),
            ),
            (503, String::new(), b"{}".to_vec()),
        ]);
        let mut remote = Remote::parse(
            "origin",
            "https://example.invalid/team/repo",
            Some("github"),
        )
        .unwrap();
        remote.endpoint = format!("{url}/branches");
        let targets: Vec<_> = ["ok", "failed"]
            .into_iter()
            .map(|name| crate::branches::Target {
                name: name.into(),
                remote: Some("origin".into()),
                url: Some(remote.url.clone()),
                sha: String::new(),
            })
            .collect();
        let events = std::sync::Mutex::new(Vec::new());
        let result = load_heads_scoped(
            dir.path(),
            &crate::git::git_dir(&repo),
            &remote,
            true,
            true,
            &|event| events.lock().unwrap().push(event),
            &Upstreams::new(),
            &targets,
            &std::sync::atomic::AtomicBool::new(false),
        );
        assert!(result.is_err());
        assert_eq!(server.join().unwrap().len(), 2);
        assert!(events.into_inner().unwrap().into_iter().any(|event| matches!(event,Event::HeadsVerified{scope,update} if scope.len()==1 && update.stale)));
    }

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
    fn explicit_api_handles_ssh_alias_prefix_and_port() {
        let config = Configuration {
            name: "origin".into(),
            url: "git@work-alias:team/project.git".into(),
            provider: Some("gitlab".into()),
            api_url: Some("https://code.example:8443/gitlab/".into()),
            project: Some("team/subgroup/project".into()),
        };
        let remote = config.remote().unwrap();
        assert_eq!(
            remote.endpoint,
            "https://code.example:8443/gitlab/api/v4/projects/team%2Fsubgroup%2Fproject/repository/branches"
        );
        assert!(env_token(&remote).is_none());
        assert_eq!(
            api_base(Provider::Gitlab, "https://code.example/gitlab/api/v4/").unwrap(),
            "https://code.example/gitlab/api/v4"
        );
        assert_eq!(
            api_base(Provider::Github, "https://github.com/").unwrap(),
            "https://api.github.com"
        );
        assert_eq!(
            api_base(Provider::Github, "https://code.example:8443/").unwrap(),
            "https://code.example:8443/api/v3"
        );
        for invalid in [
            "http://code.example",
            "https://user:secret@code.example",
            "https://code.example/?token=secret",
            "https://code.example/#token",
        ] {
            assert!(api_base(Provider::Gitlab, invalid).is_err());
        }
    }

    #[test]
    fn connection_test_checks_one_page_without_saving_or_fetching() {
        let body = serde_json::to_vec(
            &serde_json::json!([{"name":"main", "commit":{"id":"a".repeat(40)}}]),
        )
        .unwrap();
        let (url, server) = server(vec![(200, "X-Next-Page: 2\r\n".into(), body)]);
        let mut remote =
            Remote::parse("origin", "git@work-alias:team/project.git", Some("gitlab")).unwrap();
        remote.endpoint = format!("{url}/api/v4/projects/team%2Fproject/repository/branches");
        let dir = tempfile::tempdir().unwrap();
        let result = test_connection(dir.path(), &remote, Some("synthetic-token"), false).unwrap();
        assert!(result.contains("more available"));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0]
                .to_lowercase()
                .contains("authorization: bearer synthetic-token")
        );
        assert!(requests[0].contains("page=1"));
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

    pub(super) fn server(
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
                stream.set_nonblocking(false).unwrap();
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
    fn api_failures_explain_retry_without_claiming_endpoint_specific_cache() {
        for provider in [Provider::Github, Provider::Gitlab] {
            for status in [403, 429, 500, 502, 503, 504] {
                let response = || Response {
                    status,
                    etag: None,
                    next: None,
                    body: Vec::new(),
                };
                let branch_error = parse_page(provider, response(), None).err().unwrap();
                let tag_error = parse_tags(provider, response()).err().unwrap();
                assert_eq!(branch_error, tag_error);
                assert!(tag_error.contains("Refresh"));
                assert!(!tag_error.contains("cached branches"));
                if status >= 500 {
                    assert!(tag_error.contains("temporarily unavailable"));
                    assert!(tag_error.contains(&format!("HTTP {status}")));
                }
            }
        }
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
                scoped: false,
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
    fn scoped_discovery_queries_only_local_heads_and_reuses_disk_cache() {
        let dir = tempfile::tempdir().unwrap();
        command::checked(dir.path(), &["init", "-q", "-b", "team/topic"]).unwrap();
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
                "root",
            ],
        )
        .unwrap();
        command::checked(
            dir.path(),
            &["update-ref", "refs/remotes/origin/unrelated", "HEAD"],
        )
        .unwrap();
        let repo = crate::git::open(dir.path()).unwrap();
        let (url, server) = server(vec![(
            200,
            String::new(),
            serde_json::to_vec(
                &serde_json::json!({"name":"team/topic", "commit":{"sha":"a".repeat(40)}}),
            )
            .unwrap(),
        )]);
        let mut remote = Remote::parse(
            "origin",
            "https://example.invalid/team/repo",
            Some("github"),
        )
        .unwrap();
        remote.endpoint = format!("{url}/branches");
        let events = std::sync::Mutex::new(Vec::new());
        let emit = |event| events.lock().unwrap().push(event);
        load_heads(
            dir.path(),
            &crate::git::git_dir(&repo),
            &remote,
            false,
            false,
            &emit,
            &configuration_data(dir.path()).unwrap().1,
        )
        .unwrap();
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("/branches/team%2Ftopic "));
        load_heads(
            dir.path(),
            &crate::git::git_dir(&repo),
            &remote,
            false,
            false,
            &emit,
            &configuration_data(dir.path()).unwrap().1,
        )
        .unwrap();
        assert!(
            matches!(events.lock().unwrap().last(), Some(Event::HeadsCached {update:Update { scoped: true, cached: true, branches, .. },..}) if branches.len() == 1)
        );
    }

    fn parallel_heads_fixture() -> serde_json::Value {
        use std::io::{Read, Write};
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        };
        let dir = tempfile::tempdir().unwrap();
        command::checked(dir.path(), &["init", "-q", "-b", "z-ghost"]).unwrap();
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
                "root",
            ],
        )
        .unwrap();
        for n in 0..11 {
            command::checked(dir.path(), &["branch", &format!("branch-{n:02}")]).unwrap();
        }
        let before = command::checked(dir.path(), &["show-ref"]).unwrap();
        let objects = command::checked(dir.path(), &["count-objects", "-v"]).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/branches", listener.local_addr().unwrap());
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (server_active, server_peak, server_requests) =
            (active.clone(), peak.clone(), requests.clone());
        let server = std::thread::spawn(move || {
            std::thread::scope(|scope| {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                for _ in 0..12 {
                    let mut socket = loop {
                        match listener.accept() {
                            Ok((socket, _)) => break socket,
                            Err(error)
                                if error.kind() == std::io::ErrorKind::WouldBlock
                                    && std::time::Instant::now() < deadline =>
                            {
                                std::thread::sleep(Duration::from_millis(1))
                            }
                            Err(error) => panic!("head fixture: {error}"),
                        }
                    };
                    let (active, peak, requests) = (
                        server_active.clone(),
                        server_peak.clone(),
                        server_requests.clone(),
                    );
                    scope.spawn(move || {
                        // Winsock accepted sockets may inherit the listener's
                        // nonblocking mode. Timeouts alone do not make reads block.
                        socket.set_nonblocking(false).unwrap();
                        socket
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .unwrap();
                        socket
                            .set_write_timeout(Some(Duration::from_secs(5)))
                            .unwrap();
                        let mut request = Vec::new();
                        let mut byte = [0u8; 1];
                        while !request.ends_with(b"\r\n\r\n") {
                            assert!(
                                request.len() < 16 * 1024,
                                "fixture request header exceeds limit"
                            );
                            assert!(
                                std::time::Instant::now() < deadline,
                                "fixture request deadline exceeded"
                            );
                            socket.read_exact(&mut byte).unwrap();
                            request.push(byte[0]);
                        }
                        let request = String::from_utf8(request).unwrap();
                        let name = request
                            .lines()
                            .next()
                            .unwrap()
                            .split_whitespace()
                            .nth(1)
                            .unwrap()
                            .strip_prefix("/branches/")
                            .unwrap()
                            .to_string();
                        requests.lock().unwrap().push(name.clone());
                        let n = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(n, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(if name == "z-ghost" {
                            10
                        } else {
                            40
                        }));
                        let body = serde_json::to_vec(
                            &serde_json::json!({"name":name,"commit":{"sha":"a".repeat(40)}}),
                        )
                        .unwrap();
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        socket.write_all(header.as_bytes()).unwrap();
                        socket.write_all(&body).unwrap();
                        active.fetch_sub(1, Ordering::SeqCst);
                    });
                }
            });
        });
        let mut remote = Remote::parse(
            "origin",
            "https://example.invalid/team/repo",
            Some("github"),
        )
        .unwrap();
        remote.endpoint = endpoint;
        let repo = crate::git::open(dir.path()).unwrap();
        let first = Mutex::new(None);
        let updates = Mutex::new(Vec::new());
        let started = std::time::Instant::now();
        let emit = |event| {
            if let Event::Branches(update)
            | Event::BranchesPartial { update, .. }
            | Event::HeadsVerified { update, .. } = event
            {
                if update.branches.iter().any(|b| b.name == "z-ghost") {
                    let mut first = first.lock().unwrap();
                    if first.is_none() {
                        *first = Some(started.elapsed().as_secs_f64() * 1000.);
                    }
                }
                updates.lock().unwrap().push(update);
            }
        };
        let target = crate::branches::Target {
            name: "z-ghost".into(),
            remote: Some("origin".into()),
            url: Some(remote.url.clone()),
            sha: "a".repeat(40),
        };
        let result = load_heads_scoped(
            dir.path(),
            &crate::git::git_dir(&repo),
            &remote,
            true,
            true,
            &emit,
            &configuration_data(dir.path()).unwrap().1,
            &[target],
            &std::sync::atomic::AtomicBool::new(false),
        );
        let final_ms = started.elapsed().as_secs_f64() * 1000.;
        // Always join the owned fixture, including a failed client request.
        let server_result = server.join();
        result.unwrap();
        server_result.unwrap();
        let updates = updates.into_inner().unwrap();
        assert_eq!(updates.last().unwrap().branches.len(), 12);
        assert!(!updates.last().unwrap().stale);
        assert!(updates.iter().any(|u| u.stale && !u.branches.is_empty()));
        assert_eq!(requests.lock().unwrap().len(), 12);
        assert!((2..=4).contains(&peak.load(Ordering::SeqCst)));
        assert_eq!(command::checked(dir.path(), &["show-ref"]).unwrap(), before);
        assert_eq!(
            command::checked(dir.path(), &["count-objects", "-v"]).unwrap(),
            objects
        );
        assert!(!repo.git_dir().join("FETCH_HEAD").exists());
        serde_json::json!({"firstHeadMs":first.into_inner().unwrap().unwrap(),"finalHeadsMs":final_ms,"requests":12,"peakRequests":peak.load(Ordering::SeqCst)})
    }
    #[test]
    fn scoped_head_checks_publish_ghost_early_with_bounded_parallelism_and_no_fetch() {
        let result = parallel_heads_fixture();
        assert!(result["firstHeadMs"].as_f64().unwrap() < result["finalHeadsMs"].as_f64().unwrap());
    }
    #[test]
    #[ignore = "one warmup and fifteen controlled API head refresh samples"]
    fn benchmark_scoped_head_refresh() {
        for sample in -1..15 {
            let mut result = parallel_heads_fixture();
            result["sample"] = sample.into();
            println!("{result}");
        }
    }
    #[test]
    fn cancelled_head_check_does_not_send_requests_or_publish_fresh_result() {
        let dir = tempfile::tempdir().unwrap();
        command::checked(dir.path(), &["init", "-q"]).unwrap();
        let repo = crate::git::open(dir.path()).unwrap();
        let mut remote = Remote::parse(
            "origin",
            "https://example.invalid/team/repo",
            Some("github"),
        )
        .unwrap();
        remote.endpoint = "http://127.0.0.1:1/branches".into();
        let target = crate::branches::Target {
            name: "ghost".into(),
            remote: Some("origin".into()),
            url: Some(remote.url.clone()),
            sha: "a".repeat(40),
        };
        let events = std::sync::Mutex::new(Vec::new());
        let result = load_heads_scoped(
            dir.path(),
            &crate::git::git_dir(&repo),
            &remote,
            true,
            true,
            &|event| events.lock().unwrap().push(event),
            &Upstreams::new(),
            &[target],
            &std::sync::atomic::AtomicBool::new(true),
        );
        assert!(result.unwrap_err().contains("cancelled"));
        assert!(events.lock().unwrap().is_empty());
    }
    #[test]
    fn credential_helper_results_persist_in_the_resolved_token_partition() {
        let dir = tempfile::tempdir().unwrap();
        command::checked(dir.path(), &["init", "-q"]).unwrap();
        command::checked(dir.path(), &["config", "credential.helper", ""]).unwrap();
        command::checked(
            dir.path(),
            &[
                "config",
                "--add",
                "credential.helper",
                "!f() { printf 'username=test\npassword=fixture-cache-token\n'; }; f",
            ],
        )
        .unwrap();
        let repo = crate::git::open(dir.path()).unwrap();
        let git_dir = crate::git::git_dir(&repo);
        let (url, server) = server(vec![
            (401, String::new(), Vec::new()),
            (200, String::new(), response(&["main"], false).body),
        ]);
        let mut remote = Remote::parse(
            "origin",
            "https://example.invalid/team/repo",
            Some("github"),
        )
        .unwrap();
        remote.endpoint = format!("{url}/branches");
        let events = std::sync::Mutex::new(Vec::new());
        let emit = |event| events.lock().unwrap().push(event);
        load_one(dir.path(), &git_dir, &remote, false, false, &emit);
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].contains("Bearer fixture-cache-token"));
        let (anonymous, _) = cache_file(&git_dir, &remote, None).unwrap();
        let (private, identity) =
            cache_file(&git_dir, &remote, Some("fixture-cache-token")).unwrap();
        assert!(!anonymous.exists());
        assert!(read_cache(&private, &identity).is_some());
        assert!(
            !String::from_utf8(std::fs::read(&private).unwrap())
                .unwrap()
                .contains("fixture-cache-token")
        );
        // Simulate another launch: only the non-secret helper marker and disk cache remain.
        HELPER_TOKENS
            .lock()
            .unwrap()
            .remove(&(dir.path().to_path_buf(), remote.endpoint.clone()));
        load_one(dir.path(), &git_dir, &remote, false, false, &emit);
        assert!(matches!(
            events.lock().unwrap().last(),
            Some(Event::Branches(Update { cached: true, .. }))
        ));
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
