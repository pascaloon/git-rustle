//! Drawing: header, commit list with graph, details panel, status bar.

pub mod branches;
pub mod details;
pub mod diff;
pub mod list;
pub mod settings;
pub mod time;
pub mod tree;

use crate::app::{App, Focus, LoadState, Mode};
use crate::term::{Color, Rect, Screen, Style};

/// Screen regions for one frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct Areas {
    pub header: Rect,
    pub list: Rect,
    pub splitter: Rect,
    pub details: Rect,
    pub status: Rect,
}

pub mod theme {
    use crate::term::{Color, Style};

    pub const BG_HEADER: Color = Color::rgb(0x1f, 0x24, 0x30);
    pub const BG_SELECTED: Color = Color::rgb(0x2e, 0x3a, 0x50);
    pub const BG_SELECTED_INACTIVE: Color = Color::rgb(0x27, 0x2c, 0x38);
    pub const FG_DIM: Color = Color::rgb(0x7a, 0x82, 0x94);
    pub const FG_TEXT: Color = Color::rgb(0xe6, 0xe9, 0xef);
    pub const FG_HASH: Color = Color::rgb(0xe2, 0xb8, 0x5a);
    pub const FG_AUTHOR: Color = Color::rgb(0x7d, 0xcf, 0xff);
    pub const FG_DATE: Color = Color::rgb(0x9a, 0xa4, 0xb8);
    pub const FG_HEAD: Color = Color::rgb(0x8c, 0xf5, 0x8c);
    pub const FG_BRANCH: Color = Color::rgb(0x5e, 0xe6, 0xa0);
    pub const FG_REMOTE: Color = Color::rgb(0xff, 0x8a, 0x80);
    pub const FG_TAG: Color = Color::rgb(0xff, 0xe0, 0x6b);
    pub const FG_OTHER: Color = Color::rgb(0xc7, 0xa8, 0xff);
    pub const FG_SPLITTER: Color = Color::rgb(0x3a, 0x42, 0x55);
    pub const FG_SPLITTER_ACTIVE: Color = Color::rgb(0x7d, 0xcf, 0xff);
    pub const FG_KEY: Color = Color::rgb(0xff, 0xb8, 0x6c);
    pub const FG_ERR: Color = Color::rgb(0xff, 0x6b, 0x6b);
    /// Chip background for commits marked for diffing.
    pub const BG_MARK: Color = Color::rgb(0xff, 0x79, 0xc6);

    // Diff view.
    pub const FG_ADDED: Color = Color::rgb(0x8c, 0xf5, 0x8c);
    pub const FG_REMOVED: Color = Color::rgb(0xff, 0x8a, 0x80);
    pub const FG_MODIFIED: Color = Color::rgb(0xe2, 0xb8, 0x5a);
    pub const FG_RENAMED: Color = Color::rgb(0xc7, 0xa8, 0xff);
    pub const BG_ADDED: Color = Color::rgb(0x1a, 0x36, 0x24);
    pub const BG_REMOVED: Color = Color::rgb(0x3d, 0x1e, 0x24);
    pub const BG_ADDED_HL: Color = Color::rgb(0x2c, 0x6e, 0x3e);
    pub const BG_REMOVED_HL: Color = Color::rgb(0x80, 0x2e, 0x38);
    pub const BG_FILLER: Color = Color::rgb(0x1a, 0x1c, 0x22);
    pub const BG_SEPARATOR: Color = Color::rgb(0x22, 0x27, 0x33);

    pub const fn text() -> Style {
        Style::new().fg(FG_TEXT)
    }
    pub const fn dim() -> Style {
        Style::new().fg(FG_DIM)
    }
}

