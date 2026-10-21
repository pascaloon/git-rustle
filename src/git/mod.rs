//! Repository access through gitoxide: opening, reference snapshots, the
//! streaming history walk, and on-demand commit details.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use gix::ObjectId;
use gix::bstr::ByteSlice;
use gix::objs::commit::ref_iter::Token;
use gix::prelude::FindExt;

use crate::graph::layout::Layout;
use crate::model::History;

pub mod checkout;
pub mod command;
pub mod operations;
pub mod refs;
pub mod worktree;

pub type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
pub type Result<T> = std::result::Result<T, Error>;

/// Open the repository containing `path` (searching upwards) with an isolated
/// configuration: no global/system config or environment lookups, which keeps
/// startup time minimal.
pub fn open(path: &Path) -> Result<gix::Repository> {
    // Discovery preserves relative paths, which would leave the header and
    // folder actions pointing at "." when launched from the repository root.
    let path = std::path::absolute(path)?;
    let opts = gix::open::Options::isolated();
    let trust_map = gix::sec::trust::Mapping {
        full: opts.clone(),
        reduced: opts,
    };
    let repo = gix::ThreadSafeRepository::discover_opts(path, Default::default(), trust_map)?;
    let mut repo = repo.to_thread_local();
    repo.object_cache_size_if_unset(32 * 1024 * 1024);
    Ok(repo)
}

/// A human-friendly repository name (the work tree directory name, or the git dir).
pub fn display_name(repo: &gix::Repository) -> String {
    let dir = repo.workdir().unwrap_or_else(|| repo.git_dir());
    dir.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string())
}

/// The git directory, used as the cache key.
pub fn git_dir(repo: &gix::Repository) -> PathBuf {
    std::fs::canonicalize(repo.git_dir()).unwrap_or_else(|_| repo.git_dir().to_path_buf())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    Head,
    Branch,
    Remote,
    Tag,
    Other,
}

#[derive(Debug, Clone)]
pub struct RefEntry {
    /// Short display name (`main`, `origin/main`, `v1.0`).
    pub name: String,
    pub id: ObjectId,
    pub kind: RefKind,
}

#[derive(Debug, Clone, Default)]
pub struct RefSnapshot {
    pub head_id: Option<ObjectId>,
    /// Short name of the checked-out branch, if HEAD is not detached.
    pub head_branch: Option<String>,
    pub refs: Vec<RefEntry>,
    /// Unique commit ids to start the walk from (HEAD plus all refs).
    pub tips: Vec<ObjectId>,
    /// Stable hash over all (name, id) pairs and HEAD; identifies a graph.
    pub fingerprint: u64,
    /// Exact upstream branch identities, keyed by remote. Built once on the
    /// worker so branch rendering/search never reads configuration.
    pub tracking: BTreeMap<String, HashSet<String>>,
}

impl RefSnapshot {
    /// Labels per commit, in display order (HEAD, branches, remotes, tags).
    pub fn labels_by_commit(&self) -> HashMap<ObjectId, Vec<&RefEntry>> {
        let mut map: HashMap<ObjectId, Vec<&RefEntry>> = HashMap::new();
        for r in &self.refs {
            map.entry(r.id).or_default().push(r);
        }
        for v in map.values_mut() {
            v.sort_by_key(|r| match r.kind {
                RefKind::Head => 0,
                RefKind::Branch => 1,
                RefKind::Remote => 2,
                RefKind::Tag => 3,
                RefKind::Other => 4,
            });
        }
        map
    }
}

