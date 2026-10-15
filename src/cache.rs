//! On-disk cache of a laid-out history.
//!
//! Flat little-endian layout designed to be written and read as a few large
//! blocks with no per-commit allocation:
//!
//! ```text
//! header | git_dir (utf8) | commit records | parent ids | text arena | graph cells (RLE)
//! ```
//!
//! Graph rows are stored as deltas against the previous row: consecutive rows
//! of a wide graph differ in only a handful of columns (the node, bends, new
//! lanes), so a 200-lane row costs a few bytes instead of 200.

use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};

use gix::ObjectId;

use crate::graph::layout::GraphCell;
use crate::model::{CommitRec, History, Span};

const MAGIC: &[u8; 4] = b"GRSL";
const VERSION: u32 = 2;
const HEADER_LEN: usize = 48;

/// What was read from disk.
pub struct Cached {
    pub fingerprint: u64,
    pub history: History,
}

/// Location of the cache file for a given git directory.
pub fn cache_path(git_dir: &Path) -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    git_dir.to_string_lossy().to_lowercase().hash(&mut h);
    let name = format!("{:016x}.bin", h.finish());
    Some(base.join("git-rustle").join("cache").join(name))
}

pub fn read(path: &Path) -> io::Result<Cached> {
    use std::io::Read;
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "corrupt cache");
    let mut file = fs::File::open(path)?;
    // Check magic and version before pulling in a possibly large, useless file.
    let mut head = [0u8; 8];
    file.read_exact(&mut head)?;
    if &head[..4] != MAGIC || u32::from_le_bytes(head[4..8].try_into().unwrap()) != VERSION {
        return Err(invalid());
    }
    let len = file.metadata().map(|m| m.len() as usize).unwrap_or(0);
    let mut data = Vec::with_capacity(len);
    data.extend_from_slice(&head);
    file.read_to_end(&mut data)?;
    parse(&data).ok_or_else(invalid)
}

pub fn write(path: &Path, git_dir: &Path, fingerprint: u64, history: &History) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serialize(git_dir, fingerprint, history);
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    fs::write(&tmp, &bytes)?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(_) => {
            // Windows refuses to rename over an open/existing file in some cases; replace explicitly.
            let _ = fs::remove_file(path);
            fs::rename(&tmp, path).inspect_err(|_| {
                let _ = fs::remove_file(&tmp);
            })
        }
    }
}

struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn span(&mut self, s: Span) {
        self.u32(s.off);
        self.u16(s.len);
    }
}

fn id_len(history: &History) -> u8 {
    history
        .commits
        .first()
        .map(|c| c.id.as_bytes().len() as u8)
        .unwrap_or(20)
}

fn record_len(id_len: u8) -> usize {
    id_len as usize + 6 + 8 + 4 + 8 + 6 + 6 + 6 + 2
}

