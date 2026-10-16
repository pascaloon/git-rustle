//! Immutable, on-demand API diffs. No branch fetch or Git ref writes.
use super::*;
use crate::diff::{ChangeKind, FileChange, FileDiff, Stat, TreeDiff};
use gix::ObjectId;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFile {
    pub remote: Arc<Remote>,
    pub old: Option<ObjectId>,
    pub new: ObjectId,
    pub stat: Option<Stat>,
    pub no_cache: bool,
    pub submodule: bool,
    pub blob: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct CachedResponse {
    url: String,
    next: Option<bool>,
    body: Vec<u8>,
}

struct Session<'a> {
    repo: &'a gix::Repository,
    remote: &'a Remote,
    http: Http,
    token: Option<String>,
    no_cache: bool,
}

impl<'a> Session<'a> {
    fn new(repo: &'a gix::Repository, remote: &'a Remote, no_cache: bool) -> Result<Self, String> {
        let path = repo.workdir().unwrap_or_else(|| repo.git_dir());
        Ok(Self {
            repo,
            remote,
            http: Http::new(),
            token: cached_token(path, &crate::git::git_dir(repo), remote)?,
            no_cache,
        })
    }

    fn location(&self, url: &str, accept: &str) -> Option<PathBuf> {
        if self.no_cache {
            return None;
        }
        let (base, _) = cache_file(
            &crate::git::git_dir(self.repo),
            self.remote,
            self.token.as_deref(),
        )?;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (url, accept).hash(&mut h);
        Some(
            base.with_extension("files")
                .join(format!("{:016x}.json", h.finish())),
        )
    }

    fn get(&mut self, url: &str, accept: &str) -> Result<Response, String> {
        if let Some(path) = self.location(url, accept)
            && std::fs::metadata(&path).is_ok_and(|m| m.len() <= MAX_CACHE)
            && let Ok(bytes) = std::fs::read(path)
            && let Ok(cached) = serde_json::from_slice::<CachedResponse>(&bytes)
            && cached.url == url
            && cached.body.len() <= 8 * 1024 * 1024
        {
            return Ok(Response {
                status: 200,
                etag: None,
                next: cached.next,
                body: cached.body,
            });
        }
        let mut response =
            self.http
                .get_url_accept(self.remote, url, None, self.token.as_deref(), accept)?;
        if matches!(response.status, 401 | 403 | 404) && self.token.is_none() {
            self.token = helper_token(
                self.repo.workdir().unwrap_or_else(|| self.repo.git_dir()),
                self.remote,
            );
            if self.token.is_some() {
                response = self.http.get_url_accept(
                    self.remote,
                    url,
                    None,
                    self.token.as_deref(),
                    accept,
                )?;
            }
        }
        if response.status != 200 {
            return Err(format!(
                "Remote diff: HTTP {} (check provider access in settings)",
                response.status
            ));
        }
        if let Some(path) = self.location(url, accept) {
            let data = CachedResponse {
                url: url.into(),
                next: response.next,
                body: response.body.clone(),
            };
            if let Ok(bytes) = serde_json::to_vec(&data) {
                let _ = std::fs::create_dir_all(path.parent().unwrap());
                let tmp = path.with_extension("tmp");
                if std::fs::write(&tmp, bytes).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
                let _ = std::fs::remove_file(tmp);
            }
        }
        Ok(response)
    }
}

fn file_path(v: &serde_json::Value, key: &str) -> Result<String, String> {
    let path = v[key].as_str().ok_or("File API omitted a path")?;
    if path.is_empty()
        || path.len() > 8192
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err("Invalid file path in API response".into());
    }
    Ok(path.to_string())
}