pub fn snapshot_refs(repo: &gix::Repository) -> Result<RefSnapshot> {
    if refs::is_reftable(repo) {
        return snapshot_reftable(repo);
    }
    let mut snap = RefSnapshot::default();
    let mut seen_tips: HashSet<ObjectId> = HashSet::new();

    // HEAD labels are presentation metadata, not graph ordering seeds.
    let head = repo.head()?;
    snap.head_branch = head
        .referent_name()
        .map(|n| n.shorten().to_str_lossy().into_owned());
    if let Some(id) = head.id() {
        let id = id.detach();
        snap.head_id = Some(id);
    }

    let platform = repo.references()?;
    // `peeled()` follows symbolic refs (e.g. `origin/HEAD` becomes `origin/master`),
    // so the same (name, id) can show up twice; keep the first.
    let mut seen_refs: HashSet<(String, ObjectId)> = HashSet::new();
    let iter = platform.all()?.peeled()?;
    for r in iter {
        let Ok(r) = r else { continue };
        let Some(id) = r.target().try_id().map(|o| o.to_owned()) else {
            continue;
        };
        let full = r.name().as_bstr();
        let kind = if full.starts_with(b"refs/heads/") {
            RefKind::Branch
        } else if full.starts_with(b"refs/remotes/") {
            RefKind::Remote
        } else if full.starts_with(b"refs/tags/") {
            RefKind::Tag
        } else if full == b"refs/stash".as_bstr()
            || full.starts_with(b"refs/notes/")
            || full.starts_with(b"refs/rustle/")
        {
            continue;
        } else {
            RefKind::Other
        };
        let name = match kind {
            RefKind::Other => full.to_str_lossy().into_owned(),
            _ => r.name().shorten().to_str_lossy().into_owned(),
        };
        if !seen_refs.insert((name.clone(), id)) {
            continue;
        }
        if seen_tips.insert(id) {
            snap.tips.push(id);
        }
        snap.refs.push(RefEntry { name, id, kind });
    }
    if let (Some(id), Some(branch)) = (snap.head_id, &snap.head_branch) {
        // Mark the checked-out branch so it can be highlighted.
        if let Some(r) = snap
            .refs
            .iter_mut()
            .find(|r| r.kind == RefKind::Branch && &r.name == branch && r.id == id)
        {
            r.kind = RefKind::Head;
        }
    } else if let Some(id) = snap.head_id {
        snap.refs.push(RefEntry {
            name: "HEAD".into(),
            id,
            kind: RefKind::Head,
        });
    }

    snapshot_tracking(repo, &mut snap);
    snap.fingerprint = fingerprint(&snap);
    Ok(snap)
}

fn snapshot_tracking(repo: &gix::Repository, snap: &mut RefSnapshot) {
    let config = repo.config_snapshot();
    let config = config.plumbing();
    for reference in &snap.refs {
        if reference.kind != RefKind::Branch
            && !(reference.kind == RefKind::Head
                && snap.head_branch.as_ref() == Some(&reference.name))
        {
            continue;
        }
        let section = Some(reference.name.as_bytes().as_bstr());
        let Some(remote) = config.string_by("branch", section, "remote") else {
            continue;
        };
        let Some(merge) = config.string_by("branch", section, "merge") else {
            continue;
        };
        let Some(name) = merge.strip_prefix(b"refs/heads/") else {
            continue;
        };
        if remote == b".".as_bstr() {
            continue;
        }
        snap.tracking
            .entry(remote.to_str_lossy().into_owned())
            .or_default()
            .insert(name.to_str_lossy().into_owned());
    }
}

fn snapshot_reftable(repo: &gix::Repository) -> Result<RefSnapshot> {
    let head = refs::head(repo)?;
    let mut snap = RefSnapshot {
        head_id: head.id,
        head_branch: head.branch,
        ..Default::default()
    };
    let mut seen = HashSet::new();
    for reference in refs::raw_refs(repo)? {
        let Some(id) = reference.id else { continue };
        let (name, kind) = if let Some(name) = reference.name.strip_prefix("refs/heads/") {
            (
                name.to_owned(),
                if snap.head_branch.as_deref() == Some(name) && snap.head_id == Some(id) {
                    RefKind::Head
                } else {
                    RefKind::Branch
                },
            )
        } else if let Some(name) = reference.name.strip_prefix("refs/remotes/") {
            (name.to_owned(), RefKind::Remote)
        } else if let Some(name) = reference.name.strip_prefix("refs/tags/") {
            (name.to_owned(), RefKind::Tag)
        } else if reference.name == "refs/stash"
            || reference.name.starts_with("refs/notes/")
            || reference.name.starts_with("refs/rustle/")
        {
            continue;
        } else {
            (reference.name, RefKind::Other)
        };
        // Peel tag objects through gitoxide; nested tags must reach their final
        // object just as the files iterator's peeled() path does.
        let id = if kind == RefKind::Tag {
            match repo.find_object(id).and_then(|o| o.peel_tags_to_end()) {
                Ok(object) => object.id,
                Err(_) => continue,
            }
        } else {
            id
        };
        if seen.insert(id) {
            snap.tips.push(id);
        }
        snap.refs.push(RefEntry { name, id, kind });
    }
    if snap.head_branch.is_none()
        && let Some(id) = snap.head_id
    {
        snap.refs.push(RefEntry {
            name: "HEAD".into(),
            id,
            kind: RefKind::Head,
        });
    }
    snapshot_tracking(repo, &mut snap);
    snap.fingerprint = fingerprint(&snap);
    Ok(snap)
}