/// Encode graph rows (in record order) as deltas against the previous row:
/// `[ncols u16][nchanges u16]([col u16][cell u8])*`. Columns past the end of
/// the previous row are compared against `GraphCell::EMPTY`.
fn encode_rows(h: &History) -> Vec<u8> {
    let mut out = Vec::with_capacity(h.commits.len() * 16);
    let mut prev: Vec<GraphCell> = Vec::new();
    for rec in &h.commits {
        let row = &h.cells[rec.row.range()];
        let changes_at = out.len();
        out.extend_from_slice(&(row.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        let mut n = 0u16;
        for (i, &cell) in row.iter().enumerate() {
            let before = prev.get(i).copied().unwrap_or(GraphCell::EMPTY);
            if cell != before {
                out.extend_from_slice(&(i as u16).to_le_bytes());
                out.push(cell.raw());
                n += 1;
            }
        }
        out[changes_at + 2..changes_at + 4].copy_from_slice(&n.to_le_bytes());
        prev.clear();
        prev.extend_from_slice(row);
    }
    out
}

/// Decode rows back into a cells arena, rewriting each record's `row` span.
fn decode_rows(data: &[u8], commits: &mut [CommitRec], total_cells: usize) -> Option<Vec<GraphCell>> {
    let mut cells: Vec<GraphCell> = Vec::with_capacity(total_cells);
    let mut prev: Vec<GraphCell> = Vec::new();
    let mut r = Reader { data, pos: 0 };
    for rec in commits.iter_mut() {
        let ncols = r.u16()? as usize;
        let nchanges = r.u16()? as usize;
        prev.resize(ncols, GraphCell::EMPTY);
        for _ in 0..nchanges {
            let col = r.u16()? as usize;
            let raw = r.u8()?;
            *prev.get_mut(col)? = GraphCell::from_raw(raw);
        }
        let off = cells.len();
        if off + ncols > total_cells {
            return None;
        }
        cells.extend_from_slice(&prev);
        rec.row = Span {
            off: off as u32,
            len: ncols as u16,
        };
    }
    (r.pos == data.len() && cells.len() == total_cells).then_some(cells)
}

fn serialize(git_dir: &Path, fingerprint: u64, h: &History) -> Vec<u8> {
    let id_len = id_len(h);
    let git_dir = git_dir.to_string_lossy();
    let git_dir = git_dir.as_bytes();
    let git_dir = &git_dir[..git_dir.len().min(u16::MAX as usize)];
    let runs = encode_rows(h);
    let total = HEADER_LEN
        + 2
        + git_dir.len()
        + h.commits.len() * record_len(id_len)
        + h.parents.len() * id_len as usize
        + h.text.len()
        + runs.len();
    let mut w = Writer(Vec::with_capacity(total));
    w.0.extend_from_slice(MAGIC);
    w.u32(VERSION);
    w.u8(id_len);
    w.u8(h.complete as u8);
    w.u16(0);
    w.u64(fingerprint);
    w.u32(h.commits.len() as u32);
    w.u32(h.parents.len() as u32);
    w.u32(h.text.len() as u32);
    w.u32(h.cells.len() as u32);
    w.u16(h.max_lanes);
    w.u16(0);
    w.u32(runs.len() as u32);
    w.u32(0);
    debug_assert_eq!(w.0.len(), HEADER_LEN);
    w.u16(git_dir.len() as u16);
    w.0.extend_from_slice(git_dir);
    for c in &h.commits {
        w.0.extend_from_slice(c.id.as_bytes());
        w.span(c.parents);
        w.i64(c.author_time);
        w.i32(c.author_offset);
        w.i64(c.commit_time);
        w.span(c.author);
        w.span(c.subject);
        w.span(c.row);
        w.u16(c.node_lane);
    }
    for p in &h.parents {
        w.0.extend_from_slice(p.as_bytes());
    }
    w.0.extend_from_slice(&h.text);
    w.0.extend_from_slice(&runs);
    w.0
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn i32(&mut self) -> Option<i32> {
        Some(i32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn span(&mut self, limit: usize) -> Option<Span> {
        let off = self.u32()?;
        let len = self.u16()?;
        if off as usize + len as usize > limit {
            return None;
        }
        Some(Span { off, len })
    }
}

fn parse(data: &[u8]) -> Option<Cached> {
    let mut r = Reader { data, pos: 0 };
    if r.take(4)? != MAGIC || r.u32()? != VERSION {
        return None;
    }
    let id_len = r.u8()?;
    if id_len != 20 && id_len != 32 {
        return None;
    }
    let complete = r.u8()? != 0;
    r.u16()?;
    let fingerprint = r.u64()?;
    let commit_count = r.u32()? as usize;
    let parent_count = r.u32()? as usize;
    let text_len = r.u32()? as usize;
    let cells_len = r.u32()? as usize;
    let max_lanes = r.u16()?;
    r.u16()?;
    let rows_len = r.u32()? as usize;
    r.u32()?;
    debug_assert_eq!(r.pos, HEADER_LEN);
    let dir_len = r.u16()? as usize;
    r.take(dir_len)?;

    let rec_len = record_len(id_len);
    let records = r.take(commit_count.checked_mul(rec_len)?)?;
    let parents_raw = r.take(parent_count.checked_mul(id_len as usize)?)?;
    let text = r.take(text_len)?;
    let rows_raw = r.take(rows_len)?;
    if r.pos != data.len() {
        return None;
    }

    let mut commits = Vec::with_capacity(commit_count);
    let mut rr = Reader {
        data: records,
        pos: 0,
    };
    for _ in 0..commit_count {
        let id = ObjectId::try_from(rr.take(id_len as usize)?).ok()?;
        let parents = rr.span(parent_count)?;
        let author_time = rr.i64()?;
        let author_offset = rr.i32()?;
        let commit_time = rr.i64()?;
        let author = rr.span(text_len)?;
        let subject = rr.span(text_len)?;
        // Row spans are rebuilt by `decode_rows`; the stored value is only validated.
        let row = rr.span(cells_len)?;
        let node_lane = rr.u16()?;
        commits.push(CommitRec {
            id,
            parents,
            author_time,
            author_offset,
            commit_time,
            author,
            subject,
            row,
            node_lane,
        });
    }
    let parents = parents_raw
        .chunks_exact(id_len as usize)
        .map(|c| ObjectId::try_from(c).ok())
        .collect::<Option<Vec<_>>>()?;
    let cells = decode_rows(rows_raw, &mut commits, cells_len)?;
    std::str::from_utf8(text).ok()?;

    Some(Cached {
        fingerprint,
        history: History {
            commits,
            parents,
            text: text.to_vec(),
            cells,
            max_lanes,
            complete,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::layout::{CellKind, Layout};

    fn id(n: u8) -> ObjectId {
        let mut bytes = [0u8; 20];
        bytes[0] = n;
        bytes[19] = n.wrapping_mul(7);
        ObjectId::from_bytes_or_panic(&bytes)
    }

    fn sample() -> History {
        let mut h = History::default();
        let mut layout = Layout::new();
        let commits = [
            (id(4), vec![id(3), id(2)], "Ada", "Merge branch 'feature'"),
            (id(3), vec![id(1)], "Bob", "Add thing 🎉"),
            (id(2), vec![id(1)], "Ada", "Fix other thing"),
            (id(1), vec![], "Céline", "Initial commit"),
        ];
        for (cid, parents, author, subject) in commits {
            let row = layout.push(&cid, &parents);
            let parents = h.push_parents(&parents);
            let author = h.push_text(author.as_bytes());
            let subject = h.push_text(subject.as_bytes());
            let cells = h.push_cells(&row.cells);
            h.commits.push(CommitRec {
                id: cid,
                parents,
                author_time: 1_700_000_000 + cid.as_bytes()[0] as i64,
                author_offset: -14400,
                commit_time: 1_700_000_100,
                author,
                subject,
                row: cells,
                node_lane: row.node_lane,
            });
        }
        h.complete = true;
        h
    }

    #[test]
    fn round_trip() {
        let h = sample();
        let bytes = serialize(Path::new("C:\\repo\\.git"), 0xDEAD_BEEF, &h);
        let cached = parse(&bytes).expect("parses");
        assert_eq!(cached.fingerprint, 0xDEAD_BEEF);
        let g = cached.history;
        assert_eq!(g.len(), h.len());
        assert_eq!(g.max_lanes, h.max_lanes);
        assert!(g.complete);
        for i in 0..h.len() {
            assert_eq!(g.commits[i].id, h.commits[i].id);
            assert_eq!(g.author(i), h.author(i));
            assert_eq!(g.subject(i), h.subject(i));
            assert_eq!(g.parents(i), h.parents(i));
            assert_eq!(g.row_cells(i), h.row_cells(i));
            assert_eq!(g.commits[i].node_lane, h.commits[i].node_lane);
            assert_eq!(g.commits[i].author_offset, -14400);
        }
    }

    #[test]
    fn row_delta_encoding_is_compact_for_wide_graphs() {
        // 300 lanes of pass-through with a node walking down one lane: each row
        // differs from the previous in two cells.
        let mut h = History::default();
        let lanes = 300usize;
        for row in 0..50usize {
            let cells: Vec<GraphCell> = (0..lanes)
                .map(|i| {
                    if i == row % lanes {
                        GraphCell::new(CellKind::Node, (i % 10) as u8)
                    } else {
                        GraphCell::new(CellKind::Vertical, (i % 10) as u8)
                    }
                })
                .collect();
            let span = h.push_cells(&cells);
            h.commits.push(CommitRec {
                id: id(row as u8),
                parents: Span::default(),
                author_time: 0,
                author_offset: 0,
                commit_time: 0,
                author: Span::default(),
                subject: Span::default(),
                row: span,
                node_lane: (row % lanes) as u16,
            });
        }
        let encoded = encode_rows(&h);
        // First row: 300 changes; following rows: 2 changes each.
        assert!(encoded.len() < 4 * 50 + 3 * 300 + 3 * 2 * 49 + 1);
        let mut commits = h.commits.clone();
        let cells = decode_rows(&encoded, &mut commits, h.cells.len()).unwrap();
        assert_eq!(cells, h.cells);
        for (a, b) in commits.iter().zip(&h.commits) {
            assert_eq!(a.row, b.row);
        }
        assert!(decode_rows(&encoded, &mut commits, h.cells.len() - 1).is_none());
        assert!(decode_rows(&encoded[..encoded.len() - 1], &mut commits, h.cells.len()).is_none());
    }

    #[test]
    fn rejects_truncated_and_garbage() {
        let h = sample();
        let bytes = serialize(Path::new("x"), 1, &h);
        assert!(parse(&bytes[..bytes.len() - 1]).is_none());
        assert!(parse(b"nope").is_none());
        let mut bad = bytes.clone();
        bad[4] = 0xFF; // version
        assert!(parse(&bad).is_none());
    }

    #[test]
    fn append_rebases_spans() {
        let a = sample();
        let mut b = History::default();
        let before = a.len();
        b.append(sample());
        b.append(sample());
        assert_eq!(b.len(), before * 2);
        for i in 0..before {
            assert_eq!(b.subject(i), b.subject(i + before));
            assert_eq!(b.row_cells(i), b.row_cells(i + before));
            assert_eq!(b.parents(i), b.parents(i + before));
        }
    }
}
