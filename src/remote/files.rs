//! Immutable, on-demand API diffs. No branch fetch or Git ref writes.
use super::*;
use crate::diff::Side;
use crate::diff::{ChangeKind, FileChange, FileDiff, Stat, TreeDiff};
use gix::ObjectId;
use gix::objs::tree::EntryKind;
use gix::prelude::FindExt;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFile {
    pub remote: Arc<Remote>,
    pub old_remote: Option<Arc<Remote>>,
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
                worktree: false,
                path,
                old_path,
                kind,
                old: None,
                new: None,
                remote: Some(Box::new(RemoteFile {
                    remote: remote.clone(),
                    old_remote: None,
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

/// Compare complete endpoint trees, including divergent histories. Provider compare
/// endpoints can use a merge base, which is not the two marked snapshots.
pub fn compare(
    repo: &gix::Repository,
    remote: &Remote,
    old_remote: Option<&Remote>,
    old: Option<ObjectId>,
    new: ObjectId,
    no_cache: bool,
) -> Result<TreeDiff, String> {
    let mut client = Session::new(repo, remote, no_cache)?;
    let mut old_client = Session::new(repo, old_remote.unwrap_or(remote), no_cache)?;
    let mut before = old
        .map(|id| snapshot(&mut old_client, id))
        .transpose()?
        .unwrap_or_default();
    let after = snapshot(&mut client, new)?;
    let mut files = Vec::new();
    for (path, side) in after {
        let old_side = before.remove(&path);
        if old_side != Some(side) {
            files.push(FileChange {
                worktree: false,
                kind: if old_side.is_some() {
                    ChangeKind::Modified
                } else {
                    ChangeKind::Added
                },
                path,
                old_path: None,
                old: old_side,
                new: Some(side),
                remote: None,
            });
        }
    }
    for (path, side) in before {
        files.push(FileChange {
            worktree: false,
            path,
            old_path: None,
            kind: ChangeKind::Deleted,
            old: Some(side),
            new: None,
            remote: None,
        });
    }
    crate::diff::pair_renames(&mut files);
    let remote = Arc::new(remote.clone());
    for f in &mut files {
        f.remote = Some(Box::new(RemoteFile {
            remote: remote.clone(),
            old_remote: old_remote.map(|r| Arc::new(r.clone())),
            old,
            new,
            stat: None,
            no_cache,
            submodule: f
                .old
                .iter()
                .chain(f.new.iter())
                .any(|s| s.kind == EntryKind::Commit),
            blob: f.new.map(|s| s.id.to_string()),
        }));
    }
    files.sort_by(|a, b| crate::diff::display_order(&a.path, &b.path));
    Ok(TreeDiff { files })
}

fn snapshot(client: &mut Session<'_>, id: ObjectId) -> Result<BTreeMap<String, Side>, String> {
    // A local side may contain unpushed commits, unavailable to the provider.
    if crate::git::commit_details(client.repo, &id).is_ok() {
        return crate::diff::tree_diff(client.repo, None, &id)
            .map(|t| {
                t.files
                    .into_iter()
                    .filter_map(|f| f.new.map(|s| (f.path, s)))
                    .collect()
            })
            .map_err(|e| e.to_string());
    }
    let root = client
        .remote
        .endpoint
        .strip_suffix("/branches")
        .ok_or("Invalid API endpoint")?
        .to_owned();
    let mut entries = BTreeMap::new();
    let start = std::time::Instant::now();
    for page in 1..=1000 {
        if start.elapsed() > Duration::from_secs(60) {
            return Err("Remote tree listing exceeded 60 seconds".into());
        }
        let url = match client.remote.provider {
            Provider::Github => format!("{root}/git/trees/{id}?recursive=1"),
            Provider::Gitlab => {
                format!("{root}/tree?ref={id}&recursive=true&per_page=100&page={page}")
            }
        };
        let response = client.get(&url, "application/json")?;
        let value: serde_json::Value =
            serde_json::from_slice(&response.body).map_err(|_| "Invalid tree API response")?;
        let list = if client.remote.provider == Provider::Github {
            if value["truncated"] != false {
                return Err("Remote tree is truncated; cannot compare complete snapshots".into());
            }
            &value["tree"]
        } else {
            &value
        };
        let list = list.as_array().ok_or("Tree API omitted entries")?;
        for v in list {
            if v["type"] == "tree" {
                continue;
            }
            let path = file_path(v, "path")?;
            let sha = v["sha"]
                .as_str()
                .or_else(|| v["id"].as_str())
                .ok_or("Tree API omitted object ID")?;
            let id = ObjectId::from_hex(sha.as_bytes()).map_err(|_| "Invalid tree object ID")?;
            let kind = match v["mode"].as_str() {
                Some("100644") => EntryKind::Blob,
                Some("100755") => EntryKind::BlobExecutable,
                Some("120000") => EntryKind::Link,
                Some("160000") => EntryKind::Commit,
                _ => return Err("Unknown remote tree mode".into()),
            };
            if entries.insert(path, Side { id, kind }).is_some() {
                return Err("Duplicate path in remote tree".into());
            }
        }
        if entries.len() > 100_000 {
            return Err("Remote tree exceeded 100,000 files".into());
        }
        if client.remote.provider == Provider::Github || !response.next.unwrap_or(list.len() == 100)
        {
            return Ok(entries);
        }
    }
    Err("Remote tree exceeded 1,000 pages".into())
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
    let mut old_client = source
        .old_remote
        .as_ref()
        .map(|r| Session::new(repo, r, source.no_cache))
        .transpose()?;
    let mut contents =
        |id: ObjectId, path: &str, blob: Option<&str>, old: bool| -> Result<Vec<u8>, String> {
            if let Some(blob) = blob.and_then(|b| ObjectId::from_hex(b.as_bytes()).ok()) {
                let mut buf = Vec::new();
                if let Ok(data) = repo.objects.find_blob(&blob, &mut buf) {
                    return Ok(data.data.to_vec());
                }
            }
            let client = if old {
                old_client.as_mut().unwrap_or(&mut client)
            } else {
                &mut client
            };
            let root = client
                .remote
                .endpoint
                .strip_suffix("/branches")
                .ok_or("Invalid API endpoint")?;
            let (url, accept) = match client.remote.provider {
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
            contents(
                id,
                change.old_path.as_deref().unwrap_or(&change.path),
                change.old.map(|s| s.id.to_string()).as_deref(),
                true,
            )?
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };
    let new = if change.kind != ChangeKind::Deleted {
        contents(source.new, &change.path, source.blob.as_deref(), false)?
    } else {
        Vec::new()
    };
    if old.len() > 8 * 1024 * 1024 || new.len() > 8 * 1024 * 1024 {
        return Ok(FileDiff::message("File too large to diff".into()));
    }
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
    for (sha, blob, content) in [
        (
            "a".repeat(40),
            "d".repeat(40),
            "Remote file contents\nLoaded from the persistent API cache\n",
        ),
        ("b".repeat(40), "c".repeat(40), "Previous remote contents\n"),
        (
            "f".repeat(40),
            "e".repeat(40),
            "Remote-only branch contents\n",
        ),
    ] {
        for (url,accept,body) in [
            (format!("{root}/git/trees/{sha}?recursive=1"),"application/json",serde_json::to_vec(&serde_json::json!({"truncated":false,"tree":[{"path":"remote-example.txt","sha":blob,"mode":"100644","type":"blob"}]})).unwrap()),
            (format!("{root}/git/blobs/{blob}"),"application/vnd.github.raw+json",content.as_bytes().to_vec()),
        ] {
            let path = session.location(&url,accept).unwrap();
            std::fs::write(path,serde_json::to_vec(&CachedResponse { url,next:Some(false),body }).unwrap()).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    #[ignore = "explicit remote-pair performance measurement"]
    fn benchmark_remote_pairs() {
        let dir = tempfile::tempdir().unwrap();
        command::checked(dir.path(), &["init", "-q"]).unwrap();
        let repo = crate::git::open(dir.path()).unwrap();
        let old = ObjectId::from_hex("a".repeat(40).as_bytes()).unwrap();
        let new = ObjectId::from_hex("b".repeat(40).as_bytes()).unwrap();
        let entries = |offset: usize| {
            (0..5000).map(|i| json!({"path":format!("dir/file-{i:04}.txt"),"sha":format!("{:040x}",i+offset),"mode":"100644","type":"blob"})).collect::<Vec<_>>()
        };
        let (url, server) = super::super::tests::server(vec![
            (
                200,
                String::new(),
                serde_json::to_vec(&json!({"truncated":false,"tree":entries(1)})).unwrap(),
            ),
            (
                200,
                String::new(),
                serde_json::to_vec(&json!({"truncated":false,"tree":entries(10001)})).unwrap(),
            ),
            (200, String::new(), b"old\nunchanged\n".to_vec()),
            (200, String::new(), b"new\nunchanged\n".to_vec()),
        ]);
        let mut remote = Remote::parse(
            "origin",
            "https://example.invalid/team/repo",
            Some("github"),
        )
        .unwrap();
        remote.endpoint = format!("{url}/branches");
        let start = std::time::Instant::now();
        let changes = compare(&repo, &remote, None, Some(old), new, false).unwrap();
        let network_tree = start.elapsed().as_secs_f64() * 1000.0;
        let start = std::time::Instant::now();
        crate::diff::file_diff(&repo, &changes.files[0]).unwrap();
        let network_file = start.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(server.join().unwrap().len(), 4);
        let mut samples = Vec::new();
        for n in 0..16 {
            let start = std::time::Instant::now();
            let changes = compare(&repo, &remote, None, Some(old), new, false).unwrap();
            let tree = start.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(changes.files.len(), 5000);
            let start = std::time::Instant::now();
            let file = crate::diff::file_diff(&repo, &changes.files[0]).unwrap();
            let file_ms = start.elapsed().as_secs_f64() * 1000.0;
            assert_eq!((file.added, file.removed), (1, 1));
            if n > 0 {
                samples.push(json!({"sample":n-1,"cachedTreeMs":tree,"cachedFileMs":file_ms}));
            }
        }
        println!(
            "5000 modified files; local HTTP fixture first tree {network_tree:.3} ms, first file {network_file:.3} ms; one offline warm-up, 15 samples: {}",
            serde_json::to_string(&samples).unwrap()
        );
    }

    #[test]
    fn marked_snapshots_compare_divergent_endpoints_and_reuse_cache() {
        for provider in [Provider::Github, Provider::Gitlab] {
            let dir = tempfile::tempdir().unwrap();
            command::checked(dir.path(), &["init", "-q"]).unwrap();
            let repo = crate::git::open(dir.path()).unwrap();
            let old = ObjectId::from_hex("a".repeat(40).as_bytes()).unwrap();
            let new = ObjectId::from_hex("b".repeat(40).as_bytes()).unwrap();
            let entry = |path: &str, sha: &str| match provider {
                Provider::Github => json!({"path":path,"sha":sha,"mode":"100644","type":"blob"}),
                Provider::Gitlab => json!({"path":path,"id":sha,"mode":"100644","type":"blob"}),
            };
            let tree = |entries: Vec<serde_json::Value>| match provider {
                Provider::Github => json!({"tree":entries,"truncated":false}),
                Provider::Gitlab => json!(entries),
            };
            let before = tree(vec![
                entry("old-only.txt", &"c".repeat(40)),
                entry("changed.txt", &"d".repeat(40)),
                entry("before.txt", &"e".repeat(40)),
            ]);
            let after = tree(vec![
                entry("new-only.txt", &"f".repeat(40)),
                entry("changed.txt", &"1".repeat(40)),
                entry("after.txt", &"e".repeat(40)),
            ]);
            let (url, server) = super::super::tests::server(vec![
                (200, String::new(), serde_json::to_vec(&before).unwrap()),
                (200, String::new(), serde_json::to_vec(&after).unwrap()),
                (200, String::new(), b"old\nunchanged\n".to_vec()),
                (200, String::new(), b"new\nunchanged\n".to_vec()),
            ]);
            let mut remote = Remote::parse(
                "origin",
                "https://example.invalid/team/repo",
                Some(provider.key()),
            )
            .unwrap();
            remote.endpoint = format!("{url}/repository/branches");
            let changes = compare(&repo, &remote, None, Some(old), new, false).unwrap();
            assert_eq!(changes.files.len(), 4);
            assert!(
                changes
                    .files
                    .iter()
                    .any(|f| f.path == "old-only.txt" && f.kind == ChangeKind::Deleted)
            );
            assert!(
                changes
                    .files
                    .iter()
                    .any(|f| f.path == "new-only.txt" && f.kind == ChangeKind::Added)
            );
            assert!(changes.files.iter().any(|f| f.path == "after.txt"
                && f.old_path.as_deref() == Some("before.txt")
                && f.kind == ChangeKind::Renamed));
            let changed = changes
                .files
                .iter()
                .find(|f| f.path == "changed.txt")
                .unwrap();
            let diff = crate::diff::file_diff(&repo, changed).unwrap();
            assert_eq!((diff.added, diff.removed), (1, 1));
            let requests = server.join().unwrap();
            assert_eq!(requests.len(), 4);
            assert!(requests[0].contains(&old.to_string()));
            assert!(requests[1].contains(&new.to_string()));
            assert!(!requests.iter().any(|r| r.contains("/compare")));
            assert_eq!(
                compare(&repo, &remote, None, Some(old), new, false)
                    .unwrap()
                    .files,
                changes.files
            );
            assert_eq!(
                crate::diff::file_diff(&repo, changed).unwrap().rows,
                diff.rows
            );
            assert!(crate::git::commit_details(&repo, &new).is_err());
        }
    }

    #[test]
    fn mixed_local_and_remote_pairs_work_in_both_directions() {
        let dir = tempfile::tempdir().unwrap();
        command::checked(dir.path(), &["init", "-q"]).unwrap();
        std::fs::write(dir.path().join("file.txt"), "local only\n").unwrap();
        command::checked(dir.path(), &["add", "file.txt"]).unwrap();
        command::checked(
            dir.path(),
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "unpushed local commit",
            ],
        )
        .unwrap();
        let repo = crate::git::open(dir.path()).unwrap();
        let local = crate::git::snapshot_refs(&repo).unwrap().head_id.unwrap();
        let ghost = ObjectId::from_hex("a".repeat(40).as_bytes()).unwrap();
        let blob = "b".repeat(40);
        let (url,server) = super::super::tests::server(vec![
            (200,String::new(),serde_json::to_vec(&json!({"truncated":false,"tree":[{"path":"file.txt","sha":blob,"mode":"100644","type":"blob"}]})).unwrap()),
            (200,String::new(),b"remote only\n".to_vec()),
        ]);
        let mut remote = Remote::parse(
            "origin",
            "https://example.invalid/team/repo",
            Some("github"),
        )
        .unwrap();
        remote.endpoint = format!("{url}/branches");
        for (old, new) in [(local, ghost), (ghost, local)] {
            let changes = compare(&repo, &remote, None, Some(old), new, false).unwrap();
            assert_eq!(changes.files.len(), 1);
            let diff = crate::diff::file_diff(&repo, &changes.files[0]).unwrap();
            assert_eq!((diff.added, diff.removed), (1, 1));
        }
        assert_eq!(server.join().unwrap().len(), 2);
        assert_eq!(
            crate::git::snapshot_refs(&repo).unwrap().head_id,
            Some(local)
        );
        assert!(crate::git::commit_details(&repo, &ghost).is_err());
    }

    #[test]
    fn pairs_use_each_remote_endpoint_for_its_own_tree_and_content() {
        let dir = tempfile::tempdir().unwrap();
        command::checked(dir.path(), &["init", "-q"]).unwrap();
        let repo = crate::git::open(dir.path()).unwrap();
        let tree = |sha: &str| {
            serde_json::to_vec(&json!({"truncated":false,"tree":[{"path":"file.txt","sha":sha,"mode":"100644","type":"blob"}]})).unwrap()
        };
        let (old_url, old_server) = super::super::tests::server(vec![
            (200, String::new(), tree(&"c".repeat(40))),
            (200, String::new(), b"old\n".to_vec()),
        ]);
        let (new_url, new_server) = super::super::tests::server(vec![
            (200, String::new(), tree(&"d".repeat(40))),
            (200, String::new(), b"new\n".to_vec()),
        ]);
        let mut old_remote = Remote::parse(
            "upstream",
            "https://example.invalid/old/repo",
            Some("github"),
        )
        .unwrap();
        old_remote.endpoint = format!("{old_url}/branches");
        let mut new_remote = old_remote.clone();
        new_remote.name = "origin".into();
        new_remote.endpoint = format!("{new_url}/branches");
        let old = ObjectId::from_hex("a".repeat(40).as_bytes()).unwrap();
        let new = ObjectId::from_hex("b".repeat(40).as_bytes()).unwrap();
        let changes = compare(&repo, &new_remote, Some(&old_remote), Some(old), new, true).unwrap();
        let diff = crate::diff::file_diff(&repo, &changes.files[0]).unwrap();
        assert_eq!((diff.added, diff.removed), (1, 1));
        assert_eq!(old_server.join().unwrap().len(), 2);
        assert_eq!(new_server.join().unwrap().len(), 2);
    }

    #[test]
    fn truncated_tree_is_an_error_instead_of_a_partial_diff() {
        let dir = tempfile::tempdir().unwrap();
        command::checked(dir.path(), &["init", "-q"]).unwrap();
        let repo = crate::git::open(dir.path()).unwrap();
        let (url, server) = super::super::tests::server(vec![(
            200,
            String::new(),
            br#"{"tree":[],"truncated":true}"#.to_vec(),
        )]);
        let mut remote = Remote::parse(
            "origin",
            "https://example.invalid/team/repo",
            Some("github"),
        )
        .unwrap();
        remote.endpoint = format!("{url}/branches");
        let id = ObjectId::from_hex("a".repeat(40).as_bytes()).unwrap();
        assert!(
            compare(&repo, &remote, None, None, id, true)
                .unwrap_err()
                .contains("truncated")
        );
        assert_eq!(server.join().unwrap().len(), 1);
    }

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