/// Decide the panel geometry for the given screen size and requested details width.
pub fn compute_areas(w: u16, h: u16, right_width: u16) -> Areas {
    let header = Rect::new(0, 0, w, 1);
    let status = Rect::new(0, h.saturating_sub(1), w, 1);
    let body_y = 1;
    let body_h = h.saturating_sub(2);
    if w < 40 {
        // Too narrow for two panels: list only.
        return Areas {
            header,
            list: Rect::new(0, body_y, w, body_h),
            splitter: Rect::new(w, body_y, 0, body_h),
            details: Rect::new(w, body_y, 0, body_h),
            status,
        };
    }
    let max_right = w.saturating_sub(30);
    let right = right_width.clamp(20, max_right.max(20));
    let list_w = w.saturating_sub(right + 1);
    Areas {
        header,
        list: Rect::new(0, body_y, list_w, body_h),
        splitter: Rect::new(list_w, body_y, 1, body_h),
        details: Rect::new(list_w + 1, body_y, right, body_h),
        status,
    }
}

/// Default details width: 38% of the screen, at least 32 cells.
pub fn default_right_width(w: u16) -> u16 {
    ((w as u32 * 38 / 100) as u16).max(32)
}

pub fn draw(app: &mut App, screen: &mut Screen, areas: &Areas, now: i64) {
    draw_underlay(app, screen, areas, now);
    if app.settings.open {
        settings::draw(&app.settings, screen);
    }
}

fn draw_underlay(app: &mut App, screen: &mut Screen, areas: &Areas, now: i64) {
    if app.branches.open {
        branches::draw(&mut app.branches, screen, &app.repo_name);
        return;
    }
    if let Mode::Diff(view) = &mut app.mode {
        let da = diff::compute_areas(screen.width(), screen.height(), view.sidebar_width);
        diff::draw(&app.repo_name, view, screen, &da);
        view.needs_redraw = false;
        return;
    }
    screen.clear();
    draw_header(app, screen, areas.header);
    list::draw(app, screen, areas.list, now);
    draw_splitter(app, screen, areas.splitter);
    details::draw(app, screen, areas.details, now);
    draw_status(app, screen, areas.status);
}

fn draw_header(app: &App, screen: &mut Screen, area: Rect) {
    let bg = theme::BG_HEADER;
    screen.fill(area, ' ', Style::new().bg(bg));
    let mut x = area.x + 1;
    x += screen.put_str(
        x,
        area.y,
        "git-rustle",
        Style::new().fg(theme::FG_KEY).bg(bg).bold(),
        area.w,
    );
    x += screen.put_str(x, area.y, "  ", Style::new().bg(bg), area.w);
    x += screen.put_str_ellipsis(
        x,
        area.y,
        &app.repo_name,
        Style::new().fg(theme::FG_TEXT).bg(bg).bold(),
        area.right().saturating_sub(x),
    );
    if let Some(refs) = &app.refs {
        let branch = refs
            .head_branch
            .clone()
            .unwrap_or_else(|| "HEAD detached".to_string());
        x += screen.put_str(
            x,
            area.y,
            "  on ",
            Style::new().fg(theme::FG_DIM).bg(bg),
            area.w,
        );
        screen.put_str_ellipsis(
            x,
            area.y,
            &branch,
            Style::new().fg(theme::FG_HEAD).bg(bg).bold(),
            area.right().saturating_sub(x),
        );
    }
    // Right side: commit count and state.
    let n = app.history.len();
    let state = match &app.load {
        LoadState::Empty => String::new(),
        LoadState::Cached => format!("{n} commits  cached"),
        LoadState::Refreshing => {
            let got = app.incoming.as_ref().map(|h| h.len()).unwrap_or(0);
            format!("{n} commits  refreshing {got}…")
        }
        LoadState::Loading => format!("{n} commits  loading…"),
        LoadState::Complete => format!("{n} commits"),
        LoadState::Error(_) => format!("{n} commits  error"),
    };
    let sw = crate::term::screen::str_width(&state) as u16;
    if sw + 1 < area.w {
        screen.put_str(
            area.right() - sw - 1,
            area.y,
            &state,
            Style::new().fg(theme::FG_DIM).bg(bg),
            sw,
        );
    }
}

fn draw_splitter(app: &App, screen: &mut Screen, area: Rect) {
    if area.w == 0 {
        return;
    }
    let color = if app.dragging_splitter {
        theme::FG_SPLITTER_ACTIVE
    } else {
        theme::FG_SPLITTER
    };
    screen.fill(area, '│', Style::new().fg(color));
}

