//! Small, read-only GitLab badge queries; complete results or REST fallback.
use super::*;
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

const QUERY: &str = r#"query ProviderBadgeSummary($project: ID!, $after: String) {
  project(fullPath: $project) {
    mergeRequests(state: opened, first: 100, after: $after) {
      nodes { iid title webUrl sourceBranch sourceProjectId targetProjectId }
      pageInfo { hasNextPage endCursor }
    }
  }
}"#;
type Failures = Mutex<HashMap<(String, u64), Instant>>;
static FAILED: LazyLock<Failures> = LazyLock::new(Default::default);

fn endpoint(remote: &Remote) -> Option<String> {
    if remote.provider != Provider::Gitlab {
        return None;
    }
    let mut url = url::Url::parse(&remote.endpoint).ok()?;
    let prefix = url.path().split_once("/api/v4/projects/")?.0.to_owned();
    let loopback = cfg!(test) && url.scheme() == "http" && url.host_str() == Some("127.0.0.1");
    if (url.scheme() != "https" && !loopback)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    url.set_path(&format!("{prefix}/api/graphql"));
    Some(url.into())
}

pub(super) fn load(client: &mut Client<'_>) -> Result<Index, &'static str> {
    let url = endpoint(client.remote).ok_or("unsupported_endpoint")?;
    let mut identity = std::collections::hash_map::DefaultHasher::new();
    client.token.hash(&mut identity);
    let key = (url.clone(), identity.finish());
    {
        let mut failed = FAILED.lock().unwrap_or_else(|e| e.into_inner());
        failed.retain(|_, time| time.elapsed() < Duration::from_secs(60));
        if failed.contains_key(&key) {
            return Err("cooldown");
        }
    }
    let result = load_pages(client, &url);
    if result.is_err() {
        FAILED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, Instant::now());
    }
    result
}

fn load_pages(client: &mut Client<'_>, url: &str) -> Result<Index, &'static str> {
    let started = Instant::now();
    let mut cursor: Option<String> = None;
    let mut cursors = HashSet::new();
    let mut index = Index::new();
    let mut total_bytes = 0;
    for _ in 1..=100 {
        let body = request(client, url, cursor.as_deref())?;
        total_bytes += body.len();
        if total_bytes > 32 * 1024 * 1024 || started.elapsed() > Duration::from_secs(60) {
            return Err("size_or_time_limit");
        }
        let response: Envelope = serde_json::from_slice(&body).map_err(|_| "invalid_response")?;
        if response.errors.is_some_and(|errors| !errors.is_empty()) {
            return Err("graphql_errors");
        }
        let connection = response
            .data
            .and_then(|data| data.project)
            .and_then(|project| project.merge_requests)
            .ok_or("missing_data")?;
        if connection.nodes.len() > 100 {
            return Err("oversized_page");
        }
        let empty = connection.nodes.is_empty();
        for node in connection.nodes {
            let number = node.iid.parse::<u64>().map_err(|_| "invalid_iid")?;
            if number == 0 || node.target_project_id == 0 {
                return Err("invalid_identity");
            }
            // Reuse REST validation: fork exclusion, URLs, duplicates and title bounds.
            append_requests(
                client.remote,
                &[json!({
                    "iid": number, "title": node.title, "web_url": node.web_url,
                    "source_branch": node.source_branch, "source_project_id": node.source_project_id,
                    "target_project_id": node.target_project_id
                })],
                &mut index,
            );
        }
        if !connection.page_info.has_next_page {
            if serde_json::to_vec(&index)
                .map_err(|_| "invalid_index")?
                .len()
                > 4 * 1024 * 1024
            {
                return Err("oversized_index");
            }
            return Ok(index);
        }
        let next = connection
            .page_info
            .end_cursor
            .filter(|s| !s.is_empty() && s.len() <= 8192)
            .ok_or("missing_cursor")?;
        if empty || !cursors.insert(next.clone()) {
            return Err("invalid_cursor_progress");
        }
        cursor = Some(next);
    }
    Err("page_limit")
}

fn request(
    client: &mut Client<'_>,
    url: &str,
    cursor: Option<&str>,
) -> Result<Vec<u8>, &'static str> {
    let body = serde_json::to_vec(
        &json!({"query": QUERY, "variables": {"project": client.remote.project, "after": cursor}}),
    )
    .map_err(|_| "invalid_query")?;
    for attempt in 0..2 {
        let mut request = client
            .http
            .agent
            .post(url)
            .header("User-Agent", "git-rustle/0.1")
            .header("Accept", "application/json")
            .header("Content-Type", "application/json");
        if let Some(token) = &client.token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let result = request.send(body.as_slice());
        let mut response = result.map_err(|_| "transport_error")?;
        let status = response.status().as_u16();
        if attempt == 0 && matches!(status, 401 | 403) && client.token.is_none() {
            client.token = helper_token(client.path, client.remote);
            if client.token.is_some() {
                continue;
            }
        }
        if status != 200 {
            return Err("http_error");
        }
        let body = response
            .body_mut()
            .with_config()
            .limit(8 * 1024 * 1024)
            .read_to_vec()
            .map_err(|_| "body_error")?;
        return Ok(body);
    }
    Err("authentication_failed")
}

