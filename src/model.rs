//! Shared, allocation-light data model for the commit history and its graph layout.
//!
//! Strings and graph cells live in flat arenas addressed by [`Span`]s so a
//! `History` can be appended in batches, written to the cache as a handful of
//! memcpy-style blocks, and read back without per-commit allocations.

use gix::ObjectId;

use crate::graph::layout::GraphCell;

/// A range inside one of the `History` arenas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub off: u32,
    pub len: u16,
}

impl Span {
    #[inline]
    pub fn range(self) -> std::ops::Range<usize> {
        self.off as usize..self.off as usize + self.len as usize
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CommitRec {
    pub id: ObjectId,
    /// Parents, as a span into `History::parents`.
    pub parents: Span,
    pub author_time: i64,
    /// Author timezone offset in seconds east of UTC.
    pub author_offset: i32,
    pub commit_time: i64,
    /// Author name, span into `History::text`.
    pub author: Span,
    /// First line of the message, span into `History::text`.
    pub subject: Span,
    /// Graph cells for this row, span into `History::cells`.
    pub row: Span,
    pub node_lane: u16,
}

#[derive(Debug, Default, Clone)]
pub struct History {
    pub commits: Vec<CommitRec>,
    pub parents: Vec<ObjectId>,
    pub text: Vec<u8>,
    pub cells: Vec<GraphCell>,
    /// Derived remote edge styles, parallel to `cells`; empty for local history.
    /// Rebuilt on the preview worker rather than serialized in the graph cache.
    pub remote_cells: Vec<u8>,
    pub max_lanes: u16,
    pub complete: bool,
}

impl History {
    pub fn len(&self) -> usize {
        self.commits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commits.is_empty()
    }

    #[inline]
    pub fn author(&self, i: usize) -> &str {
        self.str_at(self.commits[i].author)
    }

    #[inline]
    pub fn subject(&self, i: usize) -> &str {
        self.str_at(self.commits[i].subject)
    }

    #[inline]
    pub fn parents(&self, i: usize) -> &[ObjectId] {
        &self.parents[self.commits[i].parents.range()]
    }

    #[inline]
    pub fn row_cells(&self, i: usize) -> &[GraphCell] {
        &self.cells[self.commits[i].row.range()]
    }

    #[inline]
    fn str_at(&self, s: Span) -> &str {
        // Text is always pushed through `push_text`, which guarantees UTF-8.
        std::str::from_utf8(&self.text[s.range()]).unwrap_or("")
    }

    /// Append `bytes` (lossily converted to UTF-8) to the text arena and return its span.
    pub fn push_text(&mut self, bytes: &[u8]) -> Span {
        let off = self.text.len() as u32;
        match std::str::from_utf8(bytes) {
            Ok(s) => self.text.extend_from_slice(s.as_bytes()),
            Err(_) => {
                let s = String::from_utf8_lossy(bytes);
                self.text.extend_from_slice(s.as_bytes());
            }
        }
        let len = (self.text.len() as u32 - off).min(u16::MAX as u32);
        // Truncate on a char boundary if the (absurdly long) string overflowed the span.
        let mut end = off as usize + len as usize;
        while end > off as usize && end < self.text.len() && (self.text[end] & 0xC0) == 0x80 {
            end -= 1;
        }
        if end < self.text.len() {
            self.text.truncate(end);
        }
        Span {
            off,
            len: (end - off as usize) as u16,
        }
    }

    pub fn push_parents(&mut self, parents: &[ObjectId]) -> Span {
        let off = self.parents.len() as u32;
        self.parents.extend_from_slice(parents);
        Span {
            off,
            len: parents.len().min(u16::MAX as usize) as u16,
        }
    }

    pub fn push_cells(&mut self, cells: &[GraphCell]) -> Span {
        let off = self.cells.len() as u32;
        self.cells.extend_from_slice(cells);
        self.max_lanes = self.max_lanes.max(cells.len() as u16);
        Span {
            off,
            len: cells.len() as u16,
        }
    }

    /// Append another history chunk, rebasing its spans into this one's arenas.
    pub fn append(&mut self, chunk: History) {
        let text_base = self.text.len() as u32;
        let parents_base = self.parents.len() as u32;
        let cells_base = self.cells.len() as u32;
        self.text.extend_from_slice(&chunk.text);
        self.parents.extend_from_slice(&chunk.parents);
        self.cells.extend_from_slice(&chunk.cells);
        self.commits.reserve(chunk.commits.len());
        for mut c in chunk.commits {
            c.author.off += text_base;
            c.subject.off += text_base;
            c.parents.off += parents_base;
            c.row.off += cells_base;
            self.commits.push(c);
        }
        self.max_lanes = self.max_lanes.max(chunk.max_lanes);
        self.complete = chunk.complete;
    }

    /// Index of the commit with `id`, by linear scan (used rarely: HEAD lookup, ref jumps).
    pub fn index_of(&self, id: &ObjectId) -> Option<usize> {
        self.commits.iter().position(|c| &c.id == id)
    }
}