fn fingerprint(snap: &RefSnapshot) -> u64 {
    // DefaultHasher::new() uses fixed keys, so this is stable across runs of the same build.
    let mut h = std::collections::hash_map::DefaultHasher::new();
    1u32.hash(&mut h); // walk semantics version
    snap.head_id.map(|id| id.as_bytes().to_vec()).hash(&mut h);
    snap.head_branch.hash(&mut h);
    let mut pairs: Vec<(&str, &[u8])> = snap
        .refs
        .iter()
        .map(|r| (r.name.as_str(), r.id.as_bytes()))
        .collect();
    pairs.sort();
    pairs.hash(&mut h);
    // Hash upstreams in a stable order; configuration-only changes must update
    // the branch availability metadata too.
    for (remote, names) in &snap.tracking {
        remote.hash(&mut h);
        let mut names: Vec<_> = names.iter().collect();
        names.sort();
        names.hash(&mut h);
    }
    h.finish()
}

/// Commit data parsed once at enqueue time.
struct Parsed {
    parents: Vec<ObjectId>,
    author: Box<[u8]>,
    author_time: i64,
    author_offset: i32,
    commit_time: i64,
    subject: Box<[u8]>,
}

struct QueueEntry {
    time: i64,
    id: ObjectId,
}

impl PartialEq for QueueEntry {
    fn eq(&self, other: &Self) -> bool {
        self.time == other.time && self.id == other.id
    }
}
impl Eq for QueueEntry {}
impl PartialOrd for QueueEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for QueueEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Object identity breaks timestamp ties independently of HEAD/ref order.
        self.time
            .cmp(&other.time)
            .then_with(|| other.id.cmp(&self.id))
    }
}

fn parse_commit(repo: &gix::Repository, id: &ObjectId, buf: &mut Vec<u8>) -> Option<Parsed> {
    let iter = repo.objects.find_commit_iter(id, buf).ok()?;
    let mut parsed = Parsed {
        parents: Vec::new(),
        author: Box::default(),
        author_time: 0,
        author_offset: 0,
        commit_time: 0,
        subject: Box::default(),
    };
    for token in iter {
        match token.ok()? {
            Token::Tree { .. } => {}
            Token::Parent { id } => parsed.parents.push(id),
            Token::Author { signature } => {
                parsed.author = signature.name.as_bytes().into();
                match signature.time() {
                    Ok(t) => {
                        parsed.author_time = t.seconds;
                        parsed.author_offset = t.offset;
                    }
                    Err(_) => parsed.author_time = signature.seconds(),
                }
            }
            Token::Committer { signature } => {
                parsed.commit_time = signature.seconds();
            }
            Token::Encoding(_) | Token::ExtraHeader(_) => {}
            Token::Message(msg) => {
                parsed.subject = subject_of(msg).into();
                break;
            }
        }
    }
    Some(parsed)
}

fn subject_of(msg: &[u8]) -> &[u8] {
    let end = msg.iter().position(|&b| b == b'\n').unwrap_or(msg.len());
    msg[..end].trim()
}

