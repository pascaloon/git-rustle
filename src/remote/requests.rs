//! PR/MR composition. Blocking I/O belongs on a worker; typing never hits the API.
pub(crate) use super::avatars::Cache as PhotoCache;
pub(crate) use super::avatars::Stats as PhotoStats;
pub use super::avatars::image_format;
use super::*;
use serde_json::{Value, json};
pub mod integration;
pub mod pipelines;
pub mod target;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Member {
    pub id: u64,
    pub username: String,
    pub name: String,
    pub avatar: Option<String>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Metadata {
    pub default_branch: String,
    #[serde(default)]
    pub web_url: Option<String>,
    pub members: Vec<Member>,
    pub warning: String,
}

#[derive(Clone, Default)]
pub struct Draft {
    pub source: String,
    pub target: String,
    pub title: String,
    pub description: String,
    pub assignees: Vec<Member>,
    pub reviewers: Vec<Member>,
    pub draft: bool,
}

impl Draft {
    pub fn validate(&self) -> Result<(), String> {
        if self.source.trim().is_empty() || self.target.trim().is_empty() {
            return Err("Choose a source and target branch.".into());
        }
        if self.source == self.target {
            return Err("Source and target branches must be different.".into());
        }
        if self.title.trim().is_empty() || self.title.len() > 256 {
            return Err("Enter a title of up to 256 bytes.".into());
        }
        if self.description.len() > 65535 {
            return Err("Description exceeds the provider's 65,535 byte limit.".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct Created {
    pub url: String,
    pub number: u64,
    pub warning: String,
}

pub fn root(remote: &Remote) -> &str {
    remote
        .endpoint
        .strip_suffix(match remote.provider {
            Provider::Github => "/branches",
            Provider::Gitlab => "/repository/branches",
        })
        .unwrap_or(&remote.endpoint)
}

pub struct Client<'a> {
    path: &'a Path,
    git_dir: PathBuf,
    remote: &'a Remote,
    http: Http,
    token: Option<String>,
    no_cache: bool,
    photos: Option<super::avatars::Cache>,
}

impl<'a> Client<'a> {
    pub fn new(path: &'a Path, remote: &'a Remote, no_cache: bool) -> Result<Self, String> {
        let repo = crate::git::open(path).map_err(|_| "Cannot open repository")?;
        let git_dir = crate::git::git_dir(&repo);
        Ok(Self {
            path,
            token: cached_token(path, &git_dir, remote)?,
            photos: super::avatars::Cache::new(&git_dir),
            git_dir,
            remote,
            http: Http::new(),
            no_cache,
        })
    }

    fn location(&self, key: &str) -> Option<PathBuf> {
        if self.no_cache {
            return None;
        }
        let (base, _) = cache_file(&self.git_dir, self.remote, self.token.as_deref())?;
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        ("requests-v1", key).hash(&mut hash);
        Some(
            base.with_extension("requests")
                .join(format!("{:016x}.json", hash.finish())),
        )
    }

    fn read<T: serde::de::DeserializeOwned>(&self, key: &str, ttl: u64) -> Option<T> {
        let path = self.location(key)?;
        let meta = std::fs::metadata(&path).ok()?;
        if meta.len() > 4 * 1024 * 1024 || meta.modified().ok()?.elapsed().ok()?.as_secs() >= ttl {
            return None;
        }
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    fn write(&self, key: &str, data: &impl Serialize) {
        if let Some(path) = self.location(key)
            && let Ok(bytes) = serde_json::to_vec(data)
        {
            let _ = std::fs::create_dir_all(path.parent().unwrap());
            let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
            let _ = std::fs::remove_file(tmp);
        }
    }

    fn get(&mut self, url: &str) -> Result<Response, String> {
        let mut response = self
            .http
            .get_url(self.remote, url, None, self.token.as_deref())?;
        if matches!(response.status, 401 | 403 | 404) && self.token.is_none() {
            self.token = helper_token(self.path, self.remote);
            if self.token.is_some() {
                response = self
                    .http
                    .get_url(self.remote, url, None, self.token.as_deref())?;
            }
        }
        if response.status != 200 {
            return Err(status_error(response.status));
        }
        Ok(response)
    }

    pub fn metadata(&mut self, force: bool) -> Result<Metadata, String> {
        if !force && let Some(data) = self.read("metadata", TTL) {
            return Ok(data);
        }
        let response = self.get(root(self.remote))?;
        let project: Value =
            serde_json::from_slice(&response.body).map_err(|_| "Invalid project response")?;
        let mut data = Metadata {
            default_branch: project["default_branch"].as_str().unwrap_or("").into(),
            web_url: project[if self.remote.provider == Provider::Github {
                "html_url"
            } else {
                "web_url"
            }]
            .as_str()
            .filter(|u| safe_web_url(u))
            .map(str::to_string),
            ..Metadata::default()
        };
        match self.members() {
            Ok(members) => data.members = members,
            Err(error) => data.warning = format!("Member suggestions unavailable: {error}"),
        }
        // Never cache a failed/partial roster as a complete result.
        if data.warning.is_empty() {
            self.write("metadata", &data);
        }
        Ok(data)
    }

    fn members(&mut self) -> Result<Vec<Member>, String> {
        let endpoint = match self.remote.provider {
            // This also supports public repositories without collaborator-list
            // access, and accepts the PR read permission used by the composer.
            Provider::Github => "assignees",
            Provider::Gitlab => "members/all",
        };
        let mut members = BTreeMap::new();
        for page in 1..=100 {
            let response = self.get(&format!(
                "{}/{endpoint}?per_page=100&page={page}",
                root(self.remote)
            ))?;
            let values: Vec<Value> =
                serde_json::from_slice(&response.body).map_err(|_| "Invalid members response")?;
            let count = values.len();
            for v in values {
                if v["state"].as_str().is_some_and(|s| s != "active") {
                    continue;
                }
                let username = v[if self.remote.provider == Provider::Github {
                    "login"
                } else {
                    "username"
                }]
                .as_str()
                .ok_or("Member is missing a username")?;
                let id = v["id"].as_u64().ok_or("Member is missing an ID")?;
                members.insert(
                    id,
                    Member {
                        id,
                        username: username.into(),
                        name: v["name"].as_str().unwrap_or(username).into(),
                        avatar: v["avatar_url"]
                            .as_str()
                            .filter(|u| safe_image_url(u))
                            .map(str::to_string),
                    },
                );
            }
            if !response.next.unwrap_or(count == 100) {
                let mut members: Vec<_> = members.into_values().collect();
                members.sort_unstable_by(|a, b| a.username.cmp(&b.username));
                return Ok(members);
            }
        }
        Err("Project member pagination limit reached; try again later.".into())
    }

    fn post(&mut self, suffix: &str, body: Value) -> Result<Value, String> {
        if self.token.is_none() {
            self.token = helper_token(self.path, self.remote);
        }
        let mut request = self
            .http
            .agent
            .post(format!("{}{suffix}", root(self.remote)))
            .header("User-Agent", "git-rustle/0.1")
            .header("Accept", "application/json")
            .header("Content-Type", "application/json");
        if self.remote.provider == Provider::Github {
            request = request.header("X-GitHub-Api-Version", "2022-11-28");
        }
        if let Some(token) = &self.token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        // Never retry a write: timeout can mean the provider already created it.
        let mut response = request
            .send(serde_json::to_vec(&body).map_err(|_| "Cannot encode request")?)
            .map_err(|_| {
                "Request status unknown. Check the provider before trying again.".to_string()
            })?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(status_error(status));
        }
        let bytes = response
            .body_mut()
            .with_config()
            .limit(1024 * 1024)
            .read_to_vec()
            .map_err(|_| "Request may have succeeded. Check the provider before trying again.")?;
        serde_json::from_slice(&bytes).map_err(|_| {
            "Request may have succeeded. Check the provider before trying again.".into()
        })
    }

    pub fn create(&mut self, draft: &Draft) -> Result<Created, String> {
        draft.validate()?;
        let (suffix, body) = match self.remote.provider {
            Provider::Github => (
                "/pulls",
                json!({ "head": draft.source, "base": draft.target,
                "title": draft.title.trim(), "body": draft.description, "draft": draft.draft }),
            ),
            Provider::Gitlab => (
                "/merge_requests",
                json!({ "source_branch": draft.source, "target_branch": draft.target,
                "title": if draft.draft && !draft.title.starts_with("Draft:") { format!("Draft: {}", draft.title.trim()) } else { draft.title.trim().into() },
                "description": draft.description, "assignee_ids": draft.assignees.iter().map(|m| m.id).collect::<Vec<_>>(),
                "reviewer_ids": draft.reviewers.iter().map(|m| m.id).collect::<Vec<_>>() }),
            ),
        };
        let response = self.post(suffix, body)?;
        let number = response[if self.remote.provider == Provider::Github {
            "number"
        } else {
            "iid"
        }]
        .as_u64()
        .ok_or(
            "Created, but the response omitted its number. Check the provider before trying again.",
        )?;
        let url = response[if self.remote.provider == Provider::Github {
            "html_url"
        } else {
            "web_url"
        }]
        .as_str()
        .filter(|u| safe_web_url(u))
        .ok_or(
            "Created, but the response omitted its URL. Check the provider before trying again.",
        )?
        .to_string();
        let mut created = Created {
            url,
            number,
            warning: String::new(),
        };
        if self.remote.provider == Provider::Github {
            for (field, suffix, people) in [
                (
                    "assignees",
                    format!("/issues/{number}/assignees"),
                    &draft.assignees,
                ),
                (
                    "reviewers",
                    format!("/pulls/{number}/requested_reviewers"),
                    &draft.reviewers,
                ),
            ] {
                if !people.is_empty() {
                    let result = self
                        .post(
                            &suffix,
                            json!({field: people.iter().map(|m| &m.username).collect::<Vec<_>>()}),
                        )
                        .and_then(|value| {
                            let key = if field == "assignees" {
                                "assignees"
                            } else {
                                "users"
                            };
                            let confirmed = value[key].as_array();
                            if people.iter().all(|person| {
                                confirmed.is_some_and(|users| {
                                    users.iter().any(|user| {
                                        user["login"].as_str() == Some(&person.username)
                                    })
                                })
                            }) {
                                Ok(())
                            } else {
                                Err("The provider did not confirm all selected members.".into())
                            }
                        });
                    let Err(error) = result else {
                        continue;
                    };
                    if !created.warning.is_empty() {
                        created.warning.push(' ');
                    }
                    created.warning.push_str(&format!(
                        "Could not add {field}: {error} Update them on GitHub."
                    ));
                }
            }
        }
        Ok(created)
    }

    /// Resolve only an inspected commit. Immutable identity and image caches keep
    /// repeat selection offline; failed lookups are suppressed by the UI session.
    fn author_url(&mut self, sha: &str) -> Option<String> {
        if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let key = format!("author-{sha}");
        let avatar = if let Some(url) = self.read::<String>(&key, 86400) {
            url
        } else {
            let suffix = if self.remote.provider == Provider::Github {
                "commits"
            } else {
                "repository/commits"
            };
            let response = self
                .get(&format!("{}/{suffix}/{sha}", root(self.remote)))
                .ok()?;
            let value: Value = serde_json::from_slice(&response.body).ok()?;
            let url = match self.remote.provider {
                Provider::Github => {
                    let mut url = url::Url::parse(value["author"]["avatar_url"].as_str()?).ok()?;
                    url.query_pairs_mut().append_pair("s", "64");
                    url.to_string()
                }
                Provider::Gitlab => {
                    // GitLab's avatar endpoint resolves public account emails and
                    // the instance's configured fallback service.
                    let email = value["author_email"].as_str()?;
                    let response = self
                        .get(&format!(
                            "{}/avatar?email={}&size=64",
                            self.remote.endpoint.split("/projects/").next()?,
                            encode(email)
                        ))
                        .ok()?;
                    let avatar: Value = serde_json::from_slice(&response.body).ok()?;
                    avatar["avatar_url"].as_str()?.to_string()
                }
            };
            if !safe_image_url(&url) {
                return None;
            }
            self.write(&key, &url);
            url
        };
        Some(avatar)
    }

    #[cfg(test)]
    pub fn author(&mut self, sha: &str) -> Option<Vec<u8>> {
        self.author_for_email(sha, "")
    }

    fn photo_cache(&self) -> Option<&super::avatars::Cache> {
        self.photos.as_ref().filter(|_| !self.no_cache)
    }

    pub(crate) fn set_photo_cache(&mut self, cache: Option<PhotoCache>) {
        self.photos = cache;
    }

    pub fn author_for_email(&mut self, sha: &str, email: &str) -> Option<Vec<u8>> {
        if let Some(url) = self
            .photo_cache()
            .and_then(|c| c.identity(self.remote, email))
        {
            return self.avatar(&url);
        }
        let url = self.author_url(sha)?;
        let bytes = self.avatar(&url)?;
        if let Some(cache) = self.photo_cache() {
            cache.store_identity(self.remote, email, &url);
        }
        Some(bytes)
    }

    pub(crate) fn cached_avatar(&self, url: &str) -> Option<Vec<u8>> {
        if !safe_image_url(url) {
            return None;
        }
        let key = format!("image-{url}");
        if let Some(bytes) = self.photo_cache().and_then(|c| c.image(url)) {
            return Some(bytes);
        }
        // Upgrade existing fresh JSON entries without another download.
        if self
            .photo_cache()
            .is_none_or(|cache| cache.legacy_allowed())
            && let Some(bytes) = self.read::<Vec<u8>>(&key, 86400)
            && bytes.len() <= super::avatars::MAX_IMAGE as usize
            && image_format(&bytes).is_some()
        {
            if self
                .photo_cache()
                .is_some_and(|c| c.store_image(url, &bytes))
                && let Some(path) = self.location(&key)
            {
                let _ = std::fs::remove_file(path);
            }
            return Some(bytes);
        }
        None
    }

    pub fn avatar(&self, url: &str) -> Option<Vec<u8>> {
        if !safe_image_url(url) {
            return None;
        }
        if let Some(bytes) = self.cached_avatar(url) {
            return Some(bytes);
        }
        // Image hosts receive no provider token, cookies or redirect credentials.
        let mut response = self
            .http
            .agent
            .get(url)
            .header("User-Agent", "git-rustle/0.1")
            .call()
            .ok()?;
        if response.status().as_u16() != 200 {
            return None;
        }
        let bytes = response
            .body_mut()
            .with_config()
            .limit(super::avatars::MAX_IMAGE)
            .read_to_vec()
            .ok()?;
        image_format(&bytes)?;
        if let Some(cache) = self.photo_cache() {
            cache.store_image(url, &bytes);
        }
        Some(bytes)
    }
}

pub fn safe_web_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|u| {
        u.scheme() == "https"
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none()
    })
}
pub(super) fn safe_image_url(value: &str) -> bool {
    safe_web_url(value)
}

fn status_error(status: u16) -> String {
    match status {
        401 => "Sign in with a valid provider token in Settings.".into(),
        403 | 429 => format!("HTTP {status}: permission denied or API rate limit reached. Check access in Settings or retry later."),
        404 => "Project or branch not found, or your token cannot access it.".into(),
        300..=399 => "Request status unknown. Provider redirected the request; check the project on the provider before trying again.".into(),
        409 | 422 => "Provider rejected the request. Push the source branch, check the target and title, and check for an existing PR/MR or no changes.".into(),
        500..=599 => "Provider error; creation status may be unknown. Check the provider before trying again.".into(),
        _ => format!("Provider returned HTTP {status}. Check the project and token permissions."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    pub(super) fn server(
        responses: Vec<(u16, &str, Value)>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let responses: Vec<_> = responses
            .into_iter()
            .map(|(s, h, b)| (s, h.to_string(), serde_json::to_vec(&b).unwrap()))
            .collect();
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
                    std::thread::sleep(Duration::from_millis(1));
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut b = [0];
                    stream.read_exact(&mut b).unwrap();
                    request.push(b[0]);
                }
                let headers_text = String::from_utf8(request.clone()).unwrap();
                let length = headers_text
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|n| n.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                let mut bytes = vec![0; length];
                stream.read_exact(&mut bytes).unwrap();
                request.extend(bytes);
                requests.push(String::from_utf8(request).unwrap());
                write!(stream, "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n", body.len()).unwrap();
                stream.write_all(&body).unwrap();
            }
            requests
        });
        (url, handle)
    }
    pub(super) fn fixture(provider: Provider, url: &str, path: &Path) -> Remote {
        let mut r = Remote::parse(
            "origin",
            "https://example.invalid/team/project.git",
            Some(provider.key()),
        )
        .unwrap();
        r.endpoint = format!(
            "{url}{}",
            if provider == Provider::Github {
                "/repos/team/project/branches"
            } else {
                "/api/v4/projects/team%2Fproject/repository/branches"
            }
        );
        command::checked(path, &["init", "-q"]).unwrap();
        r
    }
    fn member() -> Member {
        Member {
            id: 42,
            username: "alice".into(),
            name: "Alice".into(),
            avatar: None,
        }
    }
    fn draft() -> Draft {
        Draft {
            source: "feature/a".into(),
            target: "main".into(),
            title: "A useful change".into(),
            description: "## Summary\n**Markdown**\n- [x] Tested".into(),
            assignees: vec![member()],
            reviewers: vec![member()],
            draft: true,
        }
    }
    pub(super) fn client<'a>(path: &'a Path, remote: &'a Remote) -> Client<'a> {
        Client {
            path,
            git_dir: path.join(".git"),
            remote,
            http: Http::new(),
            token: Some("fixture-only".into()),
            no_cache: true,
            photos: Some(super::super::avatars::Cache::at(path.join("photos"))),
        }
    }

