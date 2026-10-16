//! Diffing two commits: a recursive tree comparison yielding per-file changes,
//! and a per-file side-by-side line diff (imara-diff, histogram algorithm).
//!
//! The tree walk only descends into subtrees whose ids differ, so comparing
//! two distant commits costs a few hundred tree reads, not a full traversal.

pub mod changes;
pub mod filetree;
pub mod sidebyside;

use std::cmp::Ordering;
use std::collections::HashMap;

use gix::ObjectId;
use gix::bstr::ByteSlice;
use gix::objs::commit::ref_iter::Token;
use gix::objs::tree::EntryKind;
use gix::prelude::FindExt;

use crate::git::Result;
pub use sidebyside::{FileDiff, Line, LineKind, Row};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Deleted,
    Modified,
    Renamed,
}

impl ChangeKind {
    pub fn letter(self) -> char {
        match self {
            ChangeKind::Added => 'A',
            ChangeKind::Deleted => 'D',
            ChangeKind::Modified => 'M',
            ChangeKind::Renamed => 'R',
        }
    }
}

/// One side of a changed path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Side {
    pub id: ObjectId,
    pub kind: EntryKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// New side comes from the working tree rather than an immutable object.
    pub worktree: bool,
    pub remote: Option<Box<crate::remote::files::RemoteFile>>,
    /// Path on the new side (old side for deletions).
    pub path: String,
    /// Old path, for renames.
    pub old_path: Option<String>,
    pub kind: ChangeKind,
    pub old: Option<Side>,
    pub new: Option<Side>,
}

#[derive(Debug, Clone)]
pub struct TreeDiff {
    /// Sorted by path.
    pub files: Vec<FileChange>,
}

fn tree_of(repo: &gix::Repository, commit: &ObjectId, buf: &mut Vec<u8>) -> Result<ObjectId> {
    let iter = repo.objects.find_commit_iter(commit, buf)?;
    for token in iter {
        if let Token::Tree { id } = token? {
            return Ok(id);
        }
    }
    Err("commit has no tree".into())
}

/// Compare the trees of two commits; `old = None` diffs against the empty tree
/// (every file added), used for root commits.
pub fn tree_diff(
    repo: &gix::Repository,
    old: Option<&ObjectId>,
    new: &ObjectId,
) -> Result<TreeDiff> {
    let mut buf = Vec::new();
    let old_tree = old.map(|id| tree_of(repo, id, &mut buf)).transpose()?;
    let new_tree = tree_of(repo, new, &mut buf)?;
    let mut files = Vec::new();
    if old_tree != Some(new_tree) {
        let mut prefix = String::new();
        diff_trees(
            repo,
            old_tree,
            Some(new_tree),
            &mut prefix,
            &mut files,
            &mut buf,
        )?;
    }
    pair_renames(&mut files);
    files.sort_by(|a, b| display_order(&a.path, &b.path));
    Ok(TreeDiff { files })
}

/// Sidebar order: at each level folders before files, names case-insensitively.
pub(crate) fn display_order(a: &str, b: &str) -> Ordering {
    let mut ia = a.split('/').peekable();
    let mut ib = b.split('/').peekable();
    loop {
        let (Some(x), Some(y)) = (ia.next(), ib.next()) else {
            return Ordering::Equal;
        };
        let (x_dir, y_dir) = (ia.peek().is_some(), ib.peek().is_some());
        match y_dir.cmp(&x_dir) {
            Ordering::Equal => {}
            other => return other, // the directory sorts first
        }
        match x
            .to_lowercase()
            .cmp(&y.to_lowercase())
            .then_with(|| x.cmp(y))
        {
            Ordering::Equal if x_dir => continue,
            other => return other,
        }
    }
}

struct Entry {
    name: String,
    id: ObjectId,
    kind: EntryKind,
}

impl Entry {
    fn is_tree(&self) -> bool {
        self.kind == EntryKind::Tree
    }
    fn side(&self) -> Side {
        Side {
            id: self.id,
            kind: self.kind,
        }
    }
}

fn read_entries(
    repo: &gix::Repository,
    id: Option<ObjectId>,
    buf: &mut Vec<u8>,
) -> Result<Vec<Entry>> {
    let Some(id) = id else { return Ok(Vec::new()) };
    let iter = repo.objects.find_tree_iter(&id, buf)?;
    let mut out = Vec::new();
    for e in iter {
        let e = e?;
        out.push(Entry {
            name: e.filename.to_str_lossy().into_owned(),
            id: e.oid.to_owned(),
            kind: e.mode.kind(),
        });
    }
    Ok(out)
}

