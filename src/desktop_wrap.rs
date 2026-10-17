//! Cached visual rows. Byte spans always refer to the original diff line.
use crate::diff::{FileDiff, Row};
use gpui::*;
use std::ops::Range;

#[derive(Clone)]
pub struct VisualRow {
    pub source: usize,
    pub sides: [Option<Range<usize>>; 2],
}
#[derive(Default)]
pub struct Layout {
    key: Option<(u32, usize, usize, u32)>,
    pub rows: Vec<VisualRow>,
}
impl Layout {
    pub fn update(
        &mut self,
        view: u32,
        file: usize,
        diff: &FileDiff,
        width: f32,
        window: &Window,
    ) -> bool {
        let width = width.floor().max(20.) as u32;
        let key = (view, file, diff.rows.len(), width);
        if self.key == Some(key) {
            return false;
        }
        self.key = Some(key);
        self.rows.clear();
        // Most source is ASCII in a fixed-pitch code font. Measure its actual
        // advance once; do not shape every off-screen line just to wrap it.
        // Verify fixed pitch, so font fallback remains correct.
        let probe: SharedString = " !MWi09".into();
        let run = TextRun {
            len: probe.len(),
            font: font("Cascadia Mono"),
            color: rgb(0xffffff).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let shaped = window
            .text_system()
            .shape_line(probe, px(12.), &[run], None);
        let advance = f32::from(shaped.x_for_index(1));
        let fixed = advance > 0.
            && (1..shaped.len()).all(|i| {
                (f32::from(shaped.x_for_index(i + 1) - shaped.x_for_index(i)) - advance).abs()
                    < 0.02
            });
        let columns = ((width as f32 / advance).floor() as usize).max(1);
        for (source, row) in diff.rows.iter().enumerate() {
            let Row::Line { left, right } = row else {
                self.rows.push(VisualRow {
                    source,
                    sides: [None, None],
                });
                continue;
            };
            let split = |line: &Option<crate::diff::Line>| -> Vec<Range<usize>> {
                let Some(line) = line else { return vec![] };
                if fixed && line.text.bytes().all(|b| (32..127).contains(&b)) {
                    return ascii_spans(&line.text, columns);
                }
                let text: SharedString = line.text.clone().into();
                let run = TextRun {
                    len: text.len(),
                    font: font("Cascadia Mono"),
                    color: rgb(0xffffff).into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let mut starts = vec![0];
                if let Ok(lines) = window.text_system().shape_text(
                    text,
                    px(12.),
                    &[run],
                    Some(px(width as f32)),
                    None,
                ) {
                    for shaped in lines {
                        for boundary in shaped.wrap_boundaries() {
                            starts.push(
                                shaped.runs()[boundary.run_ix].glyphs[boundary.glyph_ix].index,
                            );
                        }
                    }
                }
                spans(&line.text, starts)
            };
            let l = split(left);
            let r = split(right);
            for i in 0..l.len().max(r.len()).max(1) {
                self.rows.push(VisualRow {
                    source,
                    sides: [l.get(i).cloned(), r.get(i).cloned()],
                });
            }
        }
        true
    }
    pub fn source(&self, visual: usize) -> usize {
        self.rows.get(visual).map_or(0, |r| r.source)
    }
    pub fn visual(&self, source: usize) -> usize {
        self.rows.partition_point(|r| r.source < source)
    }
}
fn ascii_spans(text: &str, columns: usize) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let limit = (start + columns).min(text.len());
        let end = if limit < text.len() {
            text.as_bytes()[start..limit]
                .iter()
                .rposition(|&b| b == b' ')
                .map_or(limit, |i| start + i + 1)
        } else {
            limit
        };
        ranges.push(start..end);
        start = end;
    }
    if ranges.is_empty() {
        ranges.push(0..0);
    }
    ranges
}
fn spans(text: &str, mut starts: Vec<usize>) -> Vec<Range<usize>> {
    starts.retain(|&i| i <= text.len() && text.is_char_boundary(i));
    starts.sort_unstable();
    starts.dedup();
    if starts.last() != Some(&text.len()) {
        starts.push(text.len());
    }
    if starts.len() == 1 {
        return std::iter::once(0..0).collect();
    }
    starts.windows(2).map(|p| p[0]..p[1]).collect()
}
#[cfg(test)]
mod tests {
    #[test]
    fn code_wrap_preserves_whitespace_and_splits_long_tokens() {
        for text in ["  a long token  ", "abcdefghijklmnop", "", "         "] {
            for columns in 1..10 {
                let spans = super::ascii_spans(text, columns);
                assert_eq!(
                    spans.iter().map(|r| &text[r.clone()]).collect::<String>(),
                    text
                );
                assert!(spans.iter().all(|r| r.len() <= columns));
            }
        }
        assert_eq!(super::ascii_spans("hello world", 7), vec![0..6, 6..11]);
    }
    #[test]
    fn wrapped_spans_preserve_all_source_bytes_including_spaces_and_unicode() {
        let text = "ab  café 世界";
        let spans = super::spans(text, vec![0, 4, 10]);
        assert_eq!(
            spans.iter().map(|r| &text[r.clone()]).collect::<String>(),
            text
        );
        assert_eq!(super::spans("", vec![0]), vec![0..0]);
    }
}