fn draw_status(app: &App, screen: &mut Screen, area: Rect) {
    let bg = theme::BG_HEADER;
    if !app.ghost_status.is_empty() {
        screen.fill(area, ' ', theme::dim().bg(bg));
        let status = format!("b branches  s settings  r refresh  | {}", app.ghost_status);
        screen.put_str_ellipsis(
            area.x + 1,
            area.y,
            &status,
            theme::dim().bg(bg),
            area.w.saturating_sub(2),
        );
        return;
    }
    screen.fill(area, ' ', Style::new().bg(bg));
    let key = Style::new().fg(theme::FG_KEY).bg(bg).bold();
    let txt = Style::new().fg(theme::FG_DIM).bg(bg);
    let mut x = area.x + 1;
    let hints: &[(&str, &str)] = match (app.focus, app.marks.len()) {
        (Focus::List, 2) => &[
            ("d", "diff marked"),
            ("Space", "unmark"),
            ("Del", "clear"),
            ("↑↓", "move"),
            ("Tab", "details"),
            ("q", "quit"),
        ],
        (Focus::List, 1) => &[
            ("Space", "mark 2nd"),
            ("Del", "clear"),
            ("↑↓", "move"),
            ("PgUp/PgDn", "page"),
            ("H", "head"),
            ("Tab", "details"),
            ("q", "quit"),
        ],
        (Focus::List, _) => &[
            ("b", "branches"),
            ("s", "settings"),
            ("↑↓", "move"),
            ("PgUp/PgDn", "page"),
            ("Home/End", "ends"),
            ("H", "head"),
            ("d", "diff"),
            ("Space", "mark"),
            ("Tab", "details"),
            ("[ ]", "panel"),
            ("r", "reload"),
            ("q", "quit"),
        ],
        (Focus::Details, _) => &[
            ("↑↓", "scroll"),
            ("PgUp/PgDn", "page"),
            ("Tab", "files"),
            ("[ ]", "panel"),
            ("r", "reload"),
            ("q", "quit"),
        ],
        (Focus::Files, _) => &[
            ("↑↓", "move"),
            ("←→", "fold"),
            ("Enter", "diff"),
            ("e/c", "expand/collapse"),
            ("Tab", "list"),
            ("[ ]", "panel"),
            ("q", "quit"),
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
    if let LoadState::Error(msg) = &app.load {
        let s = format!("error: {msg}");
        let w = (crate::term::screen::str_width(&s) as u16).min(area.right().saturating_sub(x));
        if w > 0 {
            screen.put_str_ellipsis(
                area.right() - w,
                area.y,
                &s,
                Style::new().fg(theme::FG_ERR).bg(bg),
                w,
            );
        }
    } else if !app.history.is_empty() {
        let pos = format!("{}/{}", app.selected + 1, app.history.len());
        let w = pos.len() as u16;
        if x + w < area.right() {
            screen.put_str(area.right() - w - 1, area.y, &pos, txt, w);
        }
    }
}

/// Inline (non-chip) style for a ref name, used in the details panel.
pub(crate) fn label_style(kind: crate::git::RefKind, bg: Color) -> Style {
    use crate::git::RefKind::*;
    let s = Style::new().bg(bg);
    match kind {
        Head => s.fg(theme::FG_HEAD).bold(),
        Branch => s.fg(theme::FG_BRANCH),
        Remote => s.fg(theme::FG_REMOTE),
        Tag => s.fg(theme::FG_TAG),
        Other => s.fg(theme::FG_OTHER),
    }
}

/// Chip style for a ref name in the list: coloured background, dark text.
pub(crate) fn chip_style(kind: crate::git::RefKind) -> Style {
    use crate::git::RefKind::*;
    let ink = Color::rgb(0x12, 0x14, 0x1a);
    let s = Style::new().fg(ink);
    match kind {
        Head => s.bg(theme::FG_HEAD).bold(),
        Branch => s.bg(theme::FG_BRANCH),
        Remote => s.bg(theme::FG_REMOTE),
        Tag => s.bg(theme::FG_TAG),
        Other => s.bg(theme::FG_OTHER),
    }
}
