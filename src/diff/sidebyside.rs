//! Turn two texts into rows for a side-by-side view: hunks with a few lines of
//! context, separators for skipped stretches, and per-line change kinds with
//! intra-line highlight ranges for modified pairs.

use std::ops::Range;

use imara_diff::{Algorithm, Diff, InternedInput};

/// Unchanged lines shown around each hunk.
pub const CONTEXT: usize = 3;
const TAB_WIDTH: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Removed,
    Added,
    /// One half of a removed/added pair shown on the same row.
    Changed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// 1-based line number in its file.
    pub no: u32,
    /// Tabs expanded, line ending stripped.
    pub text: String,
    pub kind: LineKind,
    /// Character ranges (into `text`) that differ from the paired line.
    pub hl: Vec<Range<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Line {
        left: Option<Line>,
        right: Option<Line>,
    },
    /// `skipped` unchanged lines are not shown here.
    Separator {
        skipped: u32,
    },
    Message(String),
}

#[derive(Debug, Clone, Default)]
pub struct FileDiff {
    pub rows: Vec<Row>,
    pub added: u32,
    pub removed: u32,
    /// Digits needed for the widest line number.
    pub num_width: u8,
    /// Display width of the widest line (for horizontal scrolling).
    pub max_width: usize,
}

impl FileDiff {
    pub fn message(text: String) -> Self {
        FileDiff {
            rows: vec![Row::Message(text)],
            ..Default::default()
        }
    }
}

fn decode(line: &[u8]) -> String {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let s = String::from_utf8_lossy(line);
    if !s.contains('\t') {
        return s.into_owned();
    }
    let mut out = String::with_capacity(s.len() + 8);
    let mut col = 0usize;
    for ch in s.chars() {
        if ch == '\t' {
            let n = TAB_WIDTH - (col % TAB_WIDTH);
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            out.push(ch);
            col += 1;
        }
    }
    out
}

fn split_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut v: Vec<&[u8]> = data.split_inclusive(|&b| b == b'\n').collect();
    if v.last().is_some_and(|l| l.is_empty()) {
        v.pop();
    }
    v
}

/// Added and removed line counts only.
pub fn counts(old: &[u8], new: &[u8]) -> (u32, u32) {
    let input = InternedInput::new(
        imara_diff::sources::byte_lines(old),
        imara_diff::sources::byte_lines(new),
    );
    let diff = Diff::compute(Algorithm::Histogram, &input);
    (diff.count_additions(), diff.count_removals())
}

/// Build side-by-side rows from the raw old and new file contents.
pub fn build(old: &[u8], new: &[u8]) -> FileDiff {
    let old_lines = split_lines(old);
    let new_lines = split_lines(new);
    let input = InternedInput::new(
        imara_diff::sources::byte_lines(old),
        imara_diff::sources::byte_lines(new),
    );
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);

    let mut max_width = 0usize;
    let mut line = |no: usize, text: &[u8], kind: LineKind| {
        let text = decode(text);
        max_width = max_width.max(crate::term::screen::str_width(&text));
        Line {
            no: no as u32 + 1,
            text,
            kind,
            hl: Vec::new(),
        }
    };
    let mut rows: Vec<Row> = Vec::new();
    let (mut added, mut removed) = (0u32, 0u32);
    // First unchanged (old, new) line pair after the previous hunk.
    let mut cur = (0usize, 0usize);
    let mut first = true;
    for hunk in diff.hunks() {
        let (b, a) = (hunk.before.start as usize, hunk.after.start as usize);
        let gap = b - cur.0;
        // Unchanged lines between hunks: trailing context of the previous hunk,
        // a separator for the skipped stretch, leading context of this one.
        let lead_from = if first {
            if gap > CONTEXT {
                rows.push(Row::Separator {
                    skipped: (gap - CONTEXT) as u32,
                });
                b - CONTEXT
            } else {
                cur.0
            }
        } else if gap > 2 * CONTEXT {
            for (k, n) in (cur.0..cur.0 + CONTEXT).zip(cur.1..) {
                rows.push(Row::Line {
                    left: Some(line(k, old_lines[k], LineKind::Context)),
                    right: Some(line(n, new_lines[n], LineKind::Context)),
                });
            }
            rows.push(Row::Separator {
                skipped: (gap - 2 * CONTEXT) as u32,
            });
            b - CONTEXT
        } else {
            cur.0
        };
        first = false;
        for (k, n) in (lead_from..b).zip(a - (b - lead_from)..) {
            rows.push(Row::Line {
                left: Some(line(k, old_lines[k], LineKind::Context)),
                right: Some(line(n, new_lines[n], LineKind::Context)),
            });
        }
        // The change itself: pair removed/added lines row by row.
        let before = hunk.before.start as usize..hunk.before.end as usize;
        let after = hunk.after.start as usize..hunk.after.end as usize;
        let paired = before.len().min(after.len());
        for k in 0..before.len().max(after.len()) {
            let mut left = (k < before.len()).then(|| {
                let o = before.start + k;
                line(
                    o,
                    old_lines[o],
                    if k < paired {
                        LineKind::Changed
                    } else {
                        LineKind::Removed
                    },
                )
            });
            let mut right = (k < after.len()).then(|| {
                let n = after.start + k;
                line(
                    n,
                    new_lines[n],
                    if k < paired {
                        LineKind::Changed
                    } else {
                        LineKind::Added
                    },
                )
            });
            if let (Some(l), Some(r)) = (left.as_mut(), right.as_mut()) {
                let (lh, rh) = intraline(&l.text, &r.text);
                l.hl = lh;
                r.hl = rh;
            }
            rows.push(Row::Line { left, right });
        }
        removed += before.len() as u32;
        added += after.len() as u32;
        cur = (before.end, after.end);
    }
    // Trailing context.
    let remaining = old_lines.len() - cur.0;
    let show = remaining.min(CONTEXT);
    for k in 0..show {
        let (o, n) = (cur.0 + k, cur.1 + k);
        rows.push(Row::Line {
            left: Some(line(o, old_lines[o], LineKind::Context)),
            right: Some(line(n, new_lines[n], LineKind::Context)),
        });
    }
    if remaining > show {
        rows.push(Row::Separator {
            skipped: (remaining - show) as u32,
        });
    }
    if first {
        // No hunks at all.
        rows.clear();
        rows.push(Row::Message(if old.is_empty() && new.is_empty() {
            "Empty file".to_string()
        } else {
            "Contents identical (mode or type change only)".to_string()
        }));
    }
    FileDiff {
        rows,
        added,
        removed,
        num_width: digits(old_lines.len().max(new_lines.len()) as u32),
        max_width,
    }
}