/// Walk commits reachable from reference `tips`, newest committer-time first, laying
/// out the graph as rows and handing them to `emit` in growing batches.
/// Detached HEAD is a fallback after reference history, so checking out an
/// already-reachable ancestor cannot change ordering or lane assignment.
/// `emit` returns `false` to cancel.
pub fn walk(
    repo: &gix::Repository,
    tips: &[ObjectId],
    emit: &mut dyn FnMut(History) -> bool,
) -> Result<()> {
    let mut buf = Vec::with_capacity(4096);
    let mut seen: HashSet<ObjectId> = HashSet::with_capacity(4096);
    let mut pending: HashMap<ObjectId, Parsed> = HashMap::with_capacity(1024);
    let mut queue: BinaryHeap<QueueEntry> = BinaryHeap::with_capacity(1024);
    let mut layout = Layout::new();
    let mut check_fallback_head = true;

    let enqueue = |id: ObjectId,
                   seen: &mut HashSet<ObjectId>,
                   pending: &mut HashMap<ObjectId, Parsed>,
                   queue: &mut BinaryHeap<QueueEntry>,
                   buf: &mut Vec<u8>| {
        if !seen.insert(id) {
            return;
        }
        if let Some(parsed) = parse_commit(repo, &id, buf) {
            queue.push(QueueEntry {
                time: parsed.commit_time,
                id,
            });
            pending.insert(id, parsed);
        }
    };

    for tip in tips {
        enqueue(*tip, &mut seen, &mut pending, &mut queue, &mut buf);
    }

    let mut chunk = History::default();
    let mut batch_size = 64usize;
    let mut graph_parents: Vec<ObjectId> = Vec::new();
    let mut ready = VecDeque::new();

    loop {
        while queue
            .peek()
            .is_some_and(|entry| !pending.contains_key(&entry.id))
        {
            queue.pop();
        }
        if queue.is_empty() && ready.is_empty() && check_fallback_head {
            check_fallback_head = false;
            if let Some(head) = refs::head(repo)?.id {
                enqueue(head, &mut seen, &mut pending, &mut queue, &mut buf);
            }
        }
        if ready.is_empty() {
            let Some(first) = queue.pop() else { break };
            let time = first.time;
            let mut peers = Vec::new();
            while queue.peek().is_some_and(|entry| entry.time == time) {
                let entry = queue.pop().unwrap();
                if pending.contains_key(&entry.id) {
                    peers.push(entry.id);
                }
            }
            graph_parents.clear();
            graph_parents.extend_from_slice(&pending[&first.id].parents);
            for parent in graph_parents.iter().copied() {
                enqueue(parent, &mut seen, &mut pending, &mut queue, &mut buf);
            }
            let equal_parent = graph_parents
                .iter()
                .any(|parent| pending.get(parent).is_some_and(|p| p.commit_time == time));
            if peers.is_empty() && !equal_parent {
                ready.push_back(first);
            } else {
                let mut frontier = HashSet::from([first.id]);
                let mut discover = vec![first.id];
                for id in peers {
                    if frontier.insert(id) {
                        discover.push(id);
                    }
                }
                // Discover only this timestamp frontier before emitting it. This
                // prevents a referenced equal-time ancestor from appearing before
                // its descendant, without delaying startup for unrelated history.
                while let Some(id) = discover.pop() {
                    let parents = pending[&id].parents.clone();
                    for parent in parents {
                        enqueue(parent, &mut seen, &mut pending, &mut queue, &mut buf);
                        if pending.get(&parent).is_some_and(|p| p.commit_time == time)
                            && frontier.insert(parent)
                        {
                            discover.push(parent);
                        }
                    }
                }
                let mut children = HashMap::<ObjectId, usize>::new();
                for id in &frontier {
                    for parent in &pending[id].parents {
                        if frontier.contains(parent) {
                            *children.entry(*parent).or_default() += 1;
                        }
                    }
                }
                let mut eligible = BinaryHeap::new();
                for id in &frontier {
                    if children.get(id).copied().unwrap_or(0) == 0 {
                        eligible.push(QueueEntry { time, id: *id });
                    }
                }
                while let Some(entry) = eligible.pop() {
                    for parent in &pending[&entry.id].parents {
                        if let Some(count) = children.get_mut(parent) {
                            *count -= 1;
                            if *count == 0 {
                                eligible.push(QueueEntry { time, id: *parent });
                            }
                        }
                    }
                    ready.push_back(entry);
                }
            }
        }
        let Some(entry) = ready.pop_front() else {
            break;
        };
        let id = entry.id;
        let Some(parsed) = pending.remove(&id) else {
            continue;
        };
        for p in &parsed.parents {
            enqueue(*p, &mut seen, &mut pending, &mut queue, &mut buf);
        }
        // Parents that are pending will appear later and get a lane. Parents already
        // emitted (clock skew) or missing (shallow clone) are not drawn.
        graph_parents.clear();
        graph_parents.extend(
            parsed
                .parents
                .iter()
                .filter(|p| pending.contains_key(*p))
                .copied(),
        );
        let row = layout.push(&id, &graph_parents);

        let parents = chunk.push_parents(&parsed.parents);
        let author = chunk.push_text(&parsed.author);
        let subject = chunk.push_text(&parsed.subject);
        let cells = chunk.push_cells(&row.cells);
        chunk.commits.push(crate::model::CommitRec {
            id,
            parents,
            author_time: parsed.author_time,
            author_offset: parsed.author_offset,
            commit_time: parsed.commit_time,
            author,
            subject,
            row: cells,
            node_lane: row.node_lane,
        });

        if chunk.commits.len() >= batch_size {
            let out = std::mem::take(&mut chunk);
            if !emit(out) {
                return Ok(());
            }
            batch_size = (batch_size * 4).min(16384);
        }
    }
    chunk.complete = true;
    emit(chunk);
    Ok(())
}

