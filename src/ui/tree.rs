//! Changed-files tree: folders with `▾`/`▸`, files with a status letter, and
//! right-aligned `+added −removed` counts (folders show their subtree total).
//! Used by the diff view sidebar and the details panel.

use super::theme;
use crate::diff::ChangeKind;
use crate::diff::changes::ChangeSet;
use crate::diff::filetree::TreeCursor;
use crate::term::{Color, Rect, Screen, Style};

pub fn kind_color(kind: ChangeKind) -> Color {
    match kind {
        ChangeKind::Added => theme::FG_ADDED,
        ChangeKind::Deleted => theme::FG_REMOVED,
        ChangeKind::Modified => theme::FG_MODIFIED,
        ChangeKind::Renamed => theme::FG_RENAMED,
    }
}

/// Draw the visible rows of `changes.tree` into `area`.
///
/// `active` highlights the cursor row as focused; `shown` is the file currently
/// displayed elsewhere (diff panel), marked with a subtle background.
pub fn draw(
    screen: &mut Screen,
    area: Rect,
    changes: &ChangeSet,
    cursor: &mut TreeCursor,
    active: bool,
    shown: Option<usize>,
) {
    if area.w < 6 || area.h == 0 {
        return;
    }
    cursor.rows = area.h as usize;
    let tree = &changes.tree;
    if !changes.loaded {
        let msg = if changes.error.is_some() {
            ""
        } else {
            "Computing…"
        };
        screen.put_str(
            area.x + 1,
            area.y,
            msg,
            theme::dim(),
            area.w.saturating_sub(1),
        );
        return;
    }
    let n = tree.visible.len();
    if n == 0 {
        screen.put_str(
            area.x + 1,
            area.y,
            "No changes",
            theme::dim(),
            area.w.saturating_sub(1),
        );
        return;
    }
    cursor.clamp(tree);
    for (row, i) in (cursor.scroll..(cursor.scroll + area.h as usize).min(n)).enumerate() {
        let y = area.y + row as u16;
        let v = tree.visible[i];
        let node = &tree.nodes[v.node];
        let selected = i == cursor.sel;
        let is_shown = node.file.is_some() && node.file == shown;
        let bg = if selected {
            if active {
                theme::BG_SELECTED
            } else {
                theme::BG_SELECTED_INACTIVE
            }
        } else if is_shown {
            theme::BG_SELECTED_INACTIVE
        } else {
            Color::Default
        };
        if bg != Color::Default {
            screen.paint_bg(area.x, y, area.w, bg);
        }

        // Stats, right-aligned.
        let stats = match (changes.node_total(v.node), node.file) {
            (Some((a, r)), _) => Some((format!("+{a}"), format!(" −{r}"))),
            (None, Some(f)) if changes.stats.get(f).copied().flatten().is_some() => None, // binary
            (None, _) if changes.stats_pending > 0 => Some(("…".to_string(), String::new())),
            _ => None,
        };
        let mut end = area.right();
        if let Some((add, rem)) = &stats {
            let w = (add.chars().count() + rem.chars().count()) as u16;
            if w + 12 < area.w {
                let mut sx = end - w - 1;
                let add_style = if add == "…" {
                    theme::dim().bg(bg)
                } else {
                    Style::new().fg(theme::FG_ADDED).bg(bg)
                };
                sx += screen.put_str(sx, y, add, add_style, w);
                screen.put_str(sx, y, rem, Style::new().fg(theme::FG_REMOVED).bg(bg), w);
                end -= w + 2;
            }
        }

        let indent = (v.depth * 2).min(area.w.saturating_sub(10));
        let mut x = area.x + 1 + indent;
        if let Some(check) = changes.checkbox(v.node) {
            x += screen.put_str(
                x,
                y,
                check,
                Style::new().fg(theme::FG_KEY).bg(bg),
                end.saturating_sub(x),
            );
        }
        if node.is_folder() {
            let arrow = if node.expanded { "▾ " } else { "▸ " };
            x += screen.put_str(
                x,
                y,
                arrow,
                Style::new().fg(theme::FG_DIM).bg(bg),
                end.saturating_sub(x),
            );
            let style = Style::new().fg(theme::FG_TEXT).bg(bg).bold();
            screen.put_str_ellipsis(x, y, &node.name, style, end.saturating_sub(x));
        } else {
            let kind = node.kind.unwrap_or(ChangeKind::Modified);
            let color = kind_color(kind);
            x += screen.put_char(x, y, kind.letter(), Style::new().fg(color).bg(bg).bold());
            x += screen.put_char(x, y, ' ', Style::new().bg(bg));
            let mut style = Style::new().fg(color).bg(bg);
            if selected {
                style = style.bold();
            }
            screen.put_str_ellipsis(x, y, &node.name, style, end.saturating_sub(x));
        }
    }
}
