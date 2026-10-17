//! Shared horizontal text offset with a fixed, equal-width diff split.
use gpui::{Bounds, Pixels, Point, Size, UniformListScrollHandle, point, px, size};
use gpui_component::scroll::ScrollbarHandle;
use std::{cell::Cell, rc::Rc};

pub const GUTTER: f32 = 56.;
const BAR_SPACE: f32 = 12.;

#[derive(Clone)]
pub struct DiffScroll {
    pub bounds: Rc<Cell<Bounds<Pixels>>>,
    vertical: UniformListScrollHandle,
    pub x: Rc<Cell<Pixels>>,
    width: Rc<Cell<Pixels>>,
    max_x: Rc<Cell<Pixels>>,
}

impl DiffScroll {
    pub fn new(vertical: UniformListScrollHandle) -> Self {
        Self {
            bounds: Default::default(),
            vertical,
            x: Default::default(),
            width: Default::default(),
            max_x: Default::default(),
        }
    }

    pub fn configure(&self, width: Pixels, line_width: Pixels, single_sided: bool) {
        let text_width =
            (width / if single_sided { 1. } else { 2. } - px(GUTTER + BAR_SPACE)).max(px(0.));
        self.width.set(width);
        self.max_x.set((line_width - text_width).max(px(0.)));
        self.set_x(self.x.get());
    }

    pub fn set_x(&self, x: Pixels) {
        self.x.set(x.clamp(-self.max_x.get(), px(0.)));
    }
}

impl ScrollbarHandle for DiffScroll {
    fn offset(&self) -> Point<Pixels> {
        point(self.x.get(), self.vertical.offset().y)
    }
    fn set_offset(&self, offset: Point<Pixels>) {
        self.set_x(offset.x);
        self.vertical.set_offset(point(px(0.), offset.y));
    }
    fn content_size(&self) -> Size<Pixels> {
        size(
            self.width.get() + self.max_x.get(),
            self.vertical.content_size().height,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_panes_share_a_clamped_text_offset_and_resize_safely() {
        let scroll = DiffScroll::new(UniformListScrollHandle::new());
        scroll.configure(px(1000.), px(800.), false);
        scroll.set_x(px(-2000.));
        assert_eq!(scroll.offset().x, px(-368.));
        assert_eq!(scroll.content_size().width, px(1368.));
        scroll.configure(px(1800.), px(800.), false);
        assert_eq!(scroll.offset().x, px(0.));
        assert_eq!(scroll.content_size().width, px(1800.));
    }
    #[test]
    fn added_and_short_files_do_not_get_spurious_horizontal_overflow() {
        let scroll = DiffScroll::new(UniformListScrollHandle::new());
        scroll.configure(px(1000.), px(800.), true);
        scroll.set_x(px(-100.));
        assert_eq!(scroll.offset().x, px(0.));
        scroll.configure(px(1000.), px(1200.), true);
        scroll.set_x(px(-1000.));
        assert_eq!(scroll.offset().x, px(-268.));
        scroll.set_x(px(10.));
        assert_eq!(scroll.offset().x, px(0.));
    }
}
