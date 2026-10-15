//! Full-screen diff view: file tree sidebar on the left, side-by-side diff of
//! the selected file on the right.

use std::ops::Range;

use super::theme;
use super::tree::kind_color;
use crate::diff::{LineKind, Row};
use crate::diffview::{DiffFocus, DiffView};
use crate::term::{Color, Rect, Screen, Style};

#[derive(Debug, Clone, Copy, Default)]
pub struct DiffAreas {
    pub header: Rect,
    pub sidebar: Rect,
    pub splitter: Rect,
    pub main: Rect,
    pub status: Rect,
}

pub fn compute_areas(w: u16, h: u16, sidebar_width: u16) -> DiffAreas {
    let header = Rect::new(0, 0, w, 1);
    let status = Rect::new(0, h.saturating_sub(1), w, 1);
    let body_y = 1;
    let body_h = h.saturating_sub(2);
    if w < 60 {
        return DiffAreas {
            header,
            sidebar: Rect::new(0, body_y, 0, body_h),
            splitter: Rect::new(0, body_y, 0, body_h),
            main: Rect::new(0, body_y, w, body_h),
            status,
        };
    }
    let side = sidebar_width.clamp(16, w - 40);
    DiffAreas {
        header,
        sidebar: Rect::new(0, body_y, side, body_h),
        splitter: Rect::new(side, body_y, 1, body_h),
        main: Rect::new(side + 1, body_y, w - side - 1, body_h),
        status,
    }
}

pub fn default_sidebar_width(w: u16) -> u16 {
    ((w as u32 * 28 / 100) as u16).clamp(24, 48)
}

pub fn draw(repo_name: &str, view: &mut DiffView, screen: &mut Screen, areas: &DiffAreas) {
    screen.clear();
    draw_header(repo_name, view, screen, areas.header);
    super::tree::draw(
        screen,
        areas.sidebar,
        &view.changes,
        &mut view.cursor,
        view.focus == DiffFocus::Files,
        view.current,
    );
    if areas.splitter.w > 0 {
        let color = if view.dragging_splitter {
            theme::FG_SPLITTER_ACTIVE
        } else {
            theme::FG_SPLITTER
        };
        screen.fill(areas.splitter, '│', Style::new().fg(color));
    }
    draw_main(view, screen, areas.main);
    draw_status(view, screen, areas.status);
}

fn draw_header(repo_name: &str, view: &DiffView, screen: &mut Screen, area: Rect) {
    let bg = theme::BG_HEADER;
    screen.fill(area, ' ', Style::new().bg(bg));
    let mut x = area.x + 1;
    x += screen.put_str(x, area.y, "git-rustle", Style::new().fg(theme::FG_KEY).bg(bg).bold(), area.w);
    x += screen.put_str(x, area.y, "  ", Style::new().bg(bg), area.w);
    x += screen.put_str_ellipsis(
        x,
        area.y,
        repo_name,
        Style::new().fg(theme::FG_TEXT).bg(bg).bold(),
        area.right().saturating_sub(x),
    );
    x += screen.put_str(x, area.y, "  diff  ", Style::new().fg(theme::FG_DIM).bg(bg), area.w);
    let hash = Style::new().fg(theme::FG_HASH).bg(bg);
    let dim = Style::new().fg(theme::FG_DIM).bg(bg);
    x += match view.changes.old {
        Some(old) => screen.put_str(x, area.y, &old.to_hex_with_len(7).to_string(), hash, area.w),
        None => screen.put_str(x, area.y, "(root)", dim, area.w),
    };
    x += screen.put_str(x, area.y, " → ", dim, area.w);
    screen.put_str(x, area.y, &view.changes.new.to_hex_with_len(7).to_string(), hash, area.w);

    let cs = &view.changes;
    let state = if let Some(e) = &cs.error {
        format!("error: {e}")
    } else if !cs.loaded {
        "computing…".to_string()
    } else {
        let n = cs.files.len();
        let (a, r) = cs.total();
        let mut s = format!("{n} file{}", if n == 1 { "" } else { "s" });
        if a + r > 0 {
            s.push_str(&format!("  +{a} −{r}"));
            if cs.stats_pending > 0 {
                s.push('…');
            }
        }
        s
    };
    let sw = crate::term::screen::str_width(&state) as u16;
    if sw + 1 < area.w {
        let fg = if cs.error.is_some() { theme::FG_ERR } else { theme::FG_DIM };
        screen.put_str(area.right() - sw - 1, area.y, &state, Style::new().fg(fg).bg(bg), sw);
    }
}

