//! Single-line selection without a retained editor or parser per visible row.
use gpui::{prelude::*, *};
use std::{cell::RefCell, ops::Range, rc::Rc};
#[derive(Default)]
pub struct Selection {
    pub id: Option<(u32, usize, bool)>,
    pub text: String,
    pub anchor: usize,
    pub end: usize,
    pub dragging: bool,
}
impl Selection {
    pub fn selected(&self) -> &str {
        &self.text[self.anchor.min(self.end)..self.anchor.max(self.end)]
    }
}
pub fn line(
    id: (u32, usize, bool),
    text: String,
    highlights: Vec<Range<usize>>,
    selection: Rc<RefCell<Selection>>,
    focus: FocusHandle,
) -> impl IntoElement {
    type LineLayout = Rc<RefCell<Option<(Bounds<Pixels>, ShapedLine)>>>;
    let layout: LineLayout = Rc::new(RefCell::new(None));
    let hit = layout.clone();
    let drag = layout.clone();
    let paint_selection = selection.clone();
    let click_selection = selection.clone();
    let click_text = text.clone();
    let display: SharedString = text.into();
    div()
        .id((if id.2 { "right-text" } else { "left-text" }, id.1))
        .flex_1()
        .h_full()
        .overflow_hidden()
        .cursor_text()
        .on_mouse_down(MouseButton::Left, move |event, window, cx| {
            if let Some((bounds, line)) = hit.borrow().as_ref() {
                let index = line.closest_index_for_x(event.position.x - bounds.left());
                let mut s = click_selection.borrow_mut();
                s.id = Some(id);
                s.text = click_text.clone();
                s.anchor = index;
                s.end = index;
                s.dragging = true;
                if event.click_count >= 2 {
                    s.anchor = 0;
                    s.end = s.text.len();
                }
                window.focus(&focus);
                window.refresh();
                cx.stop_propagation();
            }
        })
        .on_mouse_move(move |event, window, _| {
            let mut s = selection.borrow_mut();
            if s.id == Some(id)
                && s.dragging
                && event.pressed_button == Some(MouseButton::Left)
                && let Some((bounds, line)) = drag.borrow().as_ref()
            {
                s.end = line.closest_index_for_x(event.position.x - bounds.left());
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
                                point(bounds.left() + line.x_for_index(start), bounds.top()),
                                point(bounds.left() + line.x_for_index(end), bounds.bottom()),
                            ),
                            rgba(if id.2 { 0x4cba6644 } else { 0xf06c8444 }),
                        ));
                    }
                    let s = paint_selection.borrow();
                    if s.id == Some(id) && s.text.as_str() == line.text.as_ref() {
                        window.paint_quad(fill(
                            Bounds::from_corners(
                                point(
                                    bounds.left() + line.x_for_index(s.anchor.min(s.end)),
                                    bounds.top(),
                                ),
                                point(
                                    bounds.left() + line.x_for_index(s.anchor.max(s.end)),
                                    bounds.bottom(),
                                ),
                            ),
                            rgba(0x7dcfff66),
                        ));
                    }
                    let _ = line.paint(
                        point(bounds.left(), bounds.top() + px(3.)),
                        px(18.),
                        window,
                        cx,
                    );
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
    fn backwards_unicode_selection_uses_byte_boundaries() {
        let s = Selection {
            text: "aé界z".into(),
            anchor: 6,
            end: 1,
            ..Default::default()
        };
        assert_eq!(s.selected(), "é界");
    }
}