    #[test]
    fn github_creates_once_and_retains_partial_assignment_failure() {
        let (url, server) = server(vec![
            (
                201,
                "",
                json!({"number":7,"html_url":"https://github.com/team/project/pull/7"}),
            ),
            (403, "", json!({})),
            (201, "", json!({})),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let remote = fixture(Provider::Github, &url, dir.path());
        let result = client(dir.path(), &remote).create(&draft()).unwrap();
        assert_eq!(result.number, 7);
        assert!(result.warning.contains("assignees"));
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(requests[0].starts_with("POST /repos/team/project/pulls "));
        let payload: Value =
            serde_json::from_str(requests[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(payload["body"], draft().description);
        assert_eq!(payload["draft"], true);
        assert_eq!(payload["head"], "feature/a");
        assert!(requests[1].starts_with("POST /repos/team/project/issues/7/assignees "));
        let payload: Value =
            serde_json::from_str(requests[2].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(payload["reviewers"], json!(["alice"]));
    }

    #[test]
    fn gitlab_creates_draft_with_ids_and_nested_project() {
        let (url, server) = server(vec![(
            201,
            "",
            json!({"iid":8,"web_url":"https://gitlab.example/team/project/-/merge_requests/8"}),
        )]);
        let dir = tempfile::tempdir().unwrap();
        let remote = fixture(Provider::Gitlab, &url, dir.path());
        assert_eq!(
            client(dir.path(), &remote).create(&draft()).unwrap().number,
            8
        );
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("POST /api/v4/projects/team%2Fproject/merge_requests "));
        let payload: Value =
            serde_json::from_str(requests[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(payload["title"], "Draft: A useful change");
        assert_eq!(payload["reviewer_ids"], json!([42]));
        assert_eq!(payload["assignee_ids"], json!([42]));
    }

    #[test]
    fn member_pagination_cache_and_account_partition() {
        let (url, server) = server(vec![
            (200, "", json!({"default_branch":"develop"})),
            (
                200,
                "X-Next-Page: 2\r\n",
                json!([{"id":2,"username":"bob","name":"Bob","state":"active"}]),
            ),
            (
                200,
                "X-Next-Page: \r\n",
                json!([{"id":1,"username":"alice","name":"Alice"},{"id":2,"username":"bob","name":"Bob"},{"id":3,"username":"blocked","state":"blocked"}]),
            ),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let remote = fixture(Provider::Gitlab, &url, dir.path());
        let mut client = client(dir.path(), &remote);
        client.no_cache = false;
        let metadata = client.metadata(false).unwrap();
        assert_eq!(metadata.default_branch, "develop");
        assert_eq!(metadata.members.len(), 2);
        assert_eq!(metadata.members[0].username, "alice");
        assert_eq!(server.join().unwrap().len(), 3);
        assert_eq!(client.metadata(false).unwrap().members.len(), 2); // server is offline
        let location = client.location("metadata").unwrap();
        client.token = Some("different-account".into());
        assert_ne!(client.location("metadata").unwrap(), location);
        assert!(client.read::<Metadata>("metadata", TTL).is_none());
        client.token = Some("fixture-only".into());
        std::fs::write(&location, b"invalid").unwrap();
        assert!(client.read::<Metadata>("metadata", TTL).is_none());
        let _ = std::fs::remove_dir_all(location.parent().unwrap());
    }

    #[test]
    fn failed_roster_remains_retryable_and_does_not_prevent_writing() {
        let (url, server) = server(vec![
            (200, "", json!({"default_branch":"main"})),
            (403, "", json!({})),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let remote = fixture(Provider::Github, &url, dir.path());
        let data = client(dir.path(), &remote).metadata(false).unwrap();
        assert!(data.members.is_empty());
        assert!(data.warning.contains("403"));
        assert_eq!(server.join().unwrap().len(), 2);
    }

    #[test]
    fn writes_never_follow_redirects_or_retry_validation_errors() {
        for status in [302, 422, 500] {
            let (url, server) = server(vec![(
                status,
                "Location: https://example.invalid/steal\r\n",
                json!({}),
            )]);
            let dir = tempfile::tempdir().unwrap();
            let remote = fixture(Provider::Github, &url, dir.path());
            let error = client(dir.path(), &remote).create(&draft()).unwrap_err();
            assert!(!error.contains("fixture-only"));
            assert_eq!(server.join().unwrap().len(), 1);
        }
    }

    #[test]
    fn invalid_draft_never_issues_a_request_and_image_urls_are_restricted() {
        let mut draft = draft();
        draft.target = draft.source.clone();
        assert!(draft.validate().is_err());
        draft.target = "main".into();
        draft.title.clear();
        assert!(draft.validate().is_err());
        for bad in [
            "file:///secret",
            "http://example.com/photo",
            "https://user:pass@example.com/photo",
            "javascript:alert(1)",
        ] {
            assert!(!safe_web_url(bad));
        }
        assert!(safe_web_url(
            "https://avatars.githubusercontent.com/u/42?s=64"
        ));
        assert!(image_format(b"<svg />").is_none());
    }

    #[test]
    fn author_resolution_uses_exact_commit_and_gitlab_email_with_offline_cache() {
        for provider in [Provider::Github, Provider::Gitlab] {
            let responses = if provider == Provider::Github {
                vec![(
                    200,
                    "",
                    json!({"author":{"avatar_url":"https://avatars.githubusercontent.com/u/42"}}),
                )]
            } else {
                vec![
                    (200, "", json!({"author_email":"a+b@example.com"})),
                    (
                        200,
                        "",
                        json!({"avatar_url":"https://gitlab.example/uploads/avatar.png"}),
                    ),
                ]
            };
            let (url, server) = server(responses);
            let dir = tempfile::tempdir().unwrap();
            let remote = fixture(provider, &url, dir.path());
            let mut client = client(dir.path(), &remote);
            client.no_cache = false;
            let sha = "a".repeat(40);
            let avatar = client.author_url(&sha).unwrap();
            // Seed image bytes to test cache reuse independently of external media.
            let bytes = b"\x89PNG\r\n\x1a\nfixture".to_vec();
            client.write(&format!("image-{avatar}"), &bytes);
            let requests = server.join().unwrap();
            assert!(requests[0].contains(&sha));
            if provider == Provider::Gitlab {
                assert!(requests[1].contains("/avatar?email=a%2Bb%40example.com&size=64"));
            } else {
                assert!(avatar.ends_with("?s=64"));
            }
            let legacy = client.location(&format!("image-{avatar}")).unwrap();
            assert!(legacy.exists());
            assert_eq!(client.author(&sha), Some(bytes.clone())); // API server stopped
            assert!(!legacy.exists()); // migrated to shared binary, without downloading
            assert_eq!(client.photo_cache().unwrap().image(&avatar), Some(bytes));
            let location = client.location(&format!("author-{sha}")).unwrap();
            let _ = std::fs::remove_dir_all(location.parent().unwrap());
        }
    }

    #[test]
    fn known_author_reuses_binary_across_repositories_commits_and_credentials() {
        let (url, server) = server(vec![(
            200,
            "",
            json!({"author":{
                "avatar_url":"https://avatars.example/u/shared-cache-test"
            }}),
        )]);
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let remote = fixture(Provider::Github, &url, &first);
        let mut other = fixture(Provider::Github, &url, &second);
        other.endpoint = other
            .endpoint
            .replace("/team/project/", "/different/repository/");
        let photos = dir.path().join("shared");
        let mut client1 = client(&first, &remote);
        let mut client2 = client(&second, &other);
        for client in [&mut client1, &mut client2] {
            client.no_cache = false;
            client.photos = Some(super::super::avatars::Cache::at(photos.clone()));
        }
        client2.token = Some("a-different-credential".into());
        let bytes = b"\x89PNG\r\n\x1a\nshared".to_vec();
        let image_url = "https://avatars.example/u/shared-cache-test?s=64";
        assert!(
            client1
                .photo_cache()
                .unwrap()
                .store_image(image_url, &bytes)
        );
        assert_eq!(
            client1.author_for_email(&"a".repeat(40), "alice@example.com"),
            Some(bytes.clone())
        );
        assert_eq!(server.join().unwrap().len(), 1);
        // No commit/author API or image host is available now, and this SHA is new.
        assert_eq!(
            client2.author_for_email(&"b".repeat(40), "alice@example.com"),
            Some(bytes)
        );
        assert_eq!(std::fs::read_dir(photos).unwrap().count(), 2); // one image + one binding
        client2.no_cache = true;
        assert!(
            client2
                .author_for_email("invalid", "alice@example.com")
                .is_none()
        );
        let _ = std::fs::remove_dir_all(client1.location("unused").unwrap().parent().unwrap());
    }

    #[test]
    #[ignore]
    fn benchmark_author_cache() {
        let dir = tempfile::tempdir().unwrap();
        let mut cold = Vec::new();
        let mut warm = Vec::new();
        for sample in 0..16 {
            let (url, server) = server(vec![(
                200,
                "",
                json!({"author":{"avatar_url":"https://avatars.githubusercontent.com/u/42"}}),
            )]);
            let remote = fixture(Provider::Github, &url, dir.path());
            let mut client = client(dir.path(), &remote);
            client.no_cache = false;
            let mut bytes = vec![137u8; 4096];
            bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
            client
                .photo_cache()
                .unwrap()
                .store_image("https://avatars.githubusercontent.com/u/42?s=64", &bytes);
            let sha = "a".repeat(40);
            let start = std::time::Instant::now();
            assert_eq!(client.author(&sha).unwrap().len(), 4096);
            let cold_ms = start.elapsed().as_secs_f64() * 1000.;
            assert_eq!(server.join().unwrap().len(), 1);
            let start = std::time::Instant::now();
            for _ in 0..100 {
                assert_eq!(client.author(&sha).unwrap().len(), 4096);
            }
            let warm_ms = start.elapsed().as_secs_f64() * 1000. / 100.;
            if sample > 0 {
                cold.push(cold_ms);
                warm.push(warm_ms);
            }
            let location = client.location(&format!("author-{sha}")).unwrap();
            let _ = std::fs::remove_dir_all(location.parent().unwrap());
        }
        println!(
            "author_loopback_identity_cached_image_ms={cold:?}\nauthor_disk_cache_ms={warm:?}"
        );
    }

    #[test]
    #[ignore]
    fn benchmark_shared_author_cache() {
        let dir = tempfile::tempdir().unwrap();
        let remote = fixture(Provider::Github, "http://127.0.0.1:1", dir.path());
        let mut client = client(dir.path(), &remote);
        client.no_cache = false;
        let image_url = "https://avatars.example/u/benchmark?s=64";
        let mut bytes = vec![137u8; 4096];
        bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        let cache = client.photo_cache().unwrap();
        assert!(cache.store_image(image_url, &bytes));
        cache.store_identity(&remote, "alice@example.com", image_url);
        let legacy = serde_json::to_vec(&bytes).unwrap();
        println!(
            "image_binary_bytes={} image_json_bytes={}",
            bytes.len(),
            legacy.len()
        );
        let mut samples = Vec::new();
        for sample in 0..16 {
            let start = std::time::Instant::now();
            for i in 0..100 {
                assert_eq!(
                    client.author_for_email(&format!("{i:040x}"), "alice@example.com"),
                    Some(bytes.clone())
                );
            }
            if sample > 0 {
                samples.push(start.elapsed().as_secs_f64() * 1000. / 100.);
            }
        }
        println!("shared_author_different_commits_disk_ms={samples:?}");
    }

    #[test]
    #[ignore]
    fn live_github_readonly() {
        let path = std::env::var_os("GIT_RUSTLE_LIVE_PATH").expect("live repository required");
        let path = Path::new(&path);
        let remote = if let Ok(project) = std::env::var("GIT_RUSTLE_LIVE_PROJECT") {
            Remote::parse("live", &format!("https://github.com/{project}.git"), None).unwrap()
        } else {
            configured(path)
                .unwrap()
                .0
                .into_iter()
                .find(|r| r.provider == Provider::Github)
                .expect("GitHub remote required")
        };
        let mut client = Client::new(path, &remote, true).unwrap();
        let start = std::time::Instant::now();
        let data = client.metadata(false).unwrap();
        assert!(data.warning.is_empty(), "{}", data.warning);
        assert!(!data.default_branch.is_empty());
        println!(
            "live_readonly default_branch_present=true members={} elapsed_ms={:.3}",
            data.members.len(),
            start.elapsed().as_secs_f64() * 1000.
        );
        let response = client
            .get(&format!("{}/commits?per_page=1", root(&remote)))
            .unwrap();
        let commits: Value = serde_json::from_slice(&response.body).unwrap();
        let sha = commits[0]["sha"].as_str().unwrap();
        println!(
            "live_readonly author_image_present={}",
            client.author(sha).is_some()
        );
    }

    #[test]
    #[ignore]
    fn benchmark_roster() {
        let dir = tempfile::tempdir().unwrap();
        let members: Vec<_> = (0..100)
            .map(|i| json!({"id":i,"username":format!("member-{i}"),"name":format!("Member {i}")}))
            .collect();
        let mut cold = Vec::new();
        let mut warm = Vec::new();
        for sample in 0..16 {
            let (url, server) = server(vec![
                (200, "", json!({"default_branch":"main"})),
                (200, "X-Next-Page: \r\n", json!(members)),
            ]);
            let remote = fixture(Provider::Gitlab, &url, dir.path());
            let mut client = client(dir.path(), &remote);
            client.no_cache = false;
            let start = std::time::Instant::now();
            assert_eq!(client.metadata(true).unwrap().members.len(), 100);
            let cold_ms = start.elapsed().as_secs_f64() * 1000.;
            assert_eq!(server.join().unwrap().len(), 2);
            let start = std::time::Instant::now();
            assert_eq!(client.metadata(false).unwrap().members.len(), 100);
            let warm_ms = start.elapsed().as_secs_f64() * 1000.;
            if sample > 0 {
                cold.push(cold_ms);
                warm.push(warm_ms);
            }
            if let Some(path) = client.location("metadata") {
                let _ = std::fs::remove_dir_all(path.parent().unwrap());
            }
        }
        println!("roster_loopback_cold_ms={cold:?}\nroster_disk_cache_ms={warm:?}");
    }
}