fn draw_main(view: &mut DiffView, screen: &mut Screen, area: Rect) {
    if area.w < 8 || area.h < 2 {
        return;
    }
    // Title row: path and stats.
    let title_bg = theme::BG_SELECTED_INACTIVE;
    let title = Rect::new(area.x, area.y, area.w, 1);
    screen.fill(title, ' ', Style::new().bg(title_bg));
    if let Some(change) = view.current_change() {
        let mut x = area.x + 1;
        let color = kind_color(change.kind);
        x += screen.put_char(x, area.y, change.kind.letter(), Style::new().fg(color).bg(title_bg).bold());
        x += screen.put_str(x, area.y, " ", Style::new().bg(title_bg), 1);
        let stats = view
            .diff
            .as_ref()
            .filter(|d| d.added + d.removed > 0)
            .map(|d| (format!("+{}", d.added), format!(" −{}", d.removed)));
        let stats_w = stats
            .as_ref()
            .map(|(a, r)| (a.chars().count() + r.chars().count()) as u16 + 2)
            .unwrap_or(0);
        let text_end = area.right().saturating_sub(stats_w);
        let dim = Style::new().fg(theme::FG_DIM).bg(title_bg);
        if let Some(old) = &change.old_path {
            x += screen.put_str_ellipsis(x, area.y, old, dim, text_end.saturating_sub(x));
            x += screen.put_str(x, area.y, " → ", dim, text_end.saturating_sub(x));
        }
        let name = Style::new().fg(theme::FG_TEXT).bg(title_bg).bold();
        screen.put_str_ellipsis(x, area.y, &change.path, name, text_end.saturating_sub(x));
        if let Some((add, rem)) = stats {
            let mut sx = area.right() - stats_w + 1;
            sx += screen.put_str(sx, area.y, &add, Style::new().fg(theme::FG_ADDED).bg(title_bg), stats_w);
            screen.put_str(sx, area.y, &rem, Style::new().fg(theme::FG_REMOVED).bg(title_bg), stats_w);
        }
    } else if view.changes.loaded && view.changes.files.is_empty() {
        screen.put_str(area.x + 1, area.y, "No differences between these commits", theme::dim().bg(title_bg), area.w);
    }

    let body = Rect::new(area.x, area.y + 1, area.w, area.h - 1);
    view.rows = body.h as usize;
    let Some(diff) = view.diff.as_ref() else {
        if view.current.is_some() {
            screen.put_str(body.x + 1, body.y + 1, "Loading…", theme::dim(), body.w);
        }
        return;
    };
    let total = diff.rows.len();
    let max_scroll = total.saturating_sub(body.h as usize);
    if view.scroll > max_scroll {
        view.scroll = max_scroll;
    }
    let scrollbar = total > body.h as usize;
    let text_w = body.w - u16::from(scrollbar);
    let half = text_w.saturating_sub(1) / 2;
    let left = Rect::new(body.x, body.y, half, body.h);
    let gutter_x = body.x + half;
    let right = Rect::new(gutter_x + 1, body.y, text_w - half - 1, body.h);
    let num_w = diff.num_width as u16;
    view.half_cols = half.saturating_sub(num_w + 2) as usize;
    let hscroll = view.hscroll;

    for (row, i) in (view.scroll..(view.scroll + body.h as usize).min(total)).enumerate() {
        let y = body.y + row as u16;
        match &diff.rows[i] {
            Row::Line { left: l, right: r } => {
                draw_half(screen, left, y, l.as_ref(), num_w, hscroll, true);
                screen.put_char(gutter_x, y, '│', Style::new().fg(theme::FG_SPLITTER));
                draw_half(screen, right, y, r.as_ref(), num_w, hscroll, false);
            }
            Row::Separator { skipped } => {
                let style = Style::new().fg(theme::FG_DIM).bg(theme::BG_SEPARATOR);
                screen.fill(Rect::new(body.x, y, text_w, 1), ' ', style);
                let msg = format!("⋯ {skipped} unchanged line{}", if *skipped == 1 { "" } else { "s" });
                screen.put_str(body.x + num_w + 2, y, &msg, style, text_w.saturating_sub(num_w + 2));
                screen.put_str(right.x + num_w + 2, y, &msg, style, right.w.saturating_sub(num_w + 2));
            }
            Row::Message(m) => {
                screen.put_str_ellipsis(body.x + 2, y, m, theme::dim(), text_w.saturating_sub(2));
            }
        }
    }
    if scrollbar {
        let x = body.right() - 1;
        let track = Style::new().fg(theme::FG_SPLITTER);
        for y in body.y..body.bottom() {
            screen.put_char(x, y, '│', track);
        }
        let h = body.h as usize;
        let thumb_h = ((h * h) / total).clamp(1, h);
        let thumb_y = (view.scroll * (h - thumb_h)).checked_div(max_scroll).unwrap_or(0);
        for y in 0..thumb_h {
            screen.put_char(x, body.y + (thumb_y + y) as u16, '█', Style::new().fg(theme::FG_DIM));
        }
    }
}

