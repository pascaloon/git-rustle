//! Bounded project-level CI snapshots, fetched only by provider workers.
use super::*;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Status {
    Pending,
    Running,
    Success,
    Warning,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Pipeline {
    pub id: u64,
    pub sha: String,
    pub status: Status,
    pub detail: String,
    pub url: String,
}
pub type Index = BTreeMap<String, Pipeline>;

fn status(value: &str) -> Option<Status> {
    Some(match value {
        "created"
        | "waiting_for_resource"
        | "preparing"
        | "pending"
        | "queued"
        | "requested"
        | "waiting" => Status::Pending,
        "running" | "in_progress" => Status::Running,
        "success" => Status::Success,
        "manual" | "scheduled" | "skipped" | "canceled" | "cancelled" | "neutral" | "stale"
        | "action_required" => Status::Warning,
        "failed" | "failure" | "timed_out" | "startup_failure" | "error" => Status::Failed,
        _ => return None,
    })
}

fn valid(pipeline: &Pipeline) -> bool {
    pipeline.id > 0
        && matches!(pipeline.sha.len(), 40 | 64)
        && pipeline.sha.bytes().all(|b| b.is_ascii_hexdigit())
        && safe_web_url(&pipeline.url)
}

impl Client<'_> {
    /// One paginated listing per project, never one request per graph row/branch.
    pub fn pipelines(&mut self, force: bool) -> Result<Index, String> {
        if !force && let Some(mut index) = self.read::<Index>("pipelines-v1", 60) {
            index.retain(|branch, pipeline| !branch.is_empty() && valid(pipeline));
            return Ok(index);
        }
        let mut index = Index::new();
        let started = std::time::Instant::now();
        let mut total_bytes = 0usize;
        let mut workflows = HashSet::new();
        for page in 1..=20 {
            let url = match self.remote.provider {
                Provider::Gitlab => format!(
                    "{}/pipelines?scope=branches&order_by=id&sort=desc&per_page=100&page={page}",
                    root(self.remote)
                ),
                Provider::Github => format!(
                    "{}/actions/runs?per_page=100&page={page}",
                    root(self.remote)
                ),
            };
            let response = self.get(&url)?;
            total_bytes += response.body.len();
            if total_bytes > 4 * 1024 * 1024 || started.elapsed() > Duration::from_secs(30) {
                return Err("Pipeline listing exceeded its size or time limit".into());
            }
            let next = response.next;
            let value: Value =
                serde_json::from_slice(&response.body).map_err(|_| "Invalid pipeline response")?;
            let values = match self.remote.provider {
                Provider::Gitlab => value.as_array(),
                Provider::Github => value["workflow_runs"].as_array(),
            }
            .ok_or("Invalid pipeline list")?;
            if values.len() > 100 {
                return Err("Pipeline page exceeded its requested size".into());
            }
            for value in values {
                let (branch, sha, detail, url) = match self.remote.provider {
                    Provider::Gitlab => (
                        value["ref"].as_str(),
                        value["sha"].as_str(),
                        value["status"].as_str(),
                        value["web_url"].as_str(),
                    ),
                    Provider::Github => (
                        value["head_branch"].as_str(),
                        value["head_sha"].as_str(),
                        value["conclusion"].as_str().or(value["status"].as_str()),
                        value["html_url"].as_str(),
                    ),
                };
                let (Some(branch), Some(sha), Some(detail), Some(url), Some(id)) =
                    (branch, sha, detail, url, value["id"].as_u64())
                else {
                    continue;
                };
                let Some(status) = status(detail) else {
                    continue;
                };
                if self.remote.provider == Provider::Github
                    && let Some(workflow) = value["workflow_id"].as_u64()
                    && !workflows.insert((branch.to_string(), sha.to_string(), workflow))
                {
                    continue;
                }
                if branch.is_empty() || branch.len() > 4096 || branch.chars().any(char::is_control)
                {
                    continue;
                }
                let pipeline = Pipeline {
                    id,
                    sha: sha.to_ascii_lowercase(),
                    status,
                    detail: detail.into(),
                    url: url.into(),
                };
                if !valid(&pipeline) {
                    continue;
                }
                // Pages are newest first. Multiple workflows on the same SHA
                // retain a failure/running result rather than hide it behind success.
                match index.entry(branch.into()) {
                    std::collections::btree_map::Entry::Vacant(slot) => {
                        slot.insert(pipeline);
                    }
                    std::collections::btree_map::Entry::Occupied(mut slot) => {
                        if self.remote.provider == Provider::Github
                            && slot.get().sha == pipeline.sha
                            && priority(pipeline.status) > priority(slot.get().status)
                        {
                            slot.insert(pipeline);
                        }
                    }
                }
            }
            if !next.unwrap_or(values.len() == 100) {
                self.write("pipelines-v1", &index);
                return Ok(index);
            }
        }
        // Keep useful bounded coverage on very large/active projects. Display
        // still requires an exact latest-head SHA; unseen branches get no icon.
        self.write("pipelines-v1", &index);
        Ok(index)
    }
}