/// Everything the details panel shows for one commit.
#[derive(Debug, Clone)]
pub struct CommitDetails {
    pub id: ObjectId,
    pub parents: Vec<ObjectId>,
    pub author_name: String,
    pub author_email: String,
    pub author_time: i64,
    pub author_offset: i32,
    pub committer_name: String,
    pub committer_email: String,
    pub commit_time: i64,
    pub commit_offset: i32,
    pub message: String,
}

pub fn commit_details(repo: &gix::Repository, id: &ObjectId) -> Result<CommitDetails> {
    let mut buf = Vec::new();
    let iter = repo.objects.find_commit_iter(id, &mut buf)?;
    let mut d = CommitDetails {
        id: *id,
        parents: Vec::new(),
        author_name: String::new(),
        author_email: String::new(),
        author_time: 0,
        author_offset: 0,
        committer_name: String::new(),
        committer_email: String::new(),
        commit_time: 0,
        commit_offset: 0,
        message: String::new(),
    };
    for token in iter {
        match token? {
            Token::Tree { .. } => {}
            Token::Parent { id } => d.parents.push(id),
            Token::Author { signature } => {
                d.author_name = signature.name.to_str_lossy().into_owned();
                d.author_email = signature.email.to_str_lossy().into_owned();
                if let Ok(t) = signature.time() {
                    d.author_time = t.seconds;
                    d.author_offset = t.offset;
                }
            }
            Token::Committer { signature } => {
                d.committer_name = signature.name.to_str_lossy().into_owned();
                d.committer_email = signature.email.to_str_lossy().into_owned();
                if let Ok(t) = signature.time() {
                    d.commit_time = t.seconds;
                    d.commit_offset = t.offset;
                }
            }
            Token::Encoding(_) | Token::ExtraHeader(_) => {}
            Token::Message(msg) => {
                d.message = msg.to_str_lossy().replace("\r\n", "\n");
                break;
            }
        }
    }
    Ok(d)
}

#[cfg(test)]
mod graph_order_tests {
    use super::*;