fn draw_half(
    screen: &mut Screen,
    area: Rect,
    y: u16,
    line: Option<&crate::diff::Line>,
    num_w: u16,
    hscroll: usize,
    is_old: bool,
) {
    if area.w == 0 {
        return;
    }
    let Some(line) = line else {
        screen.fill(Rect::new(area.x, y, area.w, 1), ' ', Style::new().bg(theme::BG_FILLER));
        return;
    };
    let (bg, hl_bg) = match line.kind {
        LineKind::Context => (Color::Default, Color::Default),
        LineKind::Removed => (theme::BG_REMOVED, theme::BG_REMOVED),
        LineKind::Added => (theme::BG_ADDED, theme::BG_ADDED),
        LineKind::Changed if is_old => (theme::BG_REMOVED, theme::BG_REMOVED_HL),
        LineKind::Changed => (theme::BG_ADDED, theme::BG_ADDED_HL),
    };
    screen.fill(Rect::new(area.x, y, area.w, 1), ' ', Style::new().bg(bg));
    let no = format!("{:>w$} ", line.no, w = num_w as usize);
    let mut x = area.x;
    x += screen.put_str(x, y, &no, Style::new().fg(theme::FG_DIM).bg(bg), area.w);
    let text_w = area.right().saturating_sub(x);
    let base = Style::new().fg(theme::FG_TEXT).bg(bg);
    let hl = Style::new().fg(theme::FG_TEXT).bg(hl_bg).bold();
    put_text(screen, x, y, &line.text, &line.hl, base, hl, hscroll, text_w);
}

/// Draw `text` starting `skip` display columns in, switching to `hl_style`
/// inside the given character ranges.
#[allow(clippy::too_many_arguments)]
fn put_text(
    screen: &mut Screen,
    x: u16,
    y: u16,
    text: &str,
    ranges: &[Range<usize>],
    base: Style,
    hl_style: Style,
    skip: usize,
    max_w: u16,
) {
    use unicode_width::UnicodeWidthChar;
    let mut col = 0usize;
    let mut used = 0u16;
    let mut ri = 0usize;
    for (ci, ch) in text.chars().enumerate() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(1).clamp(1, 2);
        if col + cw <= skip {
            col += cw;
            continue;
        }
        if col < skip {
            // Wide glyph straddling the scroll boundary.
            col += cw;
            continue;
        }
        if used + cw as u16 > max_w {
            break;
        }
        while ri < ranges.len() && ranges[ri].end <= ci {
            ri += 1;
        }
        let style = if ri < ranges.len() && ranges[ri].contains(&ci) { hl_style } else { base };
        used += screen.put_char(x + used, y, ch, style);
        col += cw;
    }
}

fn draw_status(view: &DiffView, screen: &mut Screen, area: Rect) {
    let bg = theme::BG_HEADER;
    screen.fill(area, ' ', Style::new().bg(bg));
    let key = Style::new().fg(theme::FG_KEY).bg(bg).bold();
    let txt = Style::new().fg(theme::FG_DIM).bg(bg);
    let mut x = area.x + 1;
    let hints: &[(&str, &str)] = match view.focus {
        DiffFocus::Files => &[
            ("↑↓", "move"),
            ("←→", "fold"),
            ("Enter", "open"),
            ("n/p", "file"),
            ("e/c", "expand/collapse"),
            ("Tab", "diff"),
            ("[ ]", "panel"),
            ("Esc", "back"),
        ],
        DiffFocus::Diff => &[
            ("↑↓", "scroll"),
            ("←→", "pan"),
            ("PgUp/PgDn", "page"),
            ("n/p", "file"),
            ("Tab", "files"),
            ("[ ]", "panel"),
            ("Esc", "back"),
        ],
    };
    for (k, label) in hints {
        let need = (crate::term::screen::str_width(k) + label.len() + 3) as u16;
        if x + need >= area.right() {
            break;
        }
        x += screen.put_str(x, area.y, k, key, area.w);
        x += screen.put_str(x, area.y, " ", txt, area.w);
        x += screen.put_str(x, area.y, label, txt, area.w);
        x += screen.put_str(x, area.y, "  ", txt, area.w);
    }
    let mut right = String::new();
    if let Some(cur) = view.current
        && !view.changes.files.is_empty()
    {
        right.push_str(&format!("file {}/{}", cur + 1, view.changes.files.len()));
    }
    if let Some(d) = view.diff.as_ref()
        && !d.rows.is_empty()
    {
        let top = view.scroll + 1;
        let bottom = (view.scroll + view.rows).min(d.rows.len());
        right.push_str(&format!("  {top}-{bottom}/{}", d.rows.len()));
    }
    let w = crate::term::screen::str_width(&right) as u16;
    if w > 0 && x + w < area.right() {
        screen.put_str(area.right() - w - 1, area.y, &right, txt, w);
    }
}
