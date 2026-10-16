//! Keyboard-only checkout confirmation, over either existing view.
use super::theme;
use crate::checkout::{Confirmation, Request};
use crate::term::{Rect, Screen};

pub fn draw(dialog: &mut Confirmation, screen: &mut Screen) {
    let w = screen.width().min(84);
    let h = screen.height().min(dialog.choices.len().min(6) as u16 + 8);
    let r = Rect::new((screen.width() - w) / 2, (screen.height() - h) / 2, w, h);
    let text = theme::text().bg(theme::BG_HEADER);
    let dim = theme::dim().bg(theme::BG_HEADER);
    screen.fill(r, ' ', text);
    dialog.ready = w >= 40 && h >= 9;
    if !dialog.ready {
        screen.put_str_ellipsis(
            r.x,
            r.y,
            "Resize to confirm checkout. Esc cancels.",
            text,
            w,
        );
        return;
    }
    for x in r.x..r.right() {
        screen.put_char(x, r.y, '─', dim);
        screen.put_char(x, r.bottom() - 1, '─', dim);
    }
    for y in r.y + 1..r.bottom() - 1 {
        screen.put_char(r.x, y, '│', dim);
        screen.put_char(r.right() - 1, y, '│', dim);
    }
    let x = r.x + 2;
    let width = r.w - 4;
    let title = match dialog.chosen() {
        Request::Branch(_) => " Checkout branch? ",
        Request::Commit { .. } => " Checkout commit? ",
    };
    screen.put_str(x, r.y, title, text.bold(), width);
    screen.put_str_ellipsis(
        x,
        r.y + 1,
        if dialog.choices.len() > 1 {
            "Up/Down: choose a branch or commit"
        } else {
            "Confirm checkout:"
        },
        dim,
        width,
    );
    let slots = (h - 8) as usize;
    let start = dialog.selected.saturating_sub(slots - 1);
    for (row, choice) in dialog.choices.iter().skip(start).take(slots).enumerate() {
        let active = start + row == dialog.selected;
        let style = if active {
            text.bg(theme::BG_SELECTED).bold()
        } else {
            text
        };
        let label = match choice {
            Request::Branch(t) => format!(
                "{} {} branch: {}",
                if active { '>' } else { ' ' },
                if t.remote.is_some() {
                    "Remote"
                } else {
                    "Local"
                },
                t.label()
            ),
            Request::Commit { .. } => format!(
                "{} Commit: {}",
                if active { '>' } else { ' ' },
                choice.label()
            ),
        };
        screen.fill(Rect::new(x, r.y + 3 + row as u16, width, 1), ' ', style);
        screen.put_str_ellipsis(x, r.y + 3 + row as u16, &label, style, width);
    }
    let effect = match dialog.chosen() {
        Request::Branch(t) if t.remote.is_some() => {
            "Use/create a local tracking branch; may download commits."
        }
        Request::Branch(_) => "Switch to this local branch.",
        Request::Commit {
            remote: Some(_), ..
        } => "Detach HEAD here; missing objects will be downloaded.",
        Request::Commit { .. } => "Detach HEAD here without creating a branch.",
    };
    screen.put_str_ellipsis(x, r.bottom() - 4, effect, dim, width);
    screen.put_str(
        x,
        r.bottom() - 2,
        "Enter: confirm   Esc: cancel",
        text.fg(theme::FG_KEY),
        width,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn popup_shows_effect_and_selected_choice_and_handles_sizes() {
        let id = gix::ObjectId::from_hex("a".repeat(40).as_bytes()).unwrap();
        let mut choices = (0..20)
            .map(|i| {
                Request::Branch(crate::branches::Target {
                    name: format!("branch-{i}"),
                    remote: None,
                    url: None,
                    sha: id.to_string(),
                })
            })
            .collect::<Vec<_>>();
        choices.push(Request::Commit { id, remote: None });
        let mut dialog = Confirmation::new(choices);
        dialog.selected = dialog.choices.len() - 1;
        let mut screen = Screen::new(120, 30);
        draw(&mut dialog, &mut screen);
        let text: String = screen.test_cells().iter().map(|c| c.ch).collect();
        assert!(dialog.ready);
        assert!(text.contains("Checkout commit?"));
        assert!(text.contains("detached HEAD"));
        assert!(text.contains("Enter: confirm   Esc: cancel"));
        assert!(
            screen
                .test_cells()
                .iter()
                .any(|c| c.ch == '>' && c.style.bg == theme::BG_SELECTED)
        );
        for (w, h) in [(0, 0), (20, 5), (39, 20), (40, 9), (80, 10), (120, 40)] {
            draw(&mut dialog, &mut Screen::new(w, h));
            assert_eq!(dialog.ready, w >= 40 && h >= 9);
        }
    }
}
