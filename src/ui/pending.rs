//! Commit composer and selectable working-tree file list.
use super::theme;
use crate::app::{App, Focus};
use crate::term::{Color, Rect, Screen, Style};

pub fn draw(app: &mut App, screen: &mut Screen, area: Rect) {
    app.pending.areas = Default::default();
    app.files_area = Rect::default();
    if area.w < 8 || area.h < 3 {
        return;
    }
    let x = area.x + 1;
    let w = area.w.saturating_sub(2);
    screen.put_str_ellipsis(x, area.y, "Commit changes", theme::text().bold(), w);
    let files_h = (area.h * 45 / 100).max(4).min(area.h.saturating_sub(3));
    let bottom = area.bottom() - files_h;
    if bottom.saturating_sub(area.y) >= 12 {
        screen.put_str(x, area.y + 2, "Description", theme::dim(), w);
        let description = Rect::new(x, area.y + 3, w, 3);
        app.pending.areas.description = description;
        field(
            screen,
            description,
            &app.pending.description,
            app.focus == Focus::Details && app.pending.field == 0,
        );
        screen.put_str_ellipsis(x, area.y + 6, "New branch (optional)", theme::dim(), w);
        let branch = Rect::new(x, area.y + 7, w, 1);
        app.pending.areas.branch = branch;
        field(
            screen,
            branch,
            &app.pending.branch,
            app.focus == Focus::Details && app.pending.field == 1,
        );
        let commit = Rect::new(x, area.y + 9, w, 1);
        app.pending.areas.commit = commit;
        button(
            screen,
            commit,
            if app.pending.busy {
                "[ Committing… ]"
            } else {
                "[ Commit (Ctrl+Enter) ]"
            },
            app.focus == Focus::Details && app.pending.field == 2,
        );
        screen.put_str_ellipsis(
            x,
            area.y + 10,
            "Checked files use current contents",
            theme::dim(),
            w,
        );
        let message = if app.pending.snapshot.conflicts {
            "Resolve conflicts before committing"
        } else {
            &app.pending.status
        };
        screen.put_str_ellipsis(
            x,
            area.y + 11,
            message,
            Style::new().fg(if app.pending.error || app.pending.snapshot.conflicts {
                theme::FG_ERR
            } else {
                theme::FG_DIM
            }),
            w,
        );
    } else {
        screen.put_str_ellipsis(
            x,
            area.y + 1,
            "Enlarge panel to edit commit fields",
            theme::dim(),
            w,
        );
    }
    let Some(changes) = app.details_changes.as_ref() else {
        return;
    };
    screen.fill(
        Rect::new(area.x, bottom, area.w, 1),
        ' ',
        Style::new().bg(theme::BG_SELECTED_INACTIVE),
    );
    let title = format!(
        "Modified files ({}/{})",
        changes.checked_count(),
        changes.files.len()
    );
    screen.put_str_ellipsis(x, bottom, &title, theme::text().bold(), w);
    if files_h >= 3 {
        let check = Rect::new(x, bottom + 1, 13.min(w), 1);
        let uncheck = Rect::new(x + check.w, bottom + 1, w.saturating_sub(check.w), 1);
        app.pending.areas.check_all = check;
        app.pending.areas.uncheck_all = uncheck;
        button(
            screen,
            check,
            "[ Check all ]",
            app.focus == Focus::Details && app.pending.field == 3,
        );
        button(
            screen,
            uncheck,
            "[ Uncheck all ]",
            app.focus == Focus::Details && app.pending.field == 4,
        );
        let tree_area = Rect::new(area.x, bottom + 2, area.w.saturating_sub(1), files_h - 2);
        app.files_area = tree_area;
        super::tree::draw(
            screen,
            tree_area,
            changes,
            &mut app.details_cursor,
            app.focus == Focus::Files,
            None,
        );
    }
}
fn button(screen: &mut Screen, area: Rect, text: &str, focused: bool) {
    let style = Style::new()
        .fg(theme::FG_KEY)
        .bg(if focused {
            theme::BG_SELECTED
        } else {
            Color::Default
        })
        .bold();
    screen.fill(area, ' ', style);
    screen.put_str_ellipsis(area.x, area.y, text, style, area.w);
}
fn field(screen: &mut Screen, area: Rect, text: &crate::pending::Text, focused: bool) {
    use unicode_width::UnicodeWidthChar;
    let bg = if focused {
        theme::BG_SELECTED
    } else {
        theme::BG_SELECTED_INACTIVE
    };
    screen.fill(area, ' ', theme::text().bg(bg));
    let mut positions = Vec::new();
    let mut row: u16 = 0;
    let mut col = 0;
    for (offset, ch) in text
        .value
        .char_indices()
        .chain(std::iter::once((text.value.len(), '\0')))
    {
        if col >= area.w {
            row = row.saturating_add(1);
            col = 0;
        }
        positions.push((offset, row, col, ch));
        if ch == '\n' {
            row = row.saturating_add(1);
            col = 0;
        } else {
            col += ch.width().unwrap_or(1) as u16;
        }
    }
    let cursor = positions
        .iter()
        .find(|p| p.0 == text.cursor)
        .copied()
        .unwrap_or((0, 0, 0, '\0'));
    let first = cursor.1.saturating_sub(area.h - 1);
    for (_, row, col, ch) in positions {
        if row >= first && row < first.saturating_add(area.h) && ch != '\n' && ch != '\0' {
            screen.put_char(area.x + col, area.y + row - first, ch, theme::text().bg(bg));
        }
    }
    if focused {
        let ch = if cursor.3 == '\0' || cursor.3 == '\n' {
            '▏'
        } else {
            cursor.3
        };
        screen.put_char(
            area.x + cursor.2.min(area.w - 1),
            area.y + cursor.1 - first,
            ch,
            Style::new().fg(theme::FG_KEY).bg(theme::BG_MARK),
        );
    }
}