/// Git's tree ordering: names compare bytewise, with trees as if they had a trailing '/'.
fn tree_order(a: &Entry, b: &Entry) -> Ordering {
    let suffix = |e: &Entry| if e.is_tree() { &b"/"[..] } else { &b""[..] };
    a.name
        .as_bytes()
        .iter()
        .chain(suffix(a))
        .cmp(b.name.as_bytes().iter().chain(suffix(b)))
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}/{name}")
    }
}

fn diff_trees(
    repo: &gix::Repository,
    old: Option<ObjectId>,
    new: Option<ObjectId>,
    prefix: &mut String,
    out: &mut Vec<FileChange>,
    buf: &mut Vec<u8>,
) -> Result<()> {
    let olds = read_entries(repo, old, buf)?;
    let news = read_entries(repo, new, buf)?;
    let (mut i, mut j) = (0, 0);
    loop {
        let order = match (olds.get(i), news.get(j)) {
            (None, None) => break,
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (Some(a), Some(b)) => tree_order(a, b),
        };
        match order {
            Ordering::Less => {
                emit_whole(repo, &olds[i], prefix, false, out, buf)?;
                i += 1;
            }
            Ordering::Greater => {
                emit_whole(repo, &news[j], prefix, true, out, buf)?;
                j += 1;
            }
            Ordering::Equal => {
                let (a, b) = (&olds[i], &news[j]);
                i += 1;
                j += 1;
                if a.id == b.id && a.kind == b.kind {
                    continue;
                }
                // Equal order implies both are trees or both are not.
                if a.is_tree() {
                    let saved = prefix.len();
                    *prefix = join(prefix, &a.name);
                    diff_trees(repo, Some(a.id), Some(b.id), prefix, out, buf)?;
                    prefix.truncate(saved);
                } else {
                    out.push(FileChange {
                        worktree: false,
                        remote: None,
                        path: join(prefix, &a.name),
                        old_path: None,
                        kind: ChangeKind::Modified,
                        old: Some(a.side()),
                        new: Some(b.side()),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Emit an entry that exists on one side only (recursing into trees).
fn emit_whole(
    repo: &gix::Repository,
    e: &Entry,
    prefix: &mut String,
    added: bool,
    out: &mut Vec<FileChange>,
    buf: &mut Vec<u8>,
) -> Result<()> {
    if e.is_tree() {
        let saved = prefix.len();
        *prefix = join(prefix, &e.name);
        let (o, n) = if added {
            (None, Some(e.id))
        } else {
            (Some(e.id), None)
        };
        diff_trees(repo, o, n, prefix, out, buf)?;
        prefix.truncate(saved);
    } else {
        out.push(FileChange {
            worktree: false,
            remote: None,
            path: join(prefix, &e.name),
            old_path: None,
            kind: if added {
                ChangeKind::Added
            } else {
                ChangeKind::Deleted
            },
            old: (!added).then(|| e.side()),
            new: added.then(|| e.side()),
        });
    }
    Ok(())
}

/// Exact-content rename detection: a deleted and an added blob with the same id.
pub(crate) fn pair_renames(files: &mut Vec<FileChange>) {
    let mut deleted: HashMap<ObjectId, Vec<usize>> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        if f.kind == ChangeKind::Deleted
            && let Some(s) = f.old
        {
            deleted.entry(s.id).or_default().push(i);
        }
    }
    if deleted.is_empty() {
        return;
    }
    let mut remove = vec![false; files.len()];
    for i in 0..files.len() {
        if files[i].kind != ChangeKind::Added {
            continue;
        }
        let Some(id) = files[i].new.map(|s| s.id) else {
            continue;
        };
        if let Some(cands) = deleted.get_mut(&id)
            && let Some(d) = cands.pop()
        {
            let old_path = std::mem::take(&mut files[d].path);
            files[i].old = files[d].old;
            files[i].old_path = Some(old_path);
            files[i].kind = ChangeKind::Renamed;
            remove[d] = true;
        }
    }
    let mut k = 0;
    files.retain(|_| {
        let keep = !remove[k];
        k += 1;
        keep
    });
}

/// Largest blob (per side) we are willing to line-diff.
const MAX_BLOB: usize = 8 * 1024 * 1024;

/// Compute the side-by-side diff of one changed file.
pub fn file_diff(repo: &gix::Repository, change: &FileChange) -> Result<FileDiff> {
    if let Some(source) = &change.remote {
        return crate::remote::files::file(repo, change, source).map_err(Into::into);
    }
    let mut old_buf = Vec::new();
    let mut new_buf = Vec::new();
    let old = load_side(repo, change.old, &mut old_buf)?;
    let new = load_new(repo, change, &mut new_buf)?;
    let (old, new) = match (old, new) {
        (Loaded::Text(o), Loaded::Text(n)) => (o, n),
        (Loaded::Text(o), Loaded::Missing) => (o, &b""[..]),
        (Loaded::Missing, Loaded::Text(n)) => (&b""[..], n),
        (Loaded::Missing, Loaded::Missing) => (&b""[..], &b""[..]),
        (Loaded::Binary, _) | (_, Loaded::Binary) => {
            return Ok(FileDiff::message(format!(
                "Binary files differ ({} → {})",
                size_of(change.old, &old_buf),
                if change.worktree {
                    format!("{} B (working tree)", new_buf.len())
                } else {
                    size_of(change.new, &new_buf)
                }
            )));
        }
        (Loaded::TooLarge, _) | (_, Loaded::TooLarge) => {
            return Ok(FileDiff::message("File too large to diff".to_string()));
        }
    };
    Ok(sidebyside::build(old, new))
}

/// Line counts for one changed file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stat {
    Lines {
        added: u32,
        removed: u32,
    },
    /// Binary or too large: no line counts.
    Opaque,
}

/// Added/removed line counts without materialising rows. Pure additions and
/// deletions just count lines; only modifications run the diff algorithm.
pub fn file_stats(repo: &gix::Repository, change: &FileChange) -> Result<Stat> {
    if let Some(source) = &change.remote {
        return Ok(source.stat.unwrap_or(Stat::Opaque));
    }
    let mut old_buf = Vec::new();
    let mut new_buf = Vec::new();
    let old = load_side(repo, change.old, &mut old_buf)?;
    let new = load_new(repo, change, &mut new_buf)?;
    let count = |data: &[u8]| split_lines_count(data);
    Ok(match (old, new) {
        (Loaded::Binary | Loaded::TooLarge, _) | (_, Loaded::Binary | Loaded::TooLarge) => {
            Stat::Opaque
        }
        (Loaded::Missing, Loaded::Missing) => Stat::Lines {
            added: 0,
            removed: 0,
        },
        (Loaded::Text(o), Loaded::Missing) => Stat::Lines {
            added: 0,
            removed: count(o),
        },
        (Loaded::Missing, Loaded::Text(n)) => Stat::Lines {
            added: count(n),
            removed: 0,
        },
        (Loaded::Text(o), Loaded::Text(n)) => {
            let (added, removed) = sidebyside::counts(o, n);
            Stat::Lines { added, removed }
        }
    })
}

fn split_lines_count(data: &[u8]) -> u32 {
    let n = data.iter().filter(|&&b| b == b'\n').count();
    (n + usize::from(!data.is_empty() && !data.ends_with(b"\n"))) as u32
}

enum Loaded<'a> {
    Missing,
    Text(&'a [u8]),
    Binary,
    TooLarge,
}

fn load_new<'a>(
    repo: &gix::Repository,
    change: &FileChange,
    buf: &'a mut Vec<u8>,
) -> Result<Loaded<'a>> {
    if !change.worktree {
        return load_side(repo, change.new, buf);
    }
    let root = repo
        .workdir()
        .ok_or("Pending changes need a working tree")?;
    let path = root.join(&change.path);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Loaded::Missing),
        Err(error) => return Err(error.into()),
    };
    if metadata.is_dir() {
        let id = crate::git::command::checked(&path, &["rev-parse", "HEAD"])?;
        buf.extend_from_slice(b"Subproject commit ");
        buf.extend_from_slice(&id);
    } else if metadata.file_type().is_symlink() {
        buf.extend_from_slice(std::fs::read_link(&path)?.to_string_lossy().as_bytes());
    } else {
        if metadata.len() > MAX_BLOB as u64 {
            return Ok(Loaded::TooLarge);
        }
        use std::io::Read;
        std::fs::File::open(&path)?
            .take(MAX_BLOB as u64 + 1)
            .read_to_end(buf)?;
    }
    if buf.len() > MAX_BLOB {
        return Ok(Loaded::TooLarge);
    }
    if buf[..buf.len().min(8000)].contains(&0) {
        return Ok(Loaded::Binary);
    }
    Ok(Loaded::Text(buf))
}

fn load_side<'a>(
    repo: &gix::Repository,
    side: Option<Side>,
    buf: &'a mut Vec<u8>,
) -> Result<Loaded<'a>> {
    let Some(side) = side else {
        return Ok(Loaded::Missing);
    };
    match side.kind {
        EntryKind::Commit => {
            // Submodule: show the pointed-to commit.
            buf.clear();
            buf.extend_from_slice(format!("Subproject commit {}\n", side.id).as_bytes());
            return Ok(Loaded::Text(&buf[..]));
        }
        EntryKind::Tree => return Ok(Loaded::Missing),
        _ => {}
    }
    let blob = repo.objects.find_blob(&side.id, buf)?;
    let data = blob.data;
    if data.len() > MAX_BLOB {
        return Ok(Loaded::TooLarge);
    }
    if data[..data.len().min(8000)].contains(&0) {
        return Ok(Loaded::Binary);
    }
    Ok(Loaded::Text(data))
}

