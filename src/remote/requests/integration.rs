//! Read-only provider summaries. Call on workers; rendering uses an in-memory index.
use super::*;
mod graphql;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Association {
    pub number: u64,
    pub title: String,
    pub url: String,
}

pub type Index = BTreeMap<String, Vec<Association>>;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub web_url: Option<String>,
    pub avatar: Option<String>,
}

pub enum SummaryEvent {
    Project(Project),
    Image(Vec<u8>),
    Requests(Index),
}

fn project_avatar_url(remote: &Remote, web: Option<&str>, avatar: &str) -> Option<String> {
    if safe_image_url(avatar) {
        return Some(avatar.into());
    }
    if remote.provider != Provider::Gitlab || avatar.trim().is_empty() {
        return None;
    }
    let mut base =
        url::Url::parse(web.filter(|u| safe_web_url(u)).unwrap_or(&remote.endpoint)).ok()?;
    let project = remote
        .project
        .split('/')
        .map(encode)
        .collect::<Vec<_>>()
        .join("/");
    let prefix = base
        .path()
        .strip_suffix(&project)
        .map(str::to_string)
        .or_else(|| {
            base.path()
                .split_once("/api/v4/")
                .map(|(prefix, _)| format!("{prefix}/"))
        })
        .unwrap_or_else(|| "/".into());
    base.set_path(&prefix);
    base.set_query(None);
    base.set_fragment(None);
    let resolved = base.join(avatar).ok()?;
    safe_image_url(resolved.as_str()).then(|| resolved.to_string())
}

fn project_avatar_key(endpoint: &str, token: Option<&str>, avatar: &str) -> String {
    format!(
        "private-project-avatar-v1\0{endpoint}\0{}\0{avatar}",
        token.unwrap_or("")
    )
}

