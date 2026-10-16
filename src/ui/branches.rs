use super::{theme, time};
use crate::branches::Browser;
use crate::term::{Rect, Screen, Style};

pub fn draw(browser: &mut Browser, screen: &mut Screen, repo: &str, now: i64) {
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
            "Filter: {}   | {} branches · {status} · activity ↓",
            browser.query,
            browser.visible.len()
        ),
        theme::text(),
        w.saturating_sub(2),
    );
    browser.rows = h.saturating_sub(5) as usize;
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
            "{} {}  {}",
            if e.current { '*' } else { ' ' },
            e.target.label(),
            &e.target.sha[..e.target.sha.len().min(8)]
        );
        let show_author = w >= 70;
        let author_width = (w / 4).clamp(20, 40);
        let show_age = w >= 50;
        let right = w.saturating_sub(1);
        let reserved = if show_author {
            author_width + 15
        } else if show_age {
            14
        } else {
            0
        };
        screen.put_str_ellipsis(1, y, &line, style, w.saturating_sub(2 + reserved));
        if show_age {
            let age = e
                .activity
                .as_ref()
                .map(|a| time::format_relative(a.author_time, now))
                .unwrap_or_else(|| "—".into());
            let age_style = Style::new().fg(theme::FG_DATE).bg(style.bg);
            screen.put_str(
                right.saturating_sub(age.len().min(4) as u16),
                y,
                &age,
                age_style,
                4,
            );
            let score = e
                .activity
                .as_ref()
                .map(|a| format!("{}{} /wk", a.recent, if a.limited { "+" } else { "" }))
                .unwrap_or_else(|| "— /wk".into());
            screen.put_str_ellipsis(right.saturating_sub(13), y, &score, age_style, 8);
        }
        if show_author {
            let author = e
                .activity
                .as_ref()
                .map(|a| a.author.as_str())
                .unwrap_or("—");
            screen.put_str_ellipsis(
                right.saturating_sub(14 + author_width),
                y,
                author,
                Style::new().fg(theme::FG_AUTHOR).bg(style.bg),
                author_width,
            );
        }
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
        h.saturating_sub(3),
        &error,
        Style::new().fg(theme::FG_ERR),
        w.saturating_sub(2),
    );
    screen.put_str_ellipsis(
        1,
        h.saturating_sub(2),
        if browser.status.is_empty() {
            browser
                .chosen()
                .and_then(|e| browser.activity_errors.get(&e.target))
                .unwrap_or(&browser.status)
        } else {
            &browser.status
        },
        theme::text(),
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
