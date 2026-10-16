use super::theme;
use crate::branches::Browser;
use crate::term::{Rect, Screen, Style};

pub fn draw(browser: &mut Browser, screen: &mut Screen, repo: &str) {
    screen.clear();
    let w = screen.width();
    let h = screen.height();
    let header = Style::new().fg(theme::FG_TEXT).bg(theme::BG_HEADER).bold();
    screen.fill(Rect::new(0, 0, w, 1), ' ', header);
    screen.put_str_ellipsis(
        1,
        0,
        &format!("Branches · {repo}"),
        header,
        w.saturating_sub(2),
    );
    let status = if browser.busy {
        "checkout running"
    } else if browser.loading {
        "checking remotes…"
    } else {
        "ready"
    };
    screen.put_str_ellipsis(
        1,
        1,
        &format!(
            "Filter: {}   | {} branches · {status}",
            browser.query,
            browser.visible.len()
        ),
        theme::text(),
        w.saturating_sub(2),
    );
    browser.rows = h.saturating_sub(6) as usize;
    browser.ensure_visible();
    for (row, &index) in browser
        .visible
        .iter()
        .skip(browser.scroll)
        .take(browser.rows)
        .enumerate()
    {
        let e = &browser.entries[index];
        let selected = browser.scroll + row == browser.selected;
        let mut style = Style::new().fg(if e.target.remote.is_some() {
            theme::FG_REMOTE
        } else {
            theme::FG_BRANCH
        });
        if selected {
            style = style.bg(theme::BG_SELECTED).bold();
        }
        let y = row as u16 + 2;
        screen.fill(Rect::new(0, y, w, 1), ' ', style);
        let line = format!(
            "{} {}  {}  [{}]",
            if e.current { '*' } else { ' ' },
            e.target.label(),
            &e.target.sha[..e.target.sha.len().min(8)],
            e.source
        );
        screen.put_str_ellipsis(1, y, &line, style, w.saturating_sub(2));
    }
    if browser.visible.is_empty() && h > 6 {
        screen.put_str_ellipsis(
            1,
            3,
            "No matching branches. Remote discovery does not fetch history.",
            theme::dim(),
            w.saturating_sub(2),
        );
    }
    let error = browser.errors.join(" | ");
    screen.put_str_ellipsis(
        1,
        h.saturating_sub(4),
        &error,
        Style::new().fg(theme::FG_ERR),
        w.saturating_sub(2),
    );
    screen.put_str_ellipsis(
        1,
        h.saturating_sub(3),
        &browser.status,
        theme::text(),
        w.saturating_sub(2),
    );
    screen.put_str_ellipsis(
        1,
        h.saturating_sub(2),
        "Ghost rows preview known branches; checkout downloads missing objects. * = current branch",
        theme::dim(),
        w.saturating_sub(2),
    );
    screen.fill(Rect::new(0, h.saturating_sub(1), w, 1), ' ', header);
    screen.put_str_ellipsis(
        1,
        h.saturating_sub(1),
        "↑↓ select  Enter checkout  Tab locate in graph  F5 refresh  F2 settings  Ctrl+U clear filter  Esc back",
        header,
        w.saturating_sub(2),
    );
}