/// GitHub exposes an owner's avatar; GitLab exposes a project avatar.
impl Client<'_> {
    pub fn cached_project(&self) -> Option<Project> {
        let key = if self.remote.provider == Provider::Gitlab {
            "project-icon-v2"
        } else {
            "project-icon-v1"
        };
        let mut project = self.read::<Project>(key, 86400)?;
        project.web_url = project.web_url.filter(|u| safe_web_url(u));
        project.avatar = project
            .avatar
            .and_then(|u| project_avatar_url(self.remote, project.web_url.as_deref(), &u));
        Some(project)
    }

    pub fn project(&mut self, force: bool) -> Result<Project, String> {
        if !force && let Some(project) = self.cached_project() {
            return Ok(project);
        }
        let response = self.get(root(self.remote))?;
        let value: Value =
            serde_json::from_slice(&response.body).map_err(|_| "Invalid project response")?;
        let (web, avatar) = match self.remote.provider {
            Provider::Github => (&value["html_url"], &value["owner"]["avatar_url"]),
            Provider::Gitlab => (&value["web_url"], &value["avatar_url"]),
        };
        let web_url = web.as_str().filter(|u| safe_web_url(u)).map(str::to_string);
        let project = Project {
            avatar: avatar
                .as_str()
                .and_then(|u| project_avatar_url(self.remote, web_url.as_deref(), u)),
            web_url,
        };
        self.write(
            if self.remote.provider == Provider::Gitlab {
                "project-icon-v2"
            } else {
                "project-icon-v1"
            },
            &project,
        );
        Ok(project)
    }

    pub fn cached_project_avatar(&self, project: &Project) -> Option<Vec<u8>> {
        let avatar = project.avatar.as_deref()?;
        if self.remote.provider == Provider::Gitlab {
            let endpoint = format!("{}/avatar", root(self.remote));
            let key = project_avatar_key(&endpoint, self.token.as_deref(), avatar);
            if let Some(bytes) = self.photo_cache().and_then(|cache| cache.image(&key)) {
                return Some(bytes);
            }
        }
        self.cached_avatar(avatar)
    }

    /// Deliver disk-cached header data before any provider-list HTTP wait. The
    /// callback can stop work after a repository switch; images also have their
    /// own clear-generation check supplied by the desktop worker.
    pub fn summary_stream(
        &mut self,
        force: bool,
        images: impl Fn() -> bool,
        mut emit: impl FnMut(SummaryEvent) -> bool,
    ) {
        let cached = self.cached_project();
        let mut cached_image = false;
        if let Some(project) = cached.as_ref() {
            if !emit(SummaryEvent::Project(project.clone())) {
                return;
            }
            if images()
                && let Some(bytes) = self.cached_project_avatar(project)
            {
                cached_image = true;
                if !emit(SummaryEvent::Image(bytes)) {
                    return;
                }
            }
        }
        let project = if force || cached.is_none() {
            self.project(force).ok().or_else(|| cached.clone())
        } else {
            cached.clone()
        };
        if let Some(project) = project {
            if cached.as_ref() != Some(&project) && !emit(SummaryEvent::Project(project.clone())) {
                return;
            }
            if images()
                && (!cached_image
                    || cached.as_ref().and_then(|p| p.avatar.as_ref()) != project.avatar.as_ref())
                && let Some(bytes) = self.project_avatar(&project)
                && !emit(SummaryEvent::Image(bytes))
            {
                return;
            }
        }
        // A failed list still removes stale open-request badges, as before.
        emit(SummaryEvent::Requests(
            self.open_requests(force).unwrap_or_default(),
        ));
    }

    pub fn project_avatar(&mut self, project: &Project) -> Option<Vec<u8>> {
        let avatar = project.avatar.as_deref()?;
        if let Some(bytes) = self.cached_project_avatar(project) {
            return Some(bytes);
        }
        if self.remote.provider != Provider::Gitlab {
            return self.avatar(avatar);
        }
        // Only the configured project API receives credentials. Advertised image
        // URLs can name arbitrary hosts and are always fetched anonymously below.
        let endpoint = format!("{}/avatar", root(self.remote));
        for attempt in 0..2 {
            let key = project_avatar_key(&endpoint, self.token.as_deref(), avatar);
            if let Some(bytes) = self.photo_cache().and_then(|cache| cache.image(&key)) {
                return Some(bytes);
            }
            if let Some(bytes) = self.photo_cache().and_then(|cache| cache.image(avatar)) {
                return Some(bytes);
            }
            let mut request = self
                .http
                .agent
                .get(&endpoint)
                .header("User-Agent", "git-rustle/0.1")
                .header("Accept", "image/*");
            if let Some(token) = &self.token {
                request = request.header("Authorization", format!("Bearer {token}"));
            }
            let Ok(mut response) = request.call() else {
                break;
            };
            let status = response.status().as_u16();
            if attempt == 0 && matches!(status, 401 | 403) && self.token.is_none() {
                self.token = helper_token(self.path, self.remote);
                if self.token.is_some() {
                    continue;
                }
            }
            if status != 200 {
                break;
            }
            let Ok(bytes) = response
                .body_mut()
                .with_config()
                .limit(super::super::avatars::MAX_IMAGE)
                .read_to_vec()
            else {
                break;
            };
            if image_format(&bytes).is_none() {
                break;
            }
            // A private namespace, including the credential, is hashed into the
            // ordinary image cache. Public URL lookups cannot access this key;
            // the normal size/clear controls and clear-generation barrier apply.
            if let Some(cache) = self.photo_cache() {
                cache.store_image(&key, &bytes);
            }
            return Some(bytes);
        }
        // Older GitLab servers or redirects can still expose a public image.
        // The HTTP agent disables redirects, so a token never follows one.
        self.avatar(avatar)
    }

    pub fn open_requests(&mut self, force: bool) -> Result<Index, String> {
        if !force && let Some(mut index) = self.read::<Index>("open-requests-v1", TTL) {
            index.retain(|branch, requests| {
                requests.retain(|request| request.number > 0 && safe_web_url(&request.url));
                !branch.is_empty() && !branch.chars().any(char::is_control) && !requests.is_empty()
            });
            return Ok(index);
        }
        if self.remote.provider == Provider::Gitlab
            && let Ok(index) = graphql::load(self)
        {
            self.write("open-requests-v1", &index);
            return Ok(index);
        }
        let mut index = Index::new();
        let started = std::time::Instant::now();
        let mut total_bytes = 0;
        let (suffix, state) = match self.remote.provider {
            Provider::Github => ("pulls", "open"),
            Provider::Gitlab => ("merge_requests", "opened"),
        };
        for page in 1..=100 {
            let response = self.get(&format!(
                "{}/{suffix}?state={state}&per_page=100&page={page}",
                root(self.remote)
            ))?;
            total_bytes += response.body.len();
            if total_bytes > 32 * 1024 * 1024 || started.elapsed() > Duration::from_secs(60) {
                return Err("PR / MR listing exceeded its size or time limit.".into());
            }
            let values: Vec<Value> =
                serde_json::from_slice(&response.body).map_err(|_| "Invalid PR / MR response")?;
            let count = values.len();
            if count > 100 {
                return Err("PR / MR page exceeded its requested size.".into());
            }
            append_requests(self.remote, &values, &mut index);
            if !response.next.unwrap_or(count == 100) {
                if serde_json::to_vec(&index)
                    .map_err(|_| "Invalid PR / MR index")?
                    .len()
                    > 4 * 1024 * 1024
                {
                    return Err("PR / MR index exceeded its cache size limit.".into());
                }
                self.write("open-requests-v1", &index);
                return Ok(index);
            }
        }
        Err("PR / MR pagination limit reached; retry later.".into())
    }
}