    #[test]
    #[ignore = "one warm-up and 15 pathological same-second 10k-commit walk samples"]
    fn benchmark_equal_timestamp_frontier() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-b", "main"]);
        let mut data = String::new();
        for n in 0..10_000 {
            let message = format!("commit {n}");
            data.push_str(&format!("commit refs/heads/main\ncommitter Graph <graph@example.test> 2000000000 +0000\ndata {}\n{message}\n\n", message.len()));
        }
        let output = command::run(
            dir.path(),
            &["fast-import", "--quiet"],
            Some(data.as_bytes()),
            std::time::Duration::from_secs(60),
        )
        .unwrap();
        assert!(output.status.success());
        for sample in -1..15 {
            let repo = open(dir.path()).unwrap();
            let refs = snapshot_refs(&repo).unwrap();
            let started = std::time::Instant::now();
            let mut first_ms = None;
            let mut count = 0;
            walk(&repo, &refs.tips, &mut |batch| {
                first_ms.get_or_insert_with(|| started.elapsed().as_secs_f64() * 1000.);
                count += batch.len();
                true
            })
            .unwrap();
            assert_eq!(count, 10_000);
            println!(
                "{}",
                serde_json::json!({"sample":sample,"commits":count,
                "firstBatchMs":first_ms,"walkMs":started.elapsed().as_secs_f64()*1000.})
            );
        }
    }

    #[test]
    #[ignore = "one warm-up and 15 real reference-history walk samples"]
    fn benchmark_graph_walk() {
        let path = std::env::var_os("GIT_RUSTLE_BENCH_REPO").expect("benchmark repository");
        for sample in -1..15 {
            let repo = open(Path::new(&path)).unwrap();
            let started = std::time::Instant::now();
            let refs = snapshot_refs(&repo).unwrap();
            let refs_ms = started.elapsed().as_secs_f64() * 1000.;
            let started = std::time::Instant::now();
            let mut first_ms = None;
            let mut commits = 0;
            walk(&repo, &refs.tips, &mut |batch| {
                first_ms.get_or_insert_with(|| started.elapsed().as_secs_f64() * 1000.);
                commits += batch.len();
                std::hint::black_box(batch);
                true
            })
            .unwrap();
            println!(
                "{}",
                serde_json::json!({"sample":sample,"commits":commits,
                "refsMs":refs_ms,"firstBatchMs":first_ms,"walkMs":started.elapsed().as_secs_f64()*1000.})
            );
        }
    }

    #[test]
    fn equal_time_queue_order_is_independent_of_checkout_tip_order() {
        let ids: Vec<_> = (1..=6)
            .map(|n| ObjectId::from_bytes_or_panic(&[n; 20]))
            .collect();
        let order = |tips: Vec<ObjectId>| {
            let mut queue = BinaryHeap::new();
            for id in tips {
                queue.push(QueueEntry { time: 42, id });
            }
            std::iter::from_fn(|| queue.pop().map(|entry| entry.id)).collect::<Vec<_>>()
        };
        let expected = order(ids.clone());
        for head in 0..ids.len() {
            let mut tips = ids.clone();
            tips.rotate_left(head);
            assert_eq!(order(tips), expected);
        }
    }
    fn git(path: &Path, args: &[&str]) -> String {
        String::from_utf8(command::checked(path, args).unwrap())
            .unwrap()
            .trim()
            .to_owned()
    }

    #[test]
    fn checkouts_preserve_rows_and_lanes_with_equal_and_skewed_times() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        git(path, &["init", "-b", "main"]);
        git(path, &["config", "user.name", "Graph Test"]);
        git(path, &["config", "user.email", "graph@example.test"]);
        git(path, &["config", "commit.gpgSign", "false"]);
        let tree = git(path, &["mktree"]);
        let make = |name: &str, parent: Option<&str>, time: &str| {
            let mut args = vec!["commit-tree", &tree, "-m", name];
            if let Some(parent) = parent {
                args.extend(["-p", parent]);
            }
            let output = command::run_env(
                path,
                &args,
                None,
                std::time::Duration::from_secs(10),
                &[
                    ("GIT_AUTHOR_DATE", std::ffi::OsStr::new(time)),
                    ("GIT_COMMITTER_DATE", std::ffi::OsStr::new(time)),
                ],
            )
            .unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        // Equal-time siblings and a clock-skewed parent must remain stable.
        let root = make("root", None, "2000000100 +0000");
        let left = make("left", Some(&root), "2000000000 +0000");
        let right = make("right", Some(&root), "2000000000 +0000");
        let middle = make("middle", Some(&left), "2000000000 +0000");
        let tip = make("tip", Some(&middle), "2000000000 +0000");
        git(path, &["update-ref", "refs/heads/main", &tip]);
        git(path, &["update-ref", "refs/heads/ancestor", &left]);
        git(path, &["update-ref", "refs/heads/other", &right]);
        let graph = || {
            let repo = open(path).unwrap();
            let refs = snapshot_refs(&repo).unwrap();
            let mut rows = Vec::new();
            walk(&repo, &refs.tips, &mut |batch| {
                for (index, record) in batch.commits.iter().enumerate() {
                    rows.push((record.id, record.node_lane, batch.row_cells(index).to_vec()));
                }
                true
            })
            .unwrap();
            rows
        };
        let expected = graph();
        let position = |hex: &str| {
            let id = ObjectId::from_hex(hex.as_bytes()).unwrap();
            expected.iter().position(|row| row.0 == id).unwrap()
        };
        assert!(position(&tip) < position(&middle));
        assert!(position(&middle) < position(&left));
        assert_eq!(expected[position(&tip)].1, expected[position(&middle)].1);
        assert_eq!(expected[position(&middle)].1, expected[position(&left)].1);
        for target in ["other", "main", &root, &left, &middle, &tip, &right] {
            git(path, &["checkout", "--quiet", target]);
            assert_eq!(graph(), expected, "checkout {target}");
        }
        let disconnected = make("disconnected", None, "2000000000 +0000");
        git(path, &["checkout", "--quiet", "--detach", &disconnected]);
        let fallback = graph();
        assert_eq!(&fallback[..expected.len()], expected.as_slice());
        assert_eq!(fallback.last().unwrap().0.to_string(), disconnected);
    }
}