fn priority(status: Status) -> u8 {
    match status {
        Status::Success => 0,
        Status::Warning => 1,
        Status::Pending => 2,
        Status::Running => 3,
        Status::Failed => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{client, fixture, server};

    #[test]
    fn page_limit_retains_valid_partial_coverage_instead_of_hiding_all_pipelines() {
        let replies=(1..=20).map(|page|(200,"X-Next-Page: 99\r\n",json!([{"id":page,"ref":format!("branch/{page}"),"sha":"a".repeat(40),"status":"running","web_url":format!("https://example.invalid/pipelines/{page}")}]))).collect();
        let (url, handle) = server(replies);
        let directory = tempfile::tempdir().unwrap();
        let remote = fixture(Provider::Gitlab, &url, directory.path());
        let index = client(directory.path(), &remote).pipelines(false).unwrap();
        assert_eq!(index.len(), 20);
        assert_eq!(handle.join().unwrap().len(), 20);
    }

    #[test]
    #[ignore = "one warmup plus 15 provider pipeline listing and cache samples"]
    fn benchmark_pipeline_snapshot() {
        let page:Vec<_>=(0..100).map(|n|json!({"id":n+1,"ref":format!("feature/{n}"),"sha":"a".repeat(40),"status":"running","web_url":format!("https://example.invalid/pipelines/{}",n+1)})).collect();
        let replies = (0..16)
            .map(|_| (200, "X-Next-Page: \r\n", json!(page)))
            .collect();
        let (url, handle) = server(replies);
        let directory = tempfile::tempdir().unwrap();
        let remote = fixture(Provider::Gitlab, &url, directory.path());
        let mut client = client(directory.path(), &remote);
        client.no_cache = false;
        for sample in -1..15 {
            let start = std::time::Instant::now();
            assert_eq!(client.pipelines(true).unwrap().len(), 100);
            let worker_ms = start.elapsed().as_secs_f64() * 1000.;
            let start = std::time::Instant::now();
            assert_eq!(client.pipelines(false).unwrap().len(), 100);
            let cache_ms = start.elapsed().as_secs_f64() * 1000.;
            if sample >= 0 {
                println!(
                    "{}",
                    json!({"sample":sample,"worker_ms":worker_ms,"cache_ms":cache_ms})
                );
            }
        }
        assert_eq!(handle.join().unwrap().len(), 16);
    }
    use super::*;

    #[test]
    fn gitlab_pipeline_pages_keep_latest_and_reuse_credential_partitioned_cache() {
        let sha = "1".repeat(40);
        let (url, handle) = server(vec![
            (
                200,
                "X-Next-Page: 2\r\n",
                json!([{"id":9,"ref":"feature/a","sha":sha,"status":"running","web_url":"https://example.invalid/pipelines/9"}]),
            ),
            (
                200,
                "X-Next-Page: \r\n",
                json!([{"id":8,"ref":"feature/a","sha":sha,"status":"success","web_url":"https://example.invalid/pipelines/8"},{"id":7,"ref":"main","sha":sha,"status":"manual","web_url":"https://example.invalid/pipelines/7"}]),
            ),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let remote = fixture(Provider::Gitlab, &url, dir.path());
        let mut client = client(dir.path(), &remote);
        client.no_cache = false;
        let index = client.pipelines(false).unwrap();
        assert_eq!(index["feature/a"].id, 9);
        assert_eq!(index["feature/a"].status, Status::Running);
        assert_eq!(index["main"].status, Status::Warning);
        let requests = handle.join().unwrap();
        assert!(requests.iter().all(|r| {
            r.starts_with("GET /api/v4/projects/team%2Fproject/pipelines?scope=branches&")
        }));
        assert!(requests.iter().all(|r| {
            r.to_lowercase()
                .contains("authorization: bearer fixture-only")
        }));
        assert_eq!(
            client.pipelines(false).unwrap(),
            index,
            "closed loopback socket proves cache reuse"
        );
        client.token = Some("other-fixture-account".into());
        assert!(client.read::<Index>("pipelines-v1", 60).is_none());
    }

    #[test]
    fn github_runs_aggregate_failure_on_latest_sha_and_validate_urls() {
        let sha = "a".repeat(40);
        let (url, handle) = server(vec![(
            200,
            "",
            json!({"workflow_runs":[
                {"id":9,"head_branch":"main","head_sha":sha,"status":"completed","conclusion":"success","html_url":"https://example.invalid/runs/9"},
                {"id":8,"head_branch":"main","head_sha":sha,"status":"completed","conclusion":"failure","html_url":"https://example.invalid/runs/8"},
                {"id":7,"head_branch":"main","head_sha":"b".repeat(40),"conclusion":"success","html_url":"https://example.invalid/runs/7"},
                {"id":6,"head_branch":"unsafe","head_sha":sha,"status":"queued","html_url":"javascript:alert(1)"}
            ]}),
        )]);
        let dir = tempfile::tempdir().unwrap();
        let remote = fixture(Provider::Github, &url, dir.path());
        let index = client(dir.path(), &remote).pipelines(false).unwrap();
        assert_eq!(index.len(), 1);
        assert_eq!(index["main"].status, Status::Failed);
        assert!(handle.join().unwrap()[0].starts_with("GET /repos/team/project/actions/runs?"));
    }

    #[test]
    fn statuses_have_distinct_states_and_unknown_is_not_success() {
        for (value, expected) in [
            ("pending", Status::Pending),
            ("running", Status::Running),
            ("success", Status::Success),
            ("manual", Status::Warning),
            ("failed", Status::Failed),
        ] {
            assert_eq!(status(value), Some(expected));
        }
        assert_eq!(status("something-new"), None);
    }

    #[test]
    fn saved_global_token_authenticates_two_separate_repository_clones() {
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = crate::credentials::delete(&self.0);
            }
        }
        let first = tempfile::tempdir().unwrap();
        let probe = format!(
            "https://example.invalid/{}/api/v4",
            first.path().file_name().unwrap().to_string_lossy()
        );
        let _probe_cleanup = Cleanup(probe.clone());
        if !crate::credentials::write_fixture(&probe, "synthetic-clone-availability-probe") {
            return;
        }
        let (url, handle) = server(vec![(200, "", json!([])), (200, "", json!([]))]);
        let second = tempfile::tempdir().unwrap();
        let remote = fixture(Provider::Gitlab, &url, first.path());
        command::checked(second.path(), &["init", "-q"]).unwrap();
        let base = crate::provider_settings::credential_base(&remote).unwrap();
        let _cleanup = Cleanup(base.into());
        crate::credentials::write(base, "synthetic-clone-global-token").unwrap();
        for path in [first.path(), second.path()] {
            assert!(
                Client::new(path, &remote, true)
                    .unwrap()
                    .pipelines(false)
                    .unwrap()
                    .is_empty()
            );
        }
        assert!(handle.join().unwrap().iter().all(|request| {
            request
                .to_lowercase()
                .contains("authorization: bearer synthetic-clone-global-token")
        }));
    }
}