pub fn tree(
    repo: &gix::Repository,
    remote: &Remote,
    old: Option<ObjectId>,
    new: ObjectId,
    no_cache: bool,
) -> Result<TreeDiff, String> {
    let mut client = Session::new(repo, remote, no_cache)?;
    let root = remote
        .endpoint
        .strip_suffix("/branches")
        .ok_or("Invalid API endpoint")?;
    let remote = Arc::new(remote.clone());
    let mut files = Vec::new();
    for page in 1..=100 {
        let suffix = if remote.provider == Provider::Gitlab {
            "/diff"
        } else {
            ""
        };
        let response = client.get(
            &format!("{root}/commits/{new}{suffix}?per_page=100&page={page}"),
            "application/json",
        )?;
        let value: serde_json::Value =
            serde_json::from_slice(&response.body).map_err(|_| "Invalid diff API response")?;
        let list = if remote.provider == Provider::Github {
            &value["files"]
        } else {
            &value
        };
        let list = list.as_array().ok_or("Diff API omitted files")?;
        if list.len() > 100 {
            return Err("Oversized diff API page".into());
        }
        for v in list {
            let (path, old_path, kind, stat, submodule, blob) = if remote.provider
                == Provider::Github
            {
                let path = file_path(v, "filename")?;
                let kind = match v["status"].as_str() {
                    Some("added" | "copied") => ChangeKind::Added,
                    Some("removed") => ChangeKind::Deleted,
                    Some("renamed") => ChangeKind::Renamed,
                    Some("modified" | "changed" | "unchanged") => ChangeKind::Modified,
                    _ => return Err("Unknown file change status".into()),
                };
                let old_path = (kind == ChangeKind::Renamed)
                    .then(|| file_path(v, "previous_filename"))
                    .transpose()?;
                let stat = v["additions"]
                    .as_u64()
                    .zip(v["deletions"].as_u64())
                    .map(|(a, r)| Stat::Lines {
                        added: a.min(u32::MAX as u64) as u32,
                        removed: r.min(u32::MAX as u64) as u32,
                    });
                let blob = v["sha"]
                    .as_str()
                    .filter(|s| ObjectId::from_hex(s.as_bytes()).is_ok())
                    .map(str::to_owned);
                (path, old_path, kind, stat, false, blob)
            } else {
                let path = file_path(v, "new_path")?;
                let kind = if v["new_file"] == true {
                    ChangeKind::Added
                } else if v["deleted_file"] == true {
                    ChangeKind::Deleted
                } else if v["renamed_file"] == true {
                    ChangeKind::Renamed
                } else {
                    ChangeKind::Modified
                };
                let old_path = (kind == ChangeKind::Renamed)
                    .then(|| file_path(v, "old_path"))
                    .transpose()?;
                let stat = v["diff"]
                    .as_str()
                    .filter(|_| v["too_large"] != true && v["collapsed"] != true)
                    .filter(|d| !d.is_empty())
                    .map(|d| Stat::Lines {
                        added: d
                            .lines()
                            .filter(|l| l.starts_with('+') && !l.starts_with("+++ "))
                            .count() as u32,
                        removed: d
                            .lines()
                            .filter(|l| l.starts_with('-') && !l.starts_with("--- "))
                            .count() as u32,
                    });
                (
                    path,
                    old_path,
                    kind,
                    stat,
                    v["a_mode"] == "160000" || v["b_mode"] == "160000",
                    None,
                )
            };
            files.push(FileChange {
                path,
                old_path,
                kind,
                old: None,
                new: None,
                remote: Some(Box::new(RemoteFile {
                    remote: remote.clone(),
                    old,
                    new,
                    stat,
                    no_cache,
                    submodule,
                    blob,
                })),
            });
        }
        if !response.next.unwrap_or(list.len() == 100) {
            files.sort_by(|a, b| crate::diff::display_order(&a.path, &b.path));
            files.dedup_by(|a, b| a.path == b.path);
            return Ok(TreeDiff { files });
        }
    }
    Err("Remote diff exceeded the API file limit".into())
}