fn size_of(side: Option<Side>, buf: &[u8]) -> String {
    match side {
        None => "none".to_string(),
        Some(_) => {
            let n = buf.len();
            if n >= 1 << 20 {
                format!("{:.1} MiB", n as f64 / (1 << 20) as f64)
            } else if n >= 1 << 10 {
                format!("{:.1} KiB", n as f64 / 1024.0)
            } else {
                format!("{n} B")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, tree: bool) -> Entry {
        Entry {
            name: name.to_string(),
            id: ObjectId::null(gix::hash::Kind::Sha1),
            kind: if tree {
                EntryKind::Tree
            } else {
                EntryKind::Blob
            },
        }
    }

    #[test]
    fn tree_order_matches_git() {
        // "a" < "a-b" < "a/" (tree a) < "a0"
        assert_eq!(
            tree_order(&entry("a", false), &entry("a-b", false)),
            Ordering::Less
        );
        assert_eq!(
            tree_order(&entry("a-b", false), &entry("a", true)),
            Ordering::Less
        );
        assert_eq!(
            tree_order(&entry("a", true), &entry("a0", false)),
            Ordering::Less
        );
        assert_eq!(
            tree_order(&entry("a", false), &entry("a", true)),
            Ordering::Less
        );
        assert_eq!(
            tree_order(&entry("x", true), &entry("x", true)),
            Ordering::Equal
        );
    }

    #[test]
    fn display_order_is_folders_first() {
        let mut v = vec![
            "src/ui/list.rs",
            "README.md",
            "src/main.rs",
            "Cargo.toml",
            "docs/a.md",
            "src/ui/mod.rs",
        ];
        v.sort_by(|a, b| display_order(a, b));
        assert_eq!(
            v,
            vec![
                "docs/a.md",
                "src/ui/list.rs",
                "src/ui/mod.rs",
                "src/main.rs",
                "Cargo.toml",
                "README.md"
            ]
        );
    }

    #[test]
    fn exact_renames_are_paired() {
        let id = |b: u8| ObjectId::from_bytes_or_panic(&[b; 20]);
        let side = |b: u8| {
            Some(Side {
                id: id(b),
                kind: EntryKind::Blob,
            })
        };
        let mut files = vec![
            FileChange {
                worktree: false,
                remote: None,
                path: "old/a.rs".into(),
                old_path: None,
                kind: ChangeKind::Deleted,
                old: side(1),
                new: None,
            },
            FileChange {
                worktree: false,
                remote: None,
                path: "b.rs".into(),
                old_path: None,
                kind: ChangeKind::Modified,
                old: side(2),
                new: side(3),
            },
            FileChange {
                worktree: false,
                remote: None,
                path: "new/a.rs".into(),
                old_path: None,
                kind: ChangeKind::Added,
                old: None,
                new: side(1),
            },
            FileChange {
                worktree: false,
                remote: None,
                path: "c.rs".into(),
                old_path: None,
                kind: ChangeKind::Added,
                old: None,
                new: side(4),
            },
        ];
        pair_renames(&mut files);
        assert_eq!(files.len(), 3);
        let r = files
            .iter()
            .find(|f| f.kind == ChangeKind::Renamed)
            .unwrap();
        assert_eq!(r.path, "new/a.rs");
        assert_eq!(r.old_path.as_deref(), Some("old/a.rs"));
        assert_eq!(r.old, side(1));
        assert!(
            files
                .iter()
                .any(|f| f.path == "c.rs" && f.kind == ChangeKind::Added)
        );
    }
}
