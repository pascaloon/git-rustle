//! Repository access through gitoxide: opening, reference snapshots, the
//! streaming history walk, and on-demand commit details.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
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
    let mut snap = RefSnapshot::default();
    let mut seen_tips: HashSet<ObjectId> = HashSet::new();

    // HEAD first.
    let head = repo.head()?;
    snap.head_branch = head
        .referent_name()
        .map(|n| n.shorten().to_str_lossy().into_owned());
    if let Some(id) = head.id() {
        let id = id.detach();
        snap.head_id = Some(id);
        if seen_tips.insert(id) {
            snap.tips.push(id);
        }
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
    seq: u64,
    id: ObjectId,
}

impl PartialEq for QueueEntry {
    fn eq(&self, other: &Self) -> bool {
        self.time == other.time && self.seq == other.seq
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
        // Newest first; among equal times, the one enqueued first.
        self.time
            .cmp(&other.time)
            .then_with(|| other.seq.cmp(&self.seq))
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

/// Walk all commits reachable from `tips`, newest committer-time first, laying
/// out the graph as rows and handing them to `emit` in growing batches.
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
    let mut seq: u64 = 0;
    let mut layout = Layout::new();

    let mut enqueue = |id: ObjectId,
                       seen: &mut HashSet<ObjectId>,
                       pending: &mut HashMap<ObjectId, Parsed>,
                       queue: &mut BinaryHeap<QueueEntry>,
                       buf: &mut Vec<u8>| {
        if !seen.insert(id) {
            return;
        }
        if let Some(parsed) = parse_commit(repo, &id, buf) {
            seq += 1;
            queue.push(QueueEntry {
                time: parsed.commit_time,
                seq,
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

    while let Some(entry) = queue.pop() {
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