#[derive(Deserialize)]
struct Envelope {
    data: Option<Data>,
    errors: Option<Vec<Value>>,
}
#[derive(Deserialize)]
struct Data {
    project: Option<ProjectData>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectData {
    merge_requests: Option<Connection>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Connection {
    nodes: Vec<Node>,
    page_info: PageInfo,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Node {
    iid: String,
    title: String,
    web_url: Option<String>,
    source_branch: String,
    source_project_id: Option<u64>,
    target_project_id: u64,
}

#[cfg(test)]
mod tests {
    use super::super::super::tests::{client, fixture, server};
    use super::*;
    fn node(id: u64, branch: &str, source: u64) -> Value {
        json!({"iid":id.to_string(),"title":"Badge","webUrl":format!("https://example.invalid/mr/{id}"),
            "sourceBranch":branch,"sourceProjectId":source,"targetProjectId":7})
    }
    fn page(nodes: Vec<Value>, next: bool, cursor: Option<&str>) -> Value {
        json!({"data":{"project":{"mergeRequests":{"nodes":nodes,"pageInfo":{"hasNextPage":next,"endCursor":cursor}}}}})
    }
    #[test]
    fn graphql_pages_filter_forks_and_reuse_complete_cache() {
        let (url, server) = server(vec![
            (
                200,
                "",
                page(
                    vec![node(1, "one", 7), node(2, "fork", 8)],
                    true,
                    Some("cursor-one"),
                ),
            ),
            (
                200,
                "",
                page(vec![node(3, "two", 7), node(1, "one", 7)], false, None),
            ),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let remote = fixture(Provider::Gitlab, &url, dir.path());
        let mut client = client(dir.path(), &remote);
        client.no_cache = false;
        let index = client.open_requests(true).unwrap();
        assert_eq!(index.len(), 2);
        assert_eq!(index["one"].len(), 1);
        assert_eq!(index["two"][0].number, 3);
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
        for (i, request) in requests.iter().enumerate() {
            assert!(request.starts_with("POST /api/graphql "));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer fixture-only")
            );
            let body: Value =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(body["variables"]["project"], "team/project");
            assert_eq!(
                body["variables"]["after"],
                if i == 0 {
                    Value::Null
                } else {
                    json!("cursor-one")
                }
            );
            assert!(!body["query"].as_str().unwrap().contains("mutation"));
        }
        assert_eq!(client.open_requests(false).unwrap(), index);
    }
    #[test]
    fn graphql_errors_and_partial_pages_fall_back_without_caching_partial_results() {
        for failure in [
            json!({"errors":[{"message":"private server detail"}],"data":{"project":null}}),
            json!({"data":{"project":null}}),
            page(vec![node(9, "partial", 7)], true, Some("again")),
            page(vec![], true, None),
            json!({"data":{"project":{"mergeRequests":{"nodes":[]}}}}),
        ] {
            let (url, server) = server(vec![
                (
                    200,
                    "",
                    page(vec![node(9, "partial", 7)], true, Some("again")),
                ),
                (200, "", failure),
                (
                    200,
                    "",
                    json!([{"iid":4,"title":"REST","source_branch":"rest","source_project_id":7,"target_project_id":7,"web_url":"https://example.invalid/mr/4"}]),
                ),
                (200, "", json!([])),
            ]);
            let dir = tempfile::tempdir().unwrap();
            let remote = fixture(Provider::Gitlab, &url, dir.path());
            let mut client = client(dir.path(), &remote);
            client.no_cache = false;
            let index = client.open_requests(true).unwrap();
            assert_eq!(index.len(), 1);
            assert!(index.contains_key("rest"));
            assert_eq!(
                client.read::<Index>("open-requests-v1", TTL).unwrap(),
                index
            );
            assert!(client.open_requests(true).unwrap().is_empty());
            let requests = server.join().unwrap();
            assert!(requests[2].starts_with("GET /api/v4/"));
            assert!(
                requests[3].starts_with("GET /api/v4/"),
                "failed query must cool down"
            );
        }
    }
    #[test]
    fn graphql_force_refresh_removes_closed_requests() {
        let (url, server) = server(vec![
            (200, "", page(vec![node(1, "one", 7)], false, None)),
            (200, "", page(vec![], false, None)),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let remote = fixture(Provider::Gitlab, &url, dir.path());
        let mut client = client(dir.path(), &remote);
        client.no_cache = false;
        assert_eq!(client.open_requests(false).unwrap().len(), 1);
        assert_eq!(client.open_requests(false).unwrap().len(), 1);
        assert!(client.open_requests(true).unwrap().is_empty());
        assert!(
            client
                .read::<Index>("open-requests-v1", TTL)
                .unwrap()
                .is_empty()
        );
        assert_eq!(server.join().unwrap().len(), 2);
    }

    #[test]
    fn graphql_redirects_fall_back_without_forwarding_credentials() {
        let (url, server) = server(vec![
            (
                302,
                "Location: https://must-not-follow.invalid/secret\r\n",
                json!({}),
            ),
            (200, "", json!([])),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let remote = fixture(Provider::Gitlab, &url, dir.path());
        assert!(
            client(dir.path(), &remote)
                .open_requests(true)
                .unwrap()
                .is_empty()
        );
        assert_eq!(server.join().unwrap().len(), 2);
    }
    #[test]
    fn graphql_endpoint_preserves_configured_installation_prefix() {
        let mut remote =
            Remote::parse("origin", "git@ssh-alias:team/project.git", Some("gitlab")).unwrap();
        remote.endpoint =
            "https://code.example/gitlab/api/v4/projects/team%2Fproject/repository/branches".into();
        assert_eq!(
            endpoint(&remote).as_deref(),
            Some("https://code.example/gitlab/api/graphql")
        );
        for invalid in [
            "http://code.example/api/v4/projects/p",
            "https://user:pass@code.example/api/v4/projects/p",
            "https://code.example/api/v4/projects/p?override=1",
        ] {
            remote.endpoint = invalid.into();
            assert!(endpoint(&remote).is_none());
        }
    }
}
