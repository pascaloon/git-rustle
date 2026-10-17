//! Unicode-safe search decoration and selectable, read-only prose.
use crate::desktop_theme;
use gpui::{prelude::*, *};
use gpui_component::scroll::{Scrollbar, ScrollbarShow};
use std::ops::Range;

actions!(rustle_text, [Copy]);
pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("ctrl-c", Copy, Some("RustleText"))]);
}

#[derive(Default, Clone)]
pub struct Matches {
    pub terms: Vec<String>,
    pub files: bool,
}
impl Matches {
    pub fn styled_filename(&self, path: &str, name: &str) -> StyledText {
        let offset = path.len().saturating_sub(name.len());
        let ranges = self.ranges(path).into_iter().filter_map(|r| {
            let start = r.start.max(offset);
            let end = r.end.min(path.len());
            (start < end).then(|| ((start - offset + 1)..(end - offset + 1), search_style()))
        });
        StyledText::new(format!(" {name}")).with_highlights(ranges)
    }
    pub fn new(query: &str) -> Self {
        let query = query.trim();
        let prefixed = matches!(query.get(..2), Some("m:" | "a:" | "f:" | "s:"));
        Self {
            terms: (if prefixed { &query[2..] } else { query })
                .split_whitespace()
                .map(str::to_lowercase)
                .collect(),
            files: query.starts_with("f:"),
        }
    }
    pub fn ranges(&self, text: &str) -> Vec<Range<usize>> {
        if self.terms.is_empty() {
            return vec![];
        }
        // Map lower-case bytes back to original character boundaries (İ expands).
        let mut lower = String::new();
        let mut map = Vec::new();
        for (start, ch) in text.char_indices() {
            let folded = ch.to_lowercase().to_string();
            map.extend(std::iter::repeat_n(
                start..start + ch.len_utf8(),
                folded.len(),
            ));
            lower.push_str(&folded);
        }
        let mut ranges: Vec<Range<usize>> = self
            .terms
            .iter()
            .flat_map(|term| {
                lower
                    .match_indices(term)
                    .map(|(i, t)| map[i].start..map[i + t.len() - 1].end)
            })
            .collect();
        ranges.sort_by_key(|r| r.start);
        let mut merged: Vec<Range<usize>> = vec![];
        for r in ranges {
            if let Some(last) = merged.last_mut()
                && r.start <= last.end
            {
                last.end = last.end.max(r.end);
            } else {
                merged.push(r);
            }
        }
        merged
    }
    pub fn styled(&self, text: impl Into<SharedString>) -> StyledText {
        let text = text.into();
        let ranges = self.ranges(&text);
        StyledText::new(text).with_highlights(ranges.into_iter().map(|r| (r, search_style())))
    }
}
fn search_style() -> HighlightStyle {
    HighlightStyle {
        color: Some(rgb(desktop_theme::SEARCH_TEXT).into()),
        background_color: Some(rgba(desktop_theme::SEARCH_BG).into()),
        ..Default::default()
    }
}

pub struct TextBlock {
    text: SharedString,
    query: String,
    highlights: Vec<Range<usize>>,
    anchor: usize,
    end: usize,
    dragging: bool,
    focus: FocusHandle,
    scroll: ScrollHandle,
    hover: bool,
}
impl TextBlock {
    pub fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        if self.query != query {
            self.query = query.into();
            self.highlights = Matches::new(query).ranges(&self.text);
            cx.notify();
        }
    }
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            text: "".into(),
            query: String::new(),
            highlights: vec![],
            anchor: 0,
            end: 0,
            dragging: false,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            hover: false,
        }
    }
    pub fn set(&mut self, text: String, query: &str, cx: &mut Context<Self>) {
        if self.text.as_ref() == text && self.query == query {
            return;
        }
        if self.text.as_ref() != text {
            self.anchor = 0;
            self.end = 0;
            self.scroll.set_offset(point(px(0.), px(0.)));
        }
        self.text = text.into();
        self.query = query.to_owned();
        self.highlights = Matches::new(query).ranges(&self.text);
        cx.notify();
    }
}
impl Render for TextBlock {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let search = self.highlights.iter().cloned().map(|r| (r, search_style()));
        let selected = (self.anchor != self.end).then(|| {
            (
                self.anchor.min(self.end)..self.anchor.max(self.end),
                HighlightStyle {
                    background_color: Some(rgba(desktop_theme::SELECTION).into()),
                    ..Default::default()
                },
            )
        });
        let text = StyledText::new(self.text.clone())
            .with_highlights(combine_highlights(search, selected));
        let hit = text.layout().clone();
        let drag = hit.clone();
        div()
            .id("selectable-text")
            .size_full()
            .relative()
            .track_focus(&self.focus)
            .key_context("RustleText")
            .on_hover(cx.listener(|v, h, _, cx| {
                v.hover = *h;
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &Copy, _, cx| {
                if v.anchor != v.end {
                    cx.write_to_clipboard(ClipboardItem::new_string(
                        v.text[v.anchor.min(v.end)..v.anchor.max(v.end)].to_owned(),
                    ));
                    cx.stop_propagation();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|v, _, _, _| v.dragging = false),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|v, _, _, _| v.dragging = false),
            )
            .child(
                div()
                    .id("text-content")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        div()
                            .w_full()
                            .cursor_text()
                            .line_height(px(21.))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |v, e: &MouseDownEvent, window, cx| {
                                    let index = hit
                                        .index_for_position(e.position)
                                        .unwrap_or_else(|i| i)
                                        .min(v.text.len());
                                    v.anchor = index;
                                    v.end = index;
                                    v.dragging = e.click_count < 2;
                                    if e.click_count >= 2 {
                                        v.anchor = 0;
                                        v.end = v.text.len();
                                    }
                                    window.focus(&v.focus);
                                    cx.notify();
                                    cx.stop_propagation();
                                }),
                            )
                            .on_mouse_move(cx.listener(move |v, e: &MouseMoveEvent, _, cx| {
                                if v.dragging && e.pressed_button == Some(MouseButton::Left) {
                                    v.end = drag
                                        .index_for_position(e.position)
                                        .unwrap_or_else(|i| i)
                                        .min(v.text.len());
                                    cx.notify();
                                }
                            }))
                            .child(text),
                    ),
            )
            .child(div().absolute().inset_0().child(
                Scrollbar::vertical(&self.scroll).scrollbar_show(if self.hover {
                    ScrollbarShow::Always
                } else {
                    ScrollbarShow::Scrolling
                }),
            ))
    }
}
#[cfg(test)]
mod tests {
    use super::Matches;
    #[test]
    fn unicode_expansion_and_multiple_terms_have_valid_original_ranges() {
        let m = Matches::new("m: i café");
        let text = "İ café CAFÉ";
        let r = m.ranges(text);
        assert_eq!(
            r.iter().map(|r| &text[r.clone()]).collect::<Vec<_>>(),
            vec!["İ", "café", "CAFÉ"]
        );
    }
    #[test]
    fn overlapping_terms_are_merged_and_empty_search_is_free() {
        assert_eq!(
            Matches::new("m: rust rustle").ranges("rustle rust"),
            vec![0..6, 7..11]
        );
        assert!(Matches::new("").ranges("anything").is_empty());
    }
}
