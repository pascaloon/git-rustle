//! Small embedded monochrome SVGs; GPUI caches their rasterization.
use gpui::{AssetSource, SharedString, Styled, px, rgb, svg};
use std::borrow::Cow;
pub struct Assets;
impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let body = match path.strip_prefix("rustle/").unwrap_or("") {
            "search" => "<circle cx='10' cy='10' r='6'/><path d='m15 15 5 5'/>",
            "refresh" => "<path d='M20 8a8 8 0 1 0 0 8M20 3v5h-5'/>",
            "settings" => {
                "<path d='m10 3 4 0 1 3 3 1 3 3-2 3 0 3-3 3-3-1-3 1-3-3 0-3-2-3 3-3 3-1z'/><circle cx='12' cy='12' r='3'/>"
            }
            "back" => "<path d='m15 5-7 7 7 7'/>",
            "next" => "<path d='m9 5 7 7-7 7'/>",
            "expand" => "<path d='m7 8 5-5 5 5m-10 8 5 5 5-5M4 12h16'/>",
            "collapse" => "<path d='m7 3 5 5 5-5m-10 18 5-5 5 5M4 12h16'/>",
            "diff" => "<path d='M4 3h16v18H4zM12 3v18M6 8h4m-2-2v4m6 4h4'/>",
            "branch" => {
                "<circle cx='6' cy='5' r='2'/><circle cx='18' cy='5' r='2'/><circle cx='6' cy='19' r='2'/><path d='M6 7v10m12-10v3c0 4-12 0-12 6'/>"
            }
            "head" => "<path d='m5 10 7-7 7 7M12 4v17'/>",
            "checkout" => "<path d='M4 4h9v16H4m5-8h12m-4-4 4 4-4 4'/>",
            _ => return Ok(None),
        };
        Ok(Some(Cow::Owned(format!("<svg xmlns='http://www.w3.org/2000/svg' width='24' height='24' viewBox='0 0 24 24' fill='none' stroke='black' stroke-width='1.7' stroke-linecap='round' stroke-linejoin='round'>{body}</svg>").into_bytes())))
    }
    fn list(&self, _: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(vec![])
    }
}
pub fn icon(name: &str) -> gpui::Svg {
    svg()
        .path(format!("rustle/{name}"))
        .size(px(16.))
        .flex_shrink_0()
        .text_color(rgb(0xdce5f4))
}