fn append_requests(remote: &Remote, values: &[Value], index: &mut Index) {
    for value in values {
        let (branch, number, url, own_project) = match remote.provider {
            Provider::Github => (
                value["head"]["ref"].as_str(),
                value["number"].as_u64(),
                value["html_url"].as_str(),
                value["head"]["repo"]["full_name"]
                    .as_str()
                    .is_some_and(|p| p.eq_ignore_ascii_case(&remote.project)),
            ),
            Provider::Gitlab => (
                value["source_branch"].as_str(),
                value["iid"].as_u64(),
                value["web_url"].as_str(),
                value["source_project_id"]
                    .as_u64()
                    .is_some_and(|source| Some(source) == value["target_project_id"].as_u64()),
            ),
        };
        let (Some(branch), Some(number), Some(url)) = (branch, number, url) else {
            continue;
        };
        if !own_project
            || branch.is_empty()
            || branch.len() > 4096
            || branch.chars().any(char::is_control)
            || number == 0
            || url.len() > 8192
            || !safe_web_url(url)
        {
            continue;
        }
        let associations = index.entry(branch.into()).or_default();
        if !associations.iter().any(|a| a.number == number) {
            associations.push(Association {
                number,
                title: value["title"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(1024)
                    .collect(),
                url: url.into(),
            });
        }
    }
}

/// A transport URL can be SSH, but provider browser links always use HTTPS.
pub fn web_root(remote: &Remote) -> Option<String> {
    let normalized = if remote.url.contains("://") {
        remote.url.clone()
    } else {
        let (host, path) = remote.url.split_once(':')?;
        format!("ssh://{host}/{path}")
    };
    let mut url = url::Url::parse(&normalized).ok()?;
    let ssh = matches!(url.scheme(), "ssh" | "git");
    if ssh {
        url.set_port(None).ok()?;
    }
    // url::Url cannot switch between non-special SSH and special HTTPS schemes.
    let host = url.host_str()?;
    let authority = if !ssh && let Some(port) = url.port() {
        format!("{host}:{port}")
    } else {
        host.into()
    };
    let path = remote
        .project
        .split('/')
        .map(encode)
        .collect::<Vec<_>>()
        .join("/");
    let root = format!("https://{authority}/{path}");
    safe_web_url(&root).then_some(root)
}

pub fn commit_url(remote: &Remote, root: Option<&str>, sha: &str) -> Option<String> {
    if sha.is_empty() || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let base = root
        .filter(|u| safe_web_url(u))
        .map(str::to_string)
        .or_else(|| web_root(remote))?;
    Some(format!(
        "{}/{}{}",
        base.trim_end_matches('/'),
        if remote.provider == Provider::Gitlab {
            "-/commit/"
        } else {
            "commit/"
        },
        sha
    ))
}

pub fn branch_url(remote: &Remote, root: Option<&str>, branch: &str) -> Option<String> {
    if branch.is_empty() || branch.chars().any(char::is_control) {
        return None;
    }
    let base = root
        .filter(|u| safe_web_url(u))
        .map(str::to_string)
        .or_else(|| web_root(remote))?;
    Some(format!(
        "{}/{}{}",
        base.trim_end_matches('/'),
        if remote.provider == Provider::Gitlab {
            "-/tree/"
        } else {
            "tree/"
        },
        encode(branch)
    ))
}

#[cfg(test)]
mod tests {
    use super::super::super::tests::server as raw_server;
    use super::super::tests::{client, fixture, server};
    use super::*;

    const PROJECT_PHOTO: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x04\x00\x00\x00\xb5\x1c\x0c\x02\x00\x00\x00\x0bIDAT\x78\xda\x63\xfc\xff\x1f\x00\x03\x03\x02\x00\xef\xa3\xf9\x84\x00\x00\x00\x00IEND\xae\x42\x60\x82";

    #[test]
    fn gitlab_relative_icons_preserve_instance_prefix_and_reject_unsafe_urls() {
        let mut remote =
            Remote::parse("origin", "git@alias:team/project.git", Some("gitlab")).unwrap();
        remote.endpoint =
            "https://code.example/gitlab/api/v4/projects/team%2Fproject/repository/branches".into();
        for (avatar, expected) in [
            (
                "uploads/project.png",
                "https://code.example/gitlab/uploads/project.png",
            ),
            (
                "/gitlab/uploads/project.png",
                "https://code.example/gitlab/uploads/project.png",
            ),
            (
                "https://images.example/p.png",
                "https://images.example/p.png",
            ),
            ("//images.example/p.png", "https://images.example/p.png"),
        ] {
            assert_eq!(
                project_avatar_url(
                    &remote,
                    Some("https://code.example/gitlab/team/project"),
                    avatar
                )
                .as_deref(),
                Some(expected)
            );
        }
        assert_eq!(
            project_avatar_url(&remote, None, "uploads/p.png").as_deref(),
            Some("https://code.example/gitlab/uploads/p.png")
        );
        for unsafe_url in [
            "javascript:alert(1)",
            "http://code.example/p.png",
            "https://user:secret@code.example/p.png",
            "",
        ] {
            assert!(project_avatar_url(&remote, None, unsafe_url).is_none());
        }
        remote.provider = Provider::Github;
        assert!(project_avatar_url(&remote, None, "/uploads/p.png").is_none());
    }

    #[test]
    fn corrected_project_metadata_bypasses_old_negative_icon_cache_and_reuses_disk() {
        let (url, server) = server(vec![(
            200,
            "",
            json!({
                "web_url":"https://code.example/gitlab/team/project", "avatar_url":"uploads/p.png"
            }),
        )]);
        let dir = tempfile::tempdir().unwrap();
        let remote = fixture(Provider::Gitlab, &url, dir.path());
        let mut client = client(dir.path(), &remote);
        client.no_cache = false;
        client.write("project-icon-v1", &Project::default());
        let project = client.project(false).unwrap();
        assert_eq!(
            project.avatar.as_deref(),
            Some("https://code.example/gitlab/uploads/p.png")
        );
        assert_eq!(server.join().unwrap().len(), 1);
        assert_eq!(client.project(false).unwrap().avatar, project.avatar);
    }

    #[test]
    fn gitlab_private_avatar_uses_only_api_credentials_and_partitions_cache() {
        let (url, server) = raw_server(vec![(200, String::new(), PROJECT_PHOTO.to_vec())]);
        let dir = tempfile::tempdir().unwrap();
        let mut remote = fixture(Provider::Gitlab, &url, dir.path());
        remote.endpoint =
            format!("{url}/gitlab/api/v4/projects/team%2Fproject/repository/branches");
        let mut client = client(dir.path(), &remote);
        client.no_cache = false;
        let project = Project {
            web_url: None,
            avatar: Some("https://unrelated.example/private.png".into()),
        };
        assert_eq!(
            client.project_avatar(&project).as_deref(),
            Some(PROJECT_PHOTO)
        );
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("GET /gitlab/api/v4/projects/team%2Fproject/avatar "));
        assert!(
            requests[0]
                .to_ascii_lowercase()
                .contains("authorization: bearer fixture-only")
        );
        // The API socket is gone: this must reuse the credential-bound image.
        assert_eq!(
            client.project_avatar(&project).as_deref(),
            Some(PROJECT_PHOTO)
        );
        let endpoint = format!("{}/avatar", root(&remote));
        let advertised = project.avatar.as_deref().unwrap();
        let cache = client.photo_cache().unwrap();
        assert!(cache.image(advertised).is_none());
        assert!(
            cache
                .image(&project_avatar_key(
                    &endpoint,
                    Some("another-account"),
                    advertised
                ))
                .is_none()
        );
        assert!(
            cache
                .image(&project_avatar_key(&endpoint, None, advertised))
                .is_none()
        );
        assert_eq!(cache.stats().unwrap().images, 1);
        assert_eq!(cache.clear().unwrap().images, 0);
        assert!(
            cache
                .image(&project_avatar_key(
                    &endpoint,
                    client.token.as_deref(),
                    advertised
                ))
                .is_none()
        );
    }

    #[test]
    fn gitlab_avatar_redirect_never_forwards_token_and_can_use_public_cache() {
        use std::io::{BufRead, Write};
        let target = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        let location = format!(
            "Location: http://{}/stolen\r\n",
            target.local_addr().unwrap()
        );
        let api = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", api.local_addr().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let remote = fixture(Provider::Gitlab, &url, dir.path());
        let mut client = client(dir.path(), &remote);
        client.no_cache = false;
        let project = Project {
            web_url: None,
            avatar: Some("https://public.example/p.png".into()),
        };
        let photos = super::super::PhotoCache::at(dir.path().join("photos"));
        let server = std::thread::spawn(move || {
            let (mut stream, _) = api.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                request.push_str(&line);
            }
            // Make a public cache entry available only after the API request;
            // an existing entry now correctly bypasses this request entirely.
            assert!(photos.store_image("https://public.example/p.png", PROJECT_PHOTO));
            write!(
                stream,
                "HTTP/1.1 302 Found\r\n{location}Content-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            request
        });
        assert_eq!(
            client.project_avatar(&project).as_deref(),
            Some(PROJECT_PHOTO)
        );
        assert!(
            server
                .join()
                .unwrap()
                .to_ascii_lowercase()
                .contains("authorization: bearer fixture-only")
        );
        assert!(
            matches!(target.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
    }

    #[test]
    fn gitlab_project_icon_reopens_from_disk_offline_and_remains_private_and_clearable() {
        let (url, server) = raw_server(vec![(200, String::new(), PROJECT_PHOTO.to_vec())]);
        let dir = tempfile::tempdir().unwrap();
        let remote = fixture(Provider::Gitlab, &url, dir.path());
        let project = Project {
            web_url: Some("https://code.example/project".into()),
            avatar: Some("https://private.example/icon.png".into()),
        };
        {
            let mut first = client(dir.path(), &remote);
            first.no_cache = false;
            first.write("project-icon-v2", &project);
            assert_eq!(
                first.project_avatar(&project).as_deref(),
                Some(PROJECT_PHOTO)
            );
        }
        assert_eq!(server.join().unwrap().len(), 1);
        let mut reopened = client(dir.path(), &remote);
        reopened.no_cache = false;
        assert_eq!(reopened.cached_project().unwrap(), project);
        assert_eq!(
            reopened.cached_project_avatar(&project).as_deref(),
            Some(PROJECT_PHOTO)
        );
        reopened.token = Some("different-account".into());
        assert!(reopened.cached_project_avatar(&project).is_none());
        reopened.token = Some("fixture-only".into());
        reopened.photo_cache().unwrap().clear().unwrap();
        assert!(reopened.cached_project_avatar(&project).is_none());
    }

    #[test]
    fn cached_project_icon_is_streamed_while_request_listing_is_still_blocked() {
        use std::io::{BufRead, Write};
        let api = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", api.local_addr().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let mut remote = fixture(Provider::Gitlab, &url, dir.path());
        // Exercise the REST-only installation fallback while its list is blocked.
        remote.endpoint = format!("{url}/rest-only/repository/branches");
        let project = Project {
            web_url: Some("https://code.example/project".into()),
            avatar: Some("https://private.example/icon.png".into()),
        };
        let mut source = client(dir.path(), &remote);
        source.no_cache = false;
        source.write("project-icon-v2", &project);
        let key = project_avatar_key(
            &format!("{}/avatar", root(&remote)),
            source.token.as_deref(),
            project.avatar.as_deref().unwrap(),
        );
        assert!(
            source
                .photo_cache()
                .unwrap()
                .store_image(&key, PROJECT_PHOTO)
        );
        let (release, wait) = std::sync::mpsc::channel();
        let (requested, request) = std::sync::mpsc::channel();
        let (events, delivered) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let server = scope.spawn(move || {
                let (mut stream, _) = api.accept().unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                }
                requested.send(first).unwrap();
                wait.recv_timeout(Duration::from_secs(5)).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]"
                )
                .unwrap();
            });
            let worker = scope.spawn(move || {
                source.summary_stream(false, || true, |event| events.send(event).is_ok())
            });
            assert!(matches!(
                delivered.recv_timeout(Duration::from_secs(5)).unwrap(),
                SummaryEvent::Project(_)
            ));
            match delivered.recv_timeout(Duration::from_secs(5)).unwrap() {
                SummaryEvent::Image(bytes) => assert_eq!(bytes, PROJECT_PHOTO),
                _ => panic!("cached image must arrive before listing"),
            }
            assert!(
                request
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .contains("/merge_requests?")
            );
            assert!(
                delivered.try_recv().is_err(),
                "request list is still held by fixture"
            );
            release.send(()).unwrap();
            assert!(
                matches!(delivered.recv_timeout(Duration::from_secs(5)).unwrap(),SummaryEvent::Requests(index) if index.is_empty())
            );
            worker.join().unwrap();
            server.join().unwrap();
        });
    }

    #[test]
    #[ignore = "fresh provider client reopening cached GitLab header without HTTP"]
    fn benchmark_project_icon_reopen() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let remote = fixture(
            Provider::Gitlab,
            &format!("http://{}", listener.local_addr().unwrap()),
            dir.path(),
        );
        let project = Project {
            web_url: Some("https://code.example/project".into()),
            avatar: Some("https://private.example/icon.png".into()),
        };
        let mut seed = client(dir.path(), &remote);
        seed.no_cache = false;
        seed.write("project-icon-v2", &project);
        let key = project_avatar_key(
            &format!("{}/avatar", root(&remote)),
            seed.token.as_deref(),
            project.avatar.as_deref().unwrap(),
        );
        assert!(seed.photo_cache().unwrap().store_image(&key, PROJECT_PHOTO));
        for sample in -1..15 {
            let start = std::time::Instant::now();
            let mut reopened = client(dir.path(), &remote);
            reopened.no_cache = false;
            let cached = reopened.cached_project().unwrap();
            let image = reopened.cached_project_avatar(&cached).unwrap();
            let reopen_cached_ms = start.elapsed().as_secs_f64() * 1000.;
            assert_eq!(cached, project);
            assert_eq!(image, PROJECT_PHOTO);
            if sample >= 0 {
                println!(
                    "{}",
                    json!({"sample":sample,"reopen_cached_ms":reopen_cached_ms})
                );
            }
        }
        assert!(
            matches!(listener.accept(),Err(error) if error.kind()==std::io::ErrorKind::WouldBlock)
        );
    }

    #[test]
    fn gitlab_project_avatar_reuses_public_cache_without_api_request() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let remote = fixture(
            Provider::Gitlab,
            &format!("http://{}", listener.local_addr().unwrap()),
            dir.path(),
        );
        let mut client = client(dir.path(), &remote);
        client.no_cache = false;
        let project = Project {
            web_url: None,
            avatar: Some("https://public.example/p.png".into()),
        };
        client
            .photo_cache()
            .unwrap()
            .store_image(project.avatar.as_deref().unwrap(), PROJECT_PHOTO);
        assert_eq!(
            client.project_avatar(&project).as_deref(),
            Some(PROJECT_PHOTO)
        );
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
    }

    #[test]
    #[ignore = "one warm-up and 15 controlled GitLab project icon samples"]
    fn benchmark_gitlab_project_icon() {
        let (url, server) = raw_server(
            (0..16)
                .map(|_| (200, String::new(), PROJECT_PHOTO.to_vec()))
                .collect(),
        );
        let dir = tempfile::tempdir().unwrap();
        let remote = fixture(Provider::Gitlab, &url, dir.path());
        let mut client = client(dir.path(), &remote);
        client.no_cache = false;
        for sample in 0..16 {
            let started = std::time::Instant::now();
            for _ in 0..1000 {
                std::hint::black_box(
                    project_avatar_url(
                        &remote,
                        Some("https://code.example/gitlab/team/project"),
                        "uploads/p.png",
                    )
                    .unwrap(),
                );
            }
            let normalize_1000_ms = started.elapsed().as_secs_f64() * 1000.;
            let project = Project {
                web_url: Some("https://code.example/gitlab/team/project".into()),
                avatar: Some(format!("https://code.example/gitlab/uploads/{sample}.png")),
            };
            client.write("project-icon-v2", &project);
            let started = std::time::Instant::now();
            let cached = client.project(false).unwrap();
            let metadata_cache_ms = started.elapsed().as_secs_f64() * 1000.;
            assert_eq!(cached.avatar, project.avatar);
            let started = std::time::Instant::now();
            let downloaded = client.project_avatar(&project).unwrap();
            let api_icon_ms = started.elapsed().as_secs_f64() * 1000.;
            let started = std::time::Instant::now();
            let cached = client.project_avatar(&project).unwrap();
            let cached_icon_ms = started.elapsed().as_secs_f64() * 1000.;
            assert_eq!(downloaded, PROJECT_PHOTO);
            assert_eq!(cached, downloaded);
            if sample != 0 {
                println!(
                    "{}",
                    json!({"sample":sample,"normalize_1000_ms":normalize_1000_ms,
                    "metadata_cache_ms":metadata_cache_ms,"api_icon_ms":api_icon_ms,"cached_icon_ms":cached_icon_ms})
                );
            }
        }
        assert_eq!(server.join().unwrap().len(), 16);
        assert_eq!(client.photo_cache().unwrap().stats().unwrap().images, 16);
        assert_eq!(client.photo_cache().unwrap().clear().unwrap().images, 0);
    }

    #[test]
    fn provider_pagination_project_icons_and_offline_cache_reuse() {
        for provider in [Provider::Github, Provider::Gitlab] {
            let (project, first, second, header) = if provider == Provider::Github {
                (
                    json!({"html_url":"https://example.invalid/team/project","owner":{"avatar_url":"https://images.example.invalid/a.png"}}),
                    json!([{"number":1,"head":{"ref":"one","repo":{"full_name":"team/project"}},"html_url":"https://example.invalid/pull/1"}]),
                    json!([{"number":2,"head":{"ref":"two","repo":{"full_name":"team/project"}},"html_url":"https://example.invalid/pull/2"}]),
                    "Link: <http://example.invalid/?page=2>; rel=\"next\"\r\n",
                )
            } else {
                (
                    json!({"web_url":"https://example.invalid/team/project","avatar_url":"https://images.example.invalid/a.png"}),
                    json!([{"iid":1,"source_branch":"one","source_project_id":7,"target_project_id":7,"web_url":"https://example.invalid/merge_requests/1"}]),
                    json!([{"iid":2,"source_branch":"two","source_project_id":7,"target_project_id":7,"web_url":"https://example.invalid/merge_requests/2"}]),
                    "X-Next-Page: 2\r\n",
                )
            };
            let mut responses = vec![(200, "", project)];
            if provider == Provider::Gitlab {
                responses.push((404, "", json!({})));
            }
            responses.extend([(200, header, first), (200, "", second)]);
            let (url, server) = server(responses);
            let dir = tempfile::tempdir().unwrap();
            let remote = fixture(provider, &url, dir.path());
            let mut client = client(dir.path(), &remote);
            client.no_cache = false;
            let project = client.project(false).unwrap();
            assert_eq!(
                project.avatar.as_deref(),
                Some("https://images.example.invalid/a.png")
            );
            let requests = client.open_requests(false).unwrap();
            assert_eq!(requests.len(), 2);
            let captured = server.join().unwrap();
            let first_page = 1 + usize::from(provider == Provider::Gitlab);
            assert!(captured[first_page].contains("per_page=100&page=1"));
            assert!(captured[first_page + 1].contains("per_page=100&page=2"));
            // The listening socket is closed: these must read disk, never HTTP.
            assert_eq!(client.open_requests(false).unwrap(), requests);
            assert_eq!(client.project(false).unwrap().avatar, project.avatar);
        }
    }

    #[test]
    fn failed_request_page_never_installs_or_caches_partial_index() {
        let (url, server) = server(vec![
            (
                200,
                "Link: <http://example.invalid/?page=2>; rel=\"next\"\r\n",
                json!([
                    {"number":1,"head":{"ref":"one","repo":{"full_name":"team/project"}},"html_url":"https://example.invalid/pull/1"}
                ]),
            ),
            (502, "", json!({})),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let remote = fixture(Provider::Github, &url, dir.path());
        let mut client = client(dir.path(), &remote);
        client.no_cache = false;
        assert!(client.open_requests(false).is_err());
        server.join().unwrap();
        assert!(client.read::<Index>("open-requests-v1", TTL).is_none());
    }

    #[test]
    fn explicit_refresh_bypasses_fresh_cache_and_removes_closed_request() {
        let (url, server) = server(vec![
            (
                200,
                "",
                json!([
                    {"number":1,"head":{"ref":"one","repo":{"full_name":"team/project"}},"html_url":"https://example.invalid/pull/1"}
                ]),
            ),
            (200, "", json!([])),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let remote = fixture(Provider::Github, &url, dir.path());
        let mut client = client(dir.path(), &remote);
        client.no_cache = false;
        assert_eq!(client.open_requests(false).unwrap().len(), 1);
        assert_eq!(client.open_requests(false).unwrap().len(), 1);
        assert!(client.open_requests(true).unwrap().is_empty());
        assert_eq!(server.join().unwrap().len(), 2);
    }

    #[test]
    fn oversized_page_is_rejected_without_caching() {
        let (url, server) = server(vec![(200, "", json!(vec![json!({}); 101]))]);
        let dir = tempfile::tempdir().unwrap();
        let remote = fixture(Provider::Github, &url, dir.path());
        let mut client = client(dir.path(), &remote);
        client.no_cache = false;
        assert!(
            client
                .open_requests(false)
                .unwrap_err()
                .contains("requested size")
        );
        server.join().unwrap();
        assert!(client.read::<Index>("open-requests-v1", TTL).is_none());
    }

    #[test]
    #[ignore = "controlled cached provider summary component timing"]
    fn benchmark_provider_cache() {
        let dir = tempfile::tempdir().unwrap();
        let remote = fixture(Provider::Github, "http://127.0.0.1:1", dir.path());
        let mut client = client(dir.path(), &remote);
        client.no_cache = false;
        let index: Index = (0..10000)
            .map(|n| {
                (
                    format!("feature/{n}"),
                    vec![Association {
                        number: n + 1,
                        title: "Benchmark request".into(),
                        url: format!("https://github.com/team/project/pull/{}", n + 1),
                    }],
                )
            })
            .collect();
        client.write("open-requests-v1", &index);
        let mut samples = Vec::new();
        for sample in 0..16 {
            let started = std::time::Instant::now();
            let actual = client.open_requests(false).unwrap();
            let elapsed = started.elapsed().as_secs_f64() * 1000.;
            assert_eq!(actual, index);
            if sample != 0 {
                samples.push(elapsed);
            }
        }
        println!("provider_10000_requests_disk_ms={samples:?}");
    }
    #[test]
    fn links_support_ssh_ports_subgroups_and_encoded_branch_names() {
        let gh = Remote::parse("origin", "git@github.com:team/project.git", None).unwrap();
        assert_eq!(
            branch_url(&gh, None, "feature/a#b").unwrap(),
            "https://github.com/team/project/tree/feature%2Fa%23b"
        );
        let gl = Remote::parse(
            "upstream",
            "ssh://git@gitlab.example:2222/group/sub/project.git",
            Some("gitlab"),
        )
        .unwrap();
        assert_eq!(
            commit_url(
                &gl,
                Some("https://gitlab.example/prefix/group/sub/project"),
                "abc123"
            )
            .unwrap(),
            "https://gitlab.example/prefix/group/sub/project/-/commit/abc123"
        );
        assert_eq!(
            branch_url(&gl, None, "main").unwrap(),
            "https://gitlab.example/group/sub/project/-/tree/main"
        );
        assert!(commit_url(&gh, None, "../../bad").is_none());
    }

    #[test]
    fn indexes_only_same_project_heads_and_keeps_multiple_targets() {
        for provider in [Provider::Github, Provider::Gitlab] {
            let remote = Remote::parse(
                "origin",
                "https://example.invalid/team/project.git",
                Some(provider.key()),
            )
            .unwrap();
            let values = if provider == Provider::Github {
                vec![
                    json!({"number":1,"head":{"ref":"feature/a","repo":{"full_name":"TEAM/project"}},"html_url":"https://example.invalid/pull/1"}),
                    json!({"number":2,"head":{"ref":"feature/a","repo":{"full_name":"team/project"}},"html_url":"https://example.invalid/pull/2"}),
                    json!({"number":3,"head":{"ref":"feature/a","repo":{"full_name":"fork/project"}},"html_url":"https://example.invalid/pull/3"}),
                ]
            } else {
                vec![
                    json!({"iid":1,"source_branch":"feature/a","source_project_id":7,"target_project_id":7,"web_url":"https://example.invalid/merge_requests/1"}),
                    json!({"iid":2,"source_branch":"feature/a","source_project_id":7,"target_project_id":7,"web_url":"https://example.invalid/merge_requests/2"}),
                    json!({"iid":3,"source_branch":"feature/a","source_project_id":8,"target_project_id":7,"web_url":"https://example.invalid/merge_requests/3"}),
                ]
            };
            let mut index = Index::new();
            append_requests(&remote, &values, &mut index);
            append_requests(&remote, &values, &mut index);
            assert_eq!(index.len(), 1);
            assert_eq!(index["feature/a"].len(), 2);
        }
    }
}
