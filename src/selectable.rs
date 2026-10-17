//! Continuous diff selection without a retained editor per visible row.
use gpui::{prelude::*, *};
use std::{
    cell::{Cell, RefCell},
    ops::Range,
    rc::Rc,
};
#[derive(Default)]
pub struct Selection {
    pub id: Option<(u32, bool)>,
    pub anchor: (usize, usize),
    pub end: (usize, usize),
    pub dragging: bool,
}
impl Selection {
    pub fn range(&self, row: usize, len: usize) -> Option<Range<usize>> {
        let (a, b) = (self.anchor.min(self.end), self.anchor.max(self.end));
        if row < a.0 || row > b.0 {
            return None;
        }
        Some(
            (if row == a.0 { a.1.min(len) } else { 0 })..(if row == b.0 {
                b.1.min(len)
            } else {
                len
            }),
        )
    }
    pub fn copy(&self, diff: &crate::diff::FileDiff) -> String {
        let Some((_, right)) = self.id else {
            return String::new();
        };
        let (a, b) = (self.anchor.min(self.end), self.anchor.max(self.end));
        let line_at = |row| match diff.rows.get(row) {
            Some(crate::diff::Row::Line { left, right: r }) => {
                if right {
                    r.as_ref()
                } else {
                    left.as_ref()
                }
            }
            _ => None,
        };
        let (Some(first), Some(last)) = (line_at(a.0), line_at(b.0)) else {
            return String::new();
        };
        // Clipboard work is deferred until Copy; scrolling never builds a document.
        diff.source[usize::from(right)]
            .split_inclusive(|&c| c == b'\n')
            .enumerate()
            .skip(first.no.saturating_sub(1) as usize)
            .take((last.no - first.no + 1) as usize)
            .map(|(i, line)| {
                let text = crate::diff::sidebyside::decode(line);
                let start = if i + 1 == first.no as usize {
                    a.1.min(text.len())
                } else {
                    0
                };
                let end = if i + 1 == last.no as usize {
                    b.1.min(text.len())
                } else {
                    text.len()
                };
                text.get(start..end).unwrap_or("").to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
pub fn line(
    id: (u32, usize, bool, usize),
    text: String,
    highlights: Vec<Range<usize>>,
    selection: Rc<RefCell<Selection>>,
    focus: FocusHandle,
    horizontal: Rc<Cell<Pixels>>,
    offset: usize,
) -> impl IntoElement {
    type LineLayout = Rc<RefCell<Option<(Bounds<Pixels>, ShapedLine)>>>;
    let layout: LineLayout = Rc::new(RefCell::new(None));
    let hit = layout.clone();
    let drag = layout.clone();
    let paint_selection = selection.clone();
    let click_selection = selection.clone();
    let display: SharedString = text.into();
    let hit_offset = horizontal.clone();
    let drag_offset = horizontal.clone();
    div()
        .id((if id.2 { "right-text" } else { "left-text" }, id.3))
        .flex_1()
        .min_w_0()
        .h_full()
        .overflow_hidden()
        .cursor_text()
        .on_mouse_down(MouseButton::Left, move |event, window, cx| {
            if let Some((bounds, line)) = hit.borrow().as_ref() {
                let index =
                    line.closest_index_for_x(event.position.x - bounds.left() - hit_offset.get());
                let mut s = click_selection.borrow_mut();
                s.id = Some((id.0, id.2));
                s.anchor = (id.1, index + offset);
                s.end = (id.1, index + offset);
                s.dragging = event.click_count < 2;
                if event.click_count >= 2 {
                    s.anchor = (id.1, offset);
                    s.end = (id.1, offset + line.text.len());
                }
                window.focus(&focus);
                window.refresh();
                cx.stop_propagation();
            }
        })
        .on_mouse_move(move |event, window, _| {
            let mut s = selection.borrow_mut();
            if s.id == Some((id.0, id.2))
                && s.dragging
                && event.pressed_button == Some(MouseButton::Left)
                && let Some((bounds, line)) = drag.borrow().as_ref()
                && bounds.contains(&event.position)
            {
                s.end = (
                    id.1,
                    offset
                        + line.closest_index_for_x(
                            event.position.x - bounds.left() - drag_offset.get(),
                        ),
                );
                window.refresh();
            }
        })
        .child(
            canvas(
                move |_, window, _| {
                    let style = window.text_style();
                    window.text_system().shape_line(
                        display.clone(),
                        px(12.),
                        &[TextRun {
                            len: display.len(),
                            font: style.font(),
                            color: style.color,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        }],
                        None,
                    )
                },
                move |bounds, line, window, cx| {
                    let left = bounds.left() + horizontal.get();
                    for range in &highlights {
                        let start = line
                            .text
                            .char_indices()
                            .nth(range.start)
                            .map(|(i, _)| i)
                            .unwrap_or(line.text.len());
                        let end = line
                            .text
                            .char_indices()
                            .nth(range.end)
                            .map(|(i, _)| i)
                            .unwrap_or(line.text.len());
                        window.paint_quad(fill(
                            Bounds::from_corners(
                                point(left + line.x_for_index(start), bounds.top()),
                                point(left + line.x_for_index(end), bounds.bottom()),
                            ),
                            rgba(if id.2 { 0x4cba6644 } else { 0xf06c8444 }),
                        ));
                    }
                    let s = paint_selection.borrow();
                    if s.id == Some((id.0, id.2))
                        && let Some(range) = s.range(id.1, offset + line.text.len())
                    {
                        window.paint_quad(fill(
                            Bounds::from_corners(
                                point(
                                    left + line.x_for_index(range.start.saturating_sub(offset)),
                                    bounds.top(),
                                ),
                                point(
                                    left + line.x_for_index(range.end.saturating_sub(offset)),
                                    bounds.bottom(),
                                ),
                            ),
                            rgba(0x7dcfff66),
                        ));
                    }
                    let _ = line.paint(point(left, bounds.top() + px(3.)), px(18.), window, cx);
                    *layout.borrow_mut() = Some((bounds, line));
                },
            )
            .size_full(),
        )
}
#[cfg(test)]
mod tests {
    use super::Selection;
    #[test]
    fn wrapped_selection_copies_original_lines_without_visual_breaks() {
        let text = "a long café line with spaces\nnext line 世界\n";
        let diff = crate::diff::sidebyside::build(b"", text.as_bytes());
        let s = Selection {
            id: Some((1, true)),
            anchor: (0, 7),
            end: (1, "next line 世".len()),
            ..Default::default()
        };
        assert_eq!(s.copy(&diff), "café line with spaces\nnext line 世");
        // Each wrapped chunk clamps source offsets to its own byte range.
        let range = s.range(0, 16).unwrap();
        assert_eq!(
            range.start.saturating_sub(7)..range.end.saturating_sub(7),
            0..9
        );
    }
    #[test]
    fn backwards_block_copy_includes_collapsed_context_and_unicode() {
        let old = (1..30)
            .map(|i| format!("old {i} aé界z\n"))
            .collect::<String>();
        let new = old
            .replace("old 1 ", "new 1 ")
            .replace("old 29 ", "new 29 ");
        let diff = crate::diff::sidebyside::build(old.as_bytes(), new.as_bytes());
        let last = diff
            .rows
            .iter()
            .position(|r| matches!(r,crate::diff::Row::Line{right:Some(l),..} if l.no==29))
            .unwrap();
        let s = Selection {
            id: Some((1, true)),
            anchor: (last, "new 29 aé界".len()),
            end: (0, 4),
            ..Default::default()
        };
        let copy = s.copy(&diff);
        assert!(copy.starts_with("1 aé界z\nold 2"));
        assert!(copy.contains("old 15 aé界z"));
        assert!(copy.ends_with("new 29 aé界"));
        assert_eq!(s.range(0, 12), Some(4..12));
        assert_eq!(s.range(last + 1, 12), None);
    }
}
