//! OLED-black surfaces and warm Rust-orange accents shared by the native UI.
use gpui::{App, rgb, rgba};
use gpui_component::{Theme, ThemeMode};

pub const BG: u32 = 0x000000;
pub const PANEL: u32 = 0x0c0c0c;
pub const CONTROL: u32 = 0x191919;
pub const HOVER: u32 = 0x24201d;
pub const LINE: u32 = 0x303030;
pub const TEXT: u32 = 0xeee9e4;
pub const MUTED: u32 = 0xa39b94;
pub const INSPECTOR_LINE: u32 = 0x1c1c1c;
pub const INSPECTOR_MUTED: u32 = 0x999999;
pub const ACCENT: u32 = 0xff914d;
pub const ACCENT_HOVER: u32 = 0xffad75;
pub const SELECTED: u32 = 0x301c10;
pub const SELECTED_HOVER: u32 = 0x402717;
pub const SELECTION: u32 = 0xff914d55;
pub const SEARCH_TEXT: u32 = 0xffbc87;
pub const SEARCH_BG: u32 = 0xff914d33;
pub const REMOTE_TEXT: u32 = 0x8ccaff;
pub const REMOTE_BG: u32 = 0x11283c;
pub const ADDED: u32 = 0x98c99a;
pub const REMOVED: u32 = 0xed8c86;
pub const ADDED_BG: u32 = 0x0e1c12;
pub const REMOVED_BG: u32 = 0x251211;
pub const ADDED_HIGHLIGHT: u32 = 0x70b87844;
pub const REMOVED_HIGHLIGHT: u32 = 0xed8c8644;

// Matches diff::syntax's capture categories. Foregrounds stay readable on both
// added/removed backgrounds; syntax never overrides selection or intraline fills.
pub const SYNTAX_COLORS: [u32; 19] = [
    0x929da8, 0xa8d59b, 0xc7a0e8, 0x89c9ee, 0xe8cc91, 0xeab38b, 0xeab38b, 0xa6c9ee, 0xc9c2bb,
    0xb9b2ac, TEXT, 0xefad94, 0xe8cc91, 0xe8cc91, 0xeab38b, TEXT, 0x89c9ee, 0x89c9ee, 0xa8d59b,
];

// Orange anchors the UI; graph lanes use separated hues for branch tracking.
pub const COLORS: [u32; 10] = [
    ACCENT, 0x66d9ef, 0xc792ff, 0xa6e36d, 0xff6f91, 0xffd866, 0x6c9fff, 0x5de0b5, 0xf184df,
    0xd9e1e8,
];

pub fn apply(cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);
    let theme = Theme::global_mut(cx);
    // Inputs, tooltips and scrollbars read the component theme independently of
    // our custom elements. Set these once, before constructing the window.
    theme.background = rgb(BG).into();
    theme.foreground = rgb(TEXT).into();
    theme.border = rgb(LINE).into();
    theme.input = rgb(LINE).into();
    theme.caret = rgb(ACCENT).into();
    theme.ring = rgb(ACCENT).into();
    theme.selection = rgba(SELECTION).into();
    theme.primary = rgb(ACCENT).into();
    theme.primary_hover = rgb(ACCENT_HOVER).into();
    theme.primary_active = rgb(0xe57838).into();
    theme.primary_foreground = rgb(BG).into();
    theme.accent = rgb(SELECTED).into();
    theme.accent_foreground = rgb(ACCENT_HOVER).into();
    theme.secondary = rgb(CONTROL).into();
    theme.secondary_hover = rgb(HOVER).into();
    theme.secondary_active = rgb(SELECTED).into();
    theme.secondary_foreground = rgb(TEXT).into();
    theme.muted = rgb(CONTROL).into();
    theme.muted_foreground = rgb(MUTED).into();
    theme.popover = rgb(PANEL).into();
    theme.popover_foreground = rgb(TEXT).into();
    theme.scrollbar = rgb(BG).into();
    theme.scrollbar_thumb = rgb(0x514941).into();
    theme.scrollbar_thumb_hover = rgb(ACCENT).into();
    theme.list = rgb(BG).into();
    theme.list_hover = rgb(HOVER).into();
    theme.list_active = rgb(SELECTED).into();
    theme.list_active_border = rgb(ACCENT).into();
    theme.title_bar = rgb(BG).into();
    theme.title_bar_border = rgb(LINE).into();
    theme.link = rgb(ACCENT).into();
    theme.link_hover = rgb(ACCENT_HOVER).into();
    theme.link_active = rgb(ACCENT).into();
}
