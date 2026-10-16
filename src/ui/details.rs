//! Right-hand panel: metadata and full message of the selected commit.

use unicode_width::UnicodeWidthChar;

use super::{label_style, theme, time};
use crate::app::{App, Focus};
use crate::term::{Rect, Screen, Style};

type Line = Vec<(String, Style)>;

pub fn draw(app: &mut App, screen: &mut Screen, area: Rect, now: i64) {
    if app.search.active() && app.graph_len() == 0 {
        app.files_area = Rect::default();
        return;
    }
    if app.pending.selected {
        super::pending::draw(app, screen, area);
        return;
    }
    app.files_area = Rect::default();
    if area.w < 4 || area.h == 0 {
        return;
    }
    // Bottom section: files changed by this commit (when known and non-empty).
    let files_h = match app.details_changes.as_ref() {
        Some(cs) if !cs.loaded && cs.error.is_none() => 2,
        Some(cs) if cs.loaded && !cs.files.is_empty() => {
            let want = cs.tree.visible.len() as u16 + 1;
            want.min((area.h * 45 / 100).max(4))
        }
        _ => 0,
    };
    let files_h = if area.h < 8 {
        0
    } else {
        files_h.min(area.h - 4)
    };
    let area = if files_h > 0 {
        let files = Rect::new(area.x, area.bottom() - files_h, area.w, files_h);
        draw_files(app, screen, files);
        Rect::new(area.x, area.y, area.w, area.h - files_h)
    } else {
        area
    };

    let inner = Rect::new(area.x + 1, area.y, area.w.saturating_sub(2), area.h);
    let lines = build_lines(app, inner.w as usize, now);
    app.details_lines = lines.len();
    let max_scroll = lines.len().saturating_sub(inner.h as usize);
    if app.details_scroll > max_scroll {
        app.details_scroll = max_scroll;
    }
    for (row, line) in lines
        .iter()
        .skip(app.details_scroll)
        .take(inner.h as usize)
        .enumerate()
    {
        let y = inner.y + row as u16;
        let mut x = inner.x;
        for (text, style) in line {
            if x >= inner.right() {
                break;
            }
            x += screen.put_str(x, y, text, *style, inner.right() - x);
        }
    }
    // Scroll indicator when there is more content.
    if lines.len() > inner.h as usize {
        let track_h = inner.h.max(1) as usize;
        let thumb_pos = (app.details_scroll * track_h.saturating_sub(1)) / max_scroll.max(1);
        let color = if app.focus == Focus::Details {
            theme::FG_SPLITTER_ACTIVE
        } else {
            theme::FG_SPLITTER
        };
        for r in 0..track_h {
            let ch = if r == thumb_pos { '█' } else { '░' };
            screen.put_char(
                area.right() - 1,
                inner.y + r as u16,
                ch,
                Style::new().fg(color),
            );
        }
    }
}

/// Header row ("Files N  +a −r") followed by the changed-files tree.
fn draw_files(app: &mut App, screen: &mut Screen, area: Rect) {
    let Some(cs) = app.details_changes.as_ref() else {
        return;
    };
    let bg = theme::BG_SELECTED_INACTIVE;
    screen.fill(
        Rect::new(area.x, area.y, area.w, 1),
        ' ',
        Style::new().bg(bg),
    );
    let mut x = area.x + 1;
    x += screen.put_str(
        x,
        area.y,
        "Files",
        Style::new().fg(theme::FG_TEXT).bg(bg).bold(),
        area.w,
    );
    if cs.loaded {
        let n = cs.files.len();
        x += screen.put_str(
            x,
            area.y,
            &format!(" {n}"),
            Style::new().fg(theme::FG_DIM).bg(bg),
            area.w,
        );
        let (a, r) = cs.total();
        if a + r > 0 {
            let add = format!("+{a}");
            let rem = format!(" −{r}");
            let pending = if cs.stats_pending > 0 { "…" } else { "" };
            let w = (add.len() + rem.chars().count() + pending.len()) as u16;
            if x + w + 2 < area.right() {
                let mut sx = area.right() - w - 1;
                sx += screen.put_str(sx, area.y, &add, Style::new().fg(theme::FG_ADDED).bg(bg), w);
                sx += screen.put_str(
                    sx,
                    area.y,
                    &rem,
                    Style::new().fg(theme::FG_REMOVED).bg(bg),
                    w,
                );
                screen.put_str(
                    sx,
                    area.y,
                    pending,
                    Style::new().fg(theme::FG_DIM).bg(bg),
                    w,
                );
            }
        }
    }
    let tree_area = Rect::new(area.x, area.y + 1, area.w.saturating_sub(1), area.h - 1);
    app.files_area = tree_area;
    let active = app.focus == Focus::Files;
    let App {
        details_changes,
        details_cursor,
        ..
    } = app;
    if let Some(cs) = details_changes.as_ref() {
        super::tree::draw(screen, tree_area, cs, details_cursor, active, None);
    }
}