pub fn file(
    repo: &gix::Repository,
    change: &FileChange,
    source: &RemoteFile,
) -> Result<FileDiff, String> {
    if source.submodule {
        return Ok(FileDiff::message(
            "Remote submodule change (checkout to inspect objects)".into(),
        ));
    }
    let mut client = Session::new(repo, &source.remote, source.no_cache)?;
    let root = source
        .remote
        .endpoint
        .strip_suffix("/branches")
        .ok_or("Invalid API endpoint")?;
    let mut contents = |id: ObjectId, path: &str, blob: Option<&str>| -> Result<Vec<u8>, String> {
        let (url, accept) = match source.remote.provider {
            Provider::Github => (
                if let Some(blob) = blob {
                    format!("{root}/git/blobs/{blob}")
                } else {
                    format!(
                        "{root}/contents/{}?ref={id}",
                        encode(path).replace("%2F", "/")
                    )
                },
                "application/vnd.github.raw+json",
            ),
            Provider::Gitlab => (
                format!("{root}/files/{}/raw?ref={id}", encode(path)),
                "application/octet-stream",
            ),
        };
        Ok(client.get(&url, accept)?.body)
    };
    let old = if change.kind != ChangeKind::Added {
        if let Some(id) = source.old {
            contents(id, change.old_path.as_deref().unwrap_or(&change.path), None)?
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };
    let new = if change.kind != ChangeKind::Deleted {
        contents(source.new, &change.path, source.blob.as_deref())?
    } else {
        Vec::new()
    };
    if old
        .iter()
        .take(8000)
        .chain(new.iter().take(8000))
        .any(|&b| b == 0)
    {
        return Ok(FileDiff::message("Binary files differ".into()));
    }
    Ok(crate::diff::sidebyside::build(&old, &new))
}

#[cfg(test)]
pub(super) fn seed_native_fixture(repo: &gix::Repository, remote: &Remote, sha: &str) {
    let session = Session::new(repo, remote, false).unwrap();
    let root = remote.endpoint.strip_suffix("/branches").unwrap();
    let blob = "d".repeat(40);
    let files = serde_json::json!({"files":[{"filename":"remote-example.txt", "status":"added",
        "sha":blob,"additions":2,"deletions":0}]});
    for (url, accept, body) in [
        (
            format!("{root}/commits/{sha}?per_page=100&page=1"),
            "application/json",
            serde_json::to_vec(&files).unwrap(),
        ),
        (
            format!("{root}/git/blobs/{blob}"),
            "application/vnd.github.raw+json",
            b"Remote file contents\nLoaded from the persistent API cache\n".to_vec(),
        ),
    ] {
        let path = session.location(&url, accept).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            serde_json::to_vec(&CachedResponse {
                url,
                next: Some(false),
                body,
            })
            .unwrap(),
        )
        .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn both_providers_load_paginated_files_and_cache_content_offline() {
        for provider in [Provider::Github, Provider::Gitlab] {
            let dir = tempfile::tempdir().unwrap();
            command::checked(dir.path(), &["init", "-q"]).unwrap();
            let repo = crate::git::open(dir.path()).unwrap();
            let old = ObjectId::from_hex("a".repeat(40).as_bytes()).unwrap();
            let new = ObjectId::from_hex("b".repeat(40).as_bytes()).unwrap();
            let (first, second, next) = match provider {
                Provider::Github => (
                    json!({"files":[{"filename":"dir/new name.txt", "previous_filename":"old name.txt", "status":"renamed", "additions":1,"deletions":1}]}),
                    json!({"files":[{"filename":"deleted.txt", "status":"removed","additions":0,"deletions":1}]}),
                    "Link: <http://unused.invalid>; rel=\"next\"\r\n",
                ),
                Provider::Gitlab => (
                    json!([{"new_path":"dir/new name.txt", "old_path":"old name.txt", "renamed_file":true,"diff":"@@ -1 +1 @@\n-old\n+new\n"}]),
                    json!([{"new_path":"deleted.txt", "old_path":"deleted.txt", "deleted_file":true}]),
                    "X-Next-Page: 2\r\n",
                ),
            };
            let (url, server) = super::super::tests::server(vec![
                (200, next.into(), serde_json::to_vec(&first).unwrap()),
                (200, String::new(), serde_json::to_vec(&second).unwrap()),
                (200, String::new(), b"old\nunchanged\n".to_vec()),
                (200, String::new(), b"new\nunchanged\n".to_vec()),
                (200, String::new(), b"deleted\n".to_vec()),
            ]);
            let mut remote = Remote::parse(
                "origin",
                "https://example.invalid/team/repo",
                Some(provider.key()),
            )
            .unwrap();
            remote.endpoint = format!("{url}/repository/branches");
            let changes = tree(&repo, &remote, Some(old), new, false).unwrap();
            assert_eq!(changes.files.len(), 2);
            let renamed = changes
                .files
                .iter()
                .find(|f| f.kind == ChangeKind::Renamed)
                .unwrap();
            assert_eq!(
                crate::diff::file_stats(&repo, renamed).unwrap(),
                Stat::Lines {
                    added: 1,
                    removed: 1
                }
            );
            let diff = crate::diff::file_diff(&repo, renamed).unwrap();
            assert_eq!((diff.added, diff.removed), (1, 1));
            let deleted = changes
                .files
                .iter()
                .find(|f| f.kind == ChangeKind::Deleted)
                .unwrap();
            let deletion = crate::diff::file_diff(&repo, deleted).unwrap();
            assert_eq!((deletion.added, deletion.removed), (0, 1));
            let requests = server.join().unwrap();
            assert_eq!(requests.len(), 5);
            assert!(requests[0].contains(&new.to_string()));
            assert!(requests[1].contains("page=2"));
            assert!(requests[2].contains("old%20name.txt"));
            assert!(requests[3].contains("new%20name.txt"));
            // New sessions, dead server: both file list and complete content are persistent.
            let cached = tree(&repo, &remote, Some(old), new, false).unwrap();
            assert_eq!(cached.files, changes.files);
            assert_eq!(
                crate::diff::file_diff(&repo, renamed).unwrap().rows,
                diff.rows
            );
            // API previews never modify refs or populate local Git objects.
            assert!(crate::git::commit_details(&repo, &new).is_err());
        }
    }

    #[test]
    fn root_additions_binary_files_and_failed_requests() {
        let dir = tempfile::tempdir().unwrap();
        command::checked(dir.path(), &["init", "-q"]).unwrap();
        let repo = crate::git::open(dir.path()).unwrap();
        let new = ObjectId::from_hex("b".repeat(40).as_bytes()).unwrap();
        let (url, server) = super::super::tests::server(vec![
            (200, String::new(), serde_json::to_vec(&json!({"files":[{"filename":"binary", "status":"added", "sha":"c".repeat(40)}]})).unwrap()),
            (200, String::new(), b"binary\0data".to_vec()),
            (503, String::new(), Vec::new()),
        ]);
        let mut remote = Remote::parse(
            "origin",
            "https://example.invalid/team/repo",
            Some("github"),
        )
        .unwrap();
        remote.endpoint = format!("{url}/branches");
        let changes = tree(&repo, &remote, None, new, true).unwrap();
        let diff = crate::diff::file_diff(&repo, &changes.files[0]).unwrap();
        assert!(matches!(&diff.rows[0], crate::diff::Row::Message(s) if s.contains("Binary")));
        assert!(
            tree(&repo, &remote, None, new, true)
                .unwrap_err()
                .contains("503")
        );
        let requests = server.join().unwrap();
        assert!(requests[1].contains("/git/blobs/"));
        assert!(requests[1].contains("application/vnd.github.raw+json"));
    }
}
