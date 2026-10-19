//! Read-only provider summaries. Call on workers; rendering uses an in-memory index.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Association {
    pub number: u64,
    pub title: String,
    pub url: String,
}

pub type Index = BTreeMap<String, Vec<Association>>;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Project {
    pub web_url: Option<String>,
    pub avatar: Option<String>,
}

/// GitHub exposes an owner's avatar; GitLab exposes a project avatar.
impl Client<'_> {
    pub fn project(&mut self, force: bool) -> Result<Project, String> {
        if !force && let Some(mut project) = self.read::<Project>("project-icon-v1", 86400) {
            project.web_url = project.web_url.filter(|u| safe_web_url(u));
            project.avatar = project.avatar.filter(|u| safe_image_url(u));
            return Ok(project);
        }
        let response = self.get(root(self.remote))?;
        let value: Value =
            serde_json::from_slice(&response.body).map_err(|_| "Invalid project response")?;
        let (web, avatar) = match self.remote.provider {
            Provider::Github => (&value["html_url"], &value["owner"]["avatar_url"]),
            Provider::Gitlab => (&value["web_url"], &value["avatar_url"]),
        };
        let project = Project {
            web_url: web.as_str().filter(|u| safe_web_url(u)).map(str::to_string),
            avatar: avatar
                .as_str()
                .filter(|u| safe_image_url(u))
                .map(str::to_string),
        };
        self.write("project-icon-v1", &project);
        Ok(project)
    }

    pub fn open_requests(&mut self, force: bool) -> Result<Index, String> {
        if !force && let Some(mut index) = self.read::<Index>("open-requests-v1", TTL) {
            index.retain(|branch, requests| {
                requests.retain(|request| request.number > 0 && safe_web_url(&request.url));
                !branch.is_empty() && !branch.chars().any(char::is_control) && !requests.is_empty()
            });
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
    use super::super::tests::{client, fixture, server};
    use super::*;

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
            let (url, server) = server(vec![
                (200, "", project),
                (200, header, first),
                (200, "", second),
            ]);
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
            assert!(captured[1].contains("per_page=100&page=1"));
            assert!(captured[2].contains("per_page=100&page=2"));
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