fn build_lines(app: &App, width: usize, now: i64) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    let Some(id) = app.selected_id() else {
        lines.push(vec![("No commit selected".into(), theme::dim())]);
        return lines;
    };
    if app.ghosts.contains_key(&id) {
        for line in wrap(
            "Remote commit. Files and diffs load on demand and are cached.",
            width,
        ) {
            lines.push(vec![(line, theme::dim())]);
        }
        lines.push(Vec::new());
    }
    let label_style_key = Style::new().fg(theme::FG_DIM);
    let value = theme::text();

    let subject = app.history.subject(app.selected).to_string();
    for l in wrap(&subject, width) {
        lines.push(vec![(l, theme::text().bold())]);
    }
    lines.push(vec![(
        "─".repeat(width),
        Style::new().fg(theme::FG_SPLITTER),
    )]);

    let key = |s: &str| -> (String, Style) { (format!("{s:<9} "), label_style_key) };

    match &app.details {
        Some(d) if d.id == id => {
            lines.push(vec![
                key("Author"),
                (d.author_name.clone(), Style::new().fg(theme::FG_AUTHOR)),
                (format!(" <{}>", d.author_email), theme::dim()),
            ]);
            lines.push(vec![
                key("Date"),
                (time::format_absolute(d.author_time, d.author_offset), value),
                (
                    format!("  ({})", time::format_relative_long(d.author_time, now)),
                    theme::dim(),
                ),
            ]);
            if d.committer_name != d.author_name || d.committer_email != d.author_email {
                lines.push(vec![
                    key("Committer"),
                    (d.committer_name.clone(), Style::new().fg(theme::FG_AUTHOR)),
                    (format!(" <{}>", d.committer_email), theme::dim()),
                ]);
            }
            if d.commit_time != d.author_time {
                lines.push(vec![
                    key("Committed"),
                    (time::format_absolute(d.commit_time, d.commit_offset), value),
                ]);
            }
            if !d.parents.is_empty() {
                let mut line = vec![key("Parents")];
                for (i, p) in d.parents.iter().enumerate() {
                    if i > 0 {
                        line.push((" ".into(), value));
                    }
                    line.push((
                        p.to_hex_with_len(7).to_string(),
                        Style::new().fg(theme::FG_HASH),
                    ));
                }
                lines.push(line);
            }
        }
        _ => {
            // Details not loaded yet: show what the list already knows.
            lines.push(vec![
                key("Author"),
                (
                    app.history.author(app.selected).to_string(),
                    Style::new().fg(theme::FG_AUTHOR),
                ),
            ]);
            let rec = app.history.commits[app.selected];
            lines.push(vec![
                key("Date"),
                (
                    time::format_absolute(rec.author_time, rec.author_offset),
                    value,
                ),
            ]);
            let parents = app.history.parents(app.selected);
            if !parents.is_empty() {
                let mut line = vec![key("Parents")];
                for (i, p) in parents.iter().enumerate() {
                    if i > 0 {
                        line.push((" ".into(), value));
                    }
                    line.push((
                        p.to_hex_with_len(7).to_string(),
                        Style::new().fg(theme::FG_HASH),
                    ));
                }
                lines.push(line);
            }
        }
    }

    if let Some(labels) = app.labels.get(&id) {
        let mut line = vec![key("Refs")];
        for (i, l) in labels.iter().enumerate() {
            if i > 0 {
                line.push((", ".into(), theme::dim()));
            }
            line.push((
                l.name.clone(),
                label_style(l.kind, crate::term::Color::Default),
            ));
        }
        lines.push(line);
    }
    lines.push(vec![
        key("SHA"),
        (id.to_string(), Style::new().fg(theme::FG_HASH)),
    ]);
    lines.push(Vec::new());

    if let Some(d) = app.details.as_ref().filter(|d| d.id == id) {
        let body = d.message.trim_end();
        let mut paragraphs = body.split('\n');
        // Skip the subject line (already shown) and the blank after it.
        let _ = paragraphs.next();
        let rest: Vec<&str> = paragraphs.collect();
        let rest = rest
            .iter()
            .skip_while(|l| l.trim().is_empty())
            .copied()
            .collect::<Vec<_>>();
        if rest.is_empty() {
            lines.push(vec![("(no description)".into(), theme::dim())]);
        }
        for para in reflow(&rest) {
            if para.is_empty() {
                lines.push(Vec::new());
                continue;
            }
            for l in wrap(&para, width) {
                lines.push(vec![(l, value)]);
            }
        }
    } else {
        lines.push(vec![("Loading…".into(), theme::dim())]);
    }
    lines
}

