//! Small embedded monochrome SVGs; GPUI caches their rasterization.
use gpui::{AssetSource, SharedString, Styled, px, rgb, svg};
use std::borrow::Cow;
pub struct Assets;
impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if path == "rustle/app-icon.png" {
            return Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/app-icon.png"
            ))));
        }
        let name = match path {
            "icons/circle-x.svg" => "clear",
            "icons/check.svg" => "check",
            "icons/chevron-down.svg" => "down",
            "icons/loader.svg" => "spinner",
            _ => path.strip_prefix("rustle/").unwrap_or(""),
        };
        let body = match name {
            "trash" => "<path d='M3 6h18M9 6V3h6v3M5 6l1 15h12l1-15M10 10v7m4-7v7'/>",
            "external-link" => "<path d='M14 3h7v7m0-7L10 14M10 3H3v18h18v-7'/>",
            "pipeline-pending" => "<circle cx='12' cy='12' r='9'/><path d='M12 7v5l3 2'/>",
            "pipeline-running" => "<circle cx='12' cy='12' r='9'/><path d='m10 8 6 4-6 4z'/>",
            "pipeline-success" => "<circle cx='12' cy='12' r='9'/><path d='m7 12 3 3 7-7'/>",
            "pipeline-warning" => "<path d='M12 3 2 21h20zM12 9v5m0 3h.01'/>",
            "pipeline-failed" => "<circle cx='12' cy='12' r='9'/><path d='m8 8 8 8m0-8-8 8'/>",
            "stash" => "<path d='M3 5h18v4H3zM5 9v12h14V9M9 13h6'/>",
            "broom" => "<path d='m16 3-5 10m-5-1 9 4-3 6-10-5zm0 3-2 3m5-2-2 3'/>",
            "tool" => {
                "<rect x='3' y='3' width='18' height='18' rx='3'/><path d='m10 8-4 4 4 4m4-8 4 4-4 4'/>"
            }
            "shield-check" => {
                "<path d='M12 3 4 6v6c0 5 8 9 8 9s8-4 8-9V6z'/><path d='m8 12 3 3 5-6'/>"
            }
            "shield-alert" => {
                "<path d='M12 3 4 6v6c0 5 8 9 8 9s8-4 8-9V6z'/><path d='M12 8v5m0 3h.01'/>"
            }
            "pull" => "<path d='M12 3v12m-5-5 5 5 5-5M4 17v4h16v-4'/>",
            "push" => "<path d='M12 16V4m-5 5 5-5 5 5M4 17v4h16v-4'/>",
            "check" => "<path d='m5 12 4 4L19 6'/>",
            "history" => "<path d='M3 11a9 9 0 1 1 2 7M3 4v7h7M12 7v5l3 2'/>",
            "undo" => "<path d='m8 4-5 5 5 5M3 9h10a7 7 0 0 1 7 7v4'/>",
            "redo" => "<path d='m16 4 5 5-5 5m5-5H11a7 7 0 0 0-7 7v4'/>",
            "close" => "<path d='m6 6 12 12M18 6 6 18'/>",
            "down" => "<path d='m6 9 6 6 6-6'/>",
            "copy" => "<rect x='8' y='8' width='12' height='13' rx='2'/><path d='M16 8V3H3v13h5'/>",
            "clear" => "<circle cx='12' cy='12' r='9'/><path d='m9 9 6 6m0-6-6 6'/>",
            "search" => "<circle cx='10' cy='10' r='6'/><path d='m15 15 5 5'/>",
            "refresh" => "<path d='M20 8a8 8 0 1 0 0 8M20 3v5h-5'/>",
            "spinner" => "<path d='M12 3a9 9 0 1 1-9 9'/>",
            "settings" => {
                "<path d='m10 3 4 0 1 3 3 1 3 3-2 3 0 3-3 3-3-1-3 1-3-3 0-3-2-3 3-3 3-1z'/><circle cx='12' cy='12' r='3'/>"
            }
            "back" => "<path d='m15 5-7 7 7 7'/>",
            "next" | "right" => "<path d='m9 5 7 7-7 7'/>",
            "expand" => "<path d='m7 8 5-5 5 5m-10 8 5 5 5-5M4 12h16'/>",
            "collapse" => "<path d='m7 3 5 5 5-5m-10 18 5-5 5 5M4 12h16'/>",
            "diff" => "<path d='M4 3h16v18H4zM12 3v18M6 8h4m-2-2v4m6 4h4'/>",
            "branch" => {
                "<circle cx='6' cy='5' r='2'/><circle cx='18' cy='5' r='2'/><circle cx='6' cy='19' r='2'/><path d='M6 7v10m12-10v3c0 4-12 0-12 6'/>"
            }
            "computer" => {
                "<rect x='3' y='4' width='18' height='13' rx='1.5'/><path d='M8 21h8m-4-4v4'/>"
            }
            "cloud" => "<path d='M6 18a4 4 0 0 1-.8-7.9 7 7 0 0 1 13.5-1A4.5 4.5 0 0 1 18.5 18Z'/>",
            "head" => {
                "<circle cx='12' cy='12' r='7'/><circle cx='12' cy='12' r='2'/><path d='M12 2v4m0 12v4M2 12h4m12 0h4'/>"
            }
            "graph" => {
                "<circle cx='6' cy='4' r='2'/><circle cx='6' cy='20' r='2'/><circle cx='18' cy='12' r='2'/><path d='M6 6v12m0-10c8 0 12 0 12 2'/>"
            }
            "list" => "<path d='M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01'/>",
            "folder" => "<path d='M3 6h7l2 3h9v11H3zM3 6V4h7l2 2h8v3'/>",
            "terminal" => {
                "<rect x='2' y='4' width='20' height='16' rx='2'/><path d='m6 9 3 3-3 3m6 1h6'/>"
            }
            "wrap" => "<path d='M3 5h18M3 10h13a4 4 0 0 1 0 8h-5m3-3-3 3 3 3M3 15h4'/>",
            "checkout" => "<path d='M4 4h9v16H4m5-8h12m-4-4 4 4-4 4'/>",
            "commit" => "<path d='M3 12h5m8 0h5'/><circle cx='12' cy='12' r='4'/>",
            "tag" => "<path d='M3 3h8l10 10-8 8L3 11Z'/><circle cx='7.5' cy='7.5' r='1'/>",
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
        .text_color(rgb(crate::desktop_theme::TEXT))
}
