//! Modal rendering and hit testing; no configuration or network I/O.
use super::theme;
use crate::settings::Settings;
use crate::term::{Button, MouseEvent, MouseKind, Rect, Screen};

fn area(w: u16, h: u16) -> Rect {
    let width = w.min(94);
    let height = h.min(25);
    Rect::new((w - width) / 2, (h - height) / 2, width, height)
}

pub fn draw(settings: &Settings, screen: &mut Screen) {
    let r = area(screen.width(), screen.height());
    let text = theme::text().bg(theme::BG_HEADER);
    let dim = theme::dim().bg(theme::BG_HEADER);
    let active = text.bg(theme::BG_SELECTED).bold();
    screen.fill(r, ' ', text);
    if r.w < 60 || r.h < 25 {
        screen.put_str(
            1,
            r.y,
            "Settings: resize to 60 x 25 or larger. Esc closes.",
            text,
            r.w.saturating_sub(2),
        );
        return;
    }
    for x in r.x..r.right() {
        screen.put_char(x, r.y, '─', dim);
        screen.put_char(x, r.y + r.h - 1, '─', dim);
    }
    for y in r.y + 1..r.y + r.h - 1 {
        screen.put_char(r.x, y, '│', dim);
        screen.put_char(r.right() - 1, y, '│', dim);
    }
    let x = r.x + 3;
    let w = r.w - 6;
    screen.put_str(x, r.y, " Remote settings ", text.bold(), w);
    screen.put_str(
        x,
        r.y + 2,
        " F1 GitLab ",
        if settings.tab == 0 { active } else { dim },
        12,
    );
    screen.put_str(
        x + 15,
        r.y + 2,
        " F2 GitHub ",
        if settings.tab == 1 { active } else { dim },
        12,
    );
    screen.put_str(
        x,
        r.y + 3,
        "Tab / Shift+Tab: fields   Ctrl+Left/Right: tabs",
        dim,
        w,
    );
    if let Some(d) = settings.current() {
        let remote = format!("< {} >   (Left/Right to choose)", d.config.name);
        screen.put_str_ellipsis(
            x,
            r.y + 5,
            &remote,
            if settings.field == 1 { active } else { text },
            w,
        );
        for (field, y, label, value) in [
            (
                2,
                7,
                "Server / API URL (HTTPS; on-premise supported)",
                d.config.api_url.as_deref().unwrap_or_default(),
            ),
            (
                3,
                10,
                if settings.tab == 0 {
                    "Project path: group/subgroup/project"
                } else {
                    "Project path: owner/repository"
                },
                d.config.project.as_deref().unwrap_or_default(),
            ),
            (
                4,
                13,
                "API token (masked; stored in Windows Credential Manager)",
                d.token.as_str(),
            ),
        ] {
            screen.put_str(x, r.y + y, label, dim, w);
            let focused = settings.field == field && !settings.busy;
            let style = if focused {
                active
            } else {
                text.bg(theme::BG_FILLER)
            };
            screen.fill(Rect::new(x, r.y + y + 1, w, 1), ' ', style);
            let shown = if field == 4 {
                "*".repeat(value.chars().count())
            } else {
                value.to_string()
            };
            let cursor = value
                .char_indices()
                .take_while(|(i, _)| *i < d.cursor)
                .count();
            // Input uses character boundaries; width-aware scrolling keeps Unicode paths usable.
            let chars: Vec<char> = shown.chars().collect();
            let cursor = if focused { cursor.min(chars.len()) } else { 0 };
            let mut start = 0;
            while crate::term::screen::str_width(&chars[start..cursor].iter().collect::<String>())
                >= w as usize
            {
                start += 1;
            }
            let visible: String = chars[start..].iter().collect();
            screen.put_str(x, r.y + y + 1, &visible, style, w);
            if focused {
                let offset = crate::term::screen::str_width(
                    &chars[start..cursor].iter().collect::<String>(),
                ) as u16;
                screen.put_char(
                    x + offset,
                    r.y + y + 1,
                    chars.get(cursor).copied().unwrap_or(' '),
                    style.bg(theme::FG_TEXT).fg(theme::BG_HEADER),
                );
            }
        }
        screen.put_str_ellipsis(
            x,
            r.y + 15,
            if d.forget {
                "Saved token will be removed. Ctrl+D cancels removal."
            } else {
                "Blank keeps saved token / env / Git auth. Ctrl+D removes saved token."
            },
            dim,
            w,
        );
        screen.put_str(
            x,
            r.y + 17,
            " Test (Ctrl+T) ",
            if settings.field == 5 { active } else { text },
            17,
        );
        screen.put_str(
            x + 22,
            r.y + 17,
            " Save (Ctrl+S) ",
            if settings.field == 6 { active } else { text },
            17,
        );
    }
    let status = if settings.error {
        text.fg(theme::FG_ERR)
    } else {
        text.fg(theme::FG_HEAD)
    };
    let mut remaining = settings.status.as_str();
    for row in 19..22 {
        let end = remaining
            .char_indices()
            .nth(w as usize)
            .map_or(remaining.len(), |(i, _)| i);
        screen.put_str(x, r.y + row, &remaining[..end], status, w);
        remaining = &remaining[end..];
    }
    screen.put_str(
        x,
        r.y + 23,
        "Esc: close without saving   Ctrl+U: clear field",
        dim,
        w,
    );
}

pub fn handle_mouse(settings: &mut Settings, mouse: MouseEvent, w: u16, h: u16) {
    let r = area(w, h);
    if settings.busy
        || r.w < 60
        || r.h < 25
        || !r.contains(mouse.x, mouse.y)
        || mouse.kind != MouseKind::Down(Button::Left)
    {
        return;
    }
    let x = mouse.x.saturating_sub(r.x + 3);
    match mouse.y - r.y {
        2 if x < 27 => {
            settings.tab = usize::from(x >= 15);
            settings.field = 0;
        }
        5 => {
            settings.field = 1;
            let n = settings.drafts[settings.tab].len();
            if n > 0 {
                settings.selected[settings.tab] = (settings.selected[settings.tab] + 1) % n;
            }
        }
        7 | 8 => settings.field = 2,
        10 | 11 => settings.field = 3,
        13 | 14 => settings.field = 4,
        17 if x < 17 => {
            settings.field = 5;
            settings.action(false);
        }
        17 if (22..39).contains(&x) => {
            settings.field = 6;
            settings.action(true);
        }
        _ => {}
    }
    settings.end_cursor();
}