/// Join hard-wrapped prose lines back into paragraphs so they can be re-wrapped
/// to the panel width. Lines that look structured (indented, bullets, quotes,
/// trailers like `Signed-off-by:`) are kept as they are. Empty strings mark
/// blank lines.
fn reflow(raw: &[&str]) -> Vec<String> {
    fn is_structured(l: &str) -> bool {
        let t = l.trim_start();
        let numbered = t.chars().next().is_some_and(|c| c.is_ascii_digit())
            && t.find(['.', ')']).is_some_and(|i| i <= 3);
        l.starts_with([' ', '\t'])
            || t.starts_with(['-', '*', '+', '>', '|', '#', '`'])
            || numbered
            || is_trailer(t)
    }
    fn is_trailer(l: &str) -> bool {
        // `Word-Word: value`
        l.split_once(':').is_some_and(|(k, _)| {
            !k.is_empty()
                && k.len() <= 40
                && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
    }
    let mut out: Vec<String> = Vec::new();
    let mut prev_joinable = false;
    for raw in raw {
        let line = raw.replace('\t', "    ");
        if line.trim().is_empty() {
            out.push(String::new());
            prev_joinable = false;
            continue;
        }
        let structured = is_structured(&line);
        if prev_joinable && !structured {
            if let Some(last) = out.last_mut() {
                last.push(' ');
                last.push_str(line.trim());
            }
        } else {
            out.push(line.trim_end().to_string());
        }
        prev_joinable = !structured && !line.trim_end().ends_with(':');
    }
    out
}

/// Greedy word wrap on display width, preserving leading indentation.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    let indent: String = text.chars().take_while(|c| *c == ' ').collect();
    let mut line = indent.clone();
    let mut line_w = indent.len();
    for word in text.trim_start().split(' ') {
        let ww: usize = word
            .chars()
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(1))
            .sum();
        if line_w > indent.len() && line_w + 1 + ww > width {
            out.push(std::mem::take(&mut line));
            line = indent.clone();
            line_w = indent.len();
        }
        if ww > width {
            // Hard-break overlong tokens.
            for ch in word.chars() {
                let cw = UnicodeWidthChar::width(ch).unwrap_or(1);
                if line_w + cw > width {
                    out.push(std::mem::take(&mut line));
                    line_w = 0;
                }
                line.push(ch);
                line_w += cw;
            }
            continue;
        }
        if line_w > indent.len() {
            line.push(' ');
            line_w += 1;
        }
        line.push_str(word);
        line_w += ww;
    }
    out.push(line);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflow_joins_prose_but_keeps_structure() {
        let raw = [
            "The 'from' command has been",
            "updated to resolve references.",
            "",
            "* topic/branch:",
            "  fast-import: allow 'from'",
            "- bullet one",
            "- bullet two",
            "",
            "Signed-off-by: Someone <s@example.com>",
            "Reviewed-by: Other <o@example.com>",
        ];
        let out = reflow(&raw);
        assert_eq!(
            out,
            vec![
                "The 'from' command has been updated to resolve references.",
                "",
                "* topic/branch:",
                "  fast-import: allow 'from'",
                "- bullet one",
                "- bullet two",
                "",
                "Signed-off-by: Someone <s@example.com>",
                "Reviewed-by: Other <o@example.com>",
            ]
        );
    }

    #[test]
    fn wrap_respects_width_and_indent() {
        let w = wrap("  alpha beta gamma delta", 12);
        assert_eq!(w, vec!["  alpha beta", "  gamma", "  delta"]);
        let long = wrap("abcdefghijklmnop", 5);
        assert_eq!(long, vec!["abcde", "fghij", "klmno", "p"]);
    }
}