fn digits(n: u32) -> u8 {
    let mut d = 1u8;
    let mut v = n;
    while v >= 10 {
        v /= 10;
        d += 1;
    }
    d
}

/// Highlight ranges for a changed pair: everything outside the common prefix
/// and suffix. Dropped when the difference covers most of both lines, since a
/// highlight that spans nearly everything only adds noise.
fn intraline(a: &str, b: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let ac: Vec<char> = a.chars().collect();
    let bc: Vec<char> = b.chars().collect();
    let prefix = ac.iter().zip(&bc).take_while(|(x, y)| x == y).count();
    let max_suffix = ac.len().min(bc.len()) - prefix;
    let suffix = ac
        .iter()
        .rev()
        .zip(bc.iter().rev())
        .take(max_suffix)
        .take_while(|(x, y)| x == y)
        .count();
    let ra = prefix..ac.len() - suffix;
    let rb = prefix..bc.len() - suffix;
    let frac = |r: &Range<usize>, n: usize| {
        if n == 0 {
            0.0
        } else {
            r.len() as f64 / n as f64
        }
    };
    if frac(&ra, ac.len()) > 0.7 && frac(&rb, bc.len()) > 0.7 {
        return (Vec::new(), Vec::new());
    }
    let wrap = |r: Range<usize>| if r.is_empty() { Vec::new() } else { vec![r] };
    (wrap(ra), wrap(rb))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(d: &FileDiff) -> Vec<String> {
        d.rows
            .iter()
            .map(|r| match r {
                Row::Line { left, right } => {
                    let k = |l: &Option<Line>| match l {
                        None => "-".to_string(),
                        Some(l) => format!("{}{:?}", l.no, l.kind).chars().take(3).collect(),
                    };
                    format!("{}|{}", k(left), k(right))
                }
                Row::Separator { skipped } => format!("~{skipped}"),
                Row::Message(m) => m.clone(),
            })
            .collect()
    }

    #[test]
    fn hunks_get_context_and_separators() {
        let old: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        let new = old
            .replace("line 10\n", "line ten\n")
            .replace("line 3\n", "");
        let d = build(old.as_bytes(), new.as_bytes());
        assert_eq!(d.removed, 2);
        assert_eq!(d.added, 1);
        let k = kinds(&d);
        // Context 1-2, removed 3, context 4-6, separator (7-9 → 3 skipped? no: 7..=9 are
        // within CONTEXT of both hunks so they are shown), changed 10, context 11-13, separator.
        assert_eq!(k[0], "1Co|1Co");
        assert_eq!(k[2], "3Re|-");
        assert!(k.contains(&"10C|9Ch".to_string()));
        assert_eq!(k.last().unwrap(), "~7");
        assert_eq!(d.num_width, 2);
    }

    #[test]
    fn pure_addition_and_identical_files() {
        let d = build(b"", b"a\nb\n");
        assert_eq!(kinds(&d), vec!["-|1Ad", "-|2Ad"]);
        assert_eq!((d.added, d.removed), (2, 0));
        let same = build(b"x\n", b"x\n");
        assert!(matches!(same.rows[0], Row::Message(_)));
    }

    #[test]
    fn intraline_highlights_the_changed_span() {
        let (a, b) = intraline("let x = foo(1);", "let x = bar(1);");
        assert_eq!(a, vec![8..11]);
        assert_eq!(b, vec![8..11]);
        // Mostly different: no highlight.
        let (a, b) = intraline("abcdef", "uvwxyz");
        assert!(a.is_empty() && b.is_empty());
        // Insertion in the middle: empty range on one side is dropped.
        let (a, b) = intraline("ac", "abc");
        assert!(a.is_empty());
        assert_eq!(b, vec![1..2]);
    }

    #[test]
    fn tabs_and_line_endings() {
        assert_eq!(decode(b"a\tb\r\n"), "a   b");
        assert_eq!(decode(b"\t\tx"), "        x");
        let d = build(b"a\r\nb\r\n", b"a\r\nc\r\n");
        if let Row::Line { right: Some(r), .. } = &d.rows[1] {
            assert_eq!(r.text, "c");
        } else {
            panic!("expected a changed row");
        }
    }
}
