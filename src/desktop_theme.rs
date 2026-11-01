//! Semantic palettes shared by custom elements and GPUI components.
//! Palette lookup stays on the rendering thread; it never locks or does I/O.
use crate::appearance::ThemeChoice;
use gpui::{App, rgb, rgba};
use gpui_component::{Theme, ThemeMode};
use std::cell::Cell;

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: u32,
    pub input_bg: u32,
    pub panel: u32,
    pub control: u32,
    pub hover: u32,
    pub line: u32,
    pub text: u32,
    pub muted: u32,
    pub inspector_line: u32,
    pub inspector_muted: u32,
    pub accent: u32,
    pub accent_hover: u32,
    pub selected: u32,
    pub selected_hover: u32,
    pub selection: u32,
    pub search_text: u32,
    pub search_bg: u32,
    pub filter_active_border: u32,
    pub filter_active_bg: u32,
    pub remote_text: u32,
    pub remote_bg: u32,
    pub added: u32,
    pub removed: u32,
    pub added_bg: u32,
    pub removed_bg: u32,
    pub added_highlight: u32,
    pub removed_highlight: u32,
    pub syntax_colors: [u32; 19],
    pub colors: [u32; 10],
    pub on_accent: u32,
    pub path_bg: u32,
    pub pending_line: u32,
    pub scroll_thumb: u32,
    pub pending_bg: u32,
    pub pending_selected: u32,
    pub warning: u32,
    pub success: u32,
    pub caution: u32,
    pub danger: u32,
    pub primary_active: u32,
}

pub const BLACK: Palette = {
    Palette {
        bg: 0x000000,
        input_bg: 0x000000,
        panel: 0x0c0c0c,
        control: 0x191919,
        hover: 0x24201d,
        line: 0x303030,
        text: 0xeee9e4,
        muted: 0xa39b94,
        inspector_line: 0x1c1c1c,
        inspector_muted: 0x999999,
        accent: 0xff914d,
        accent_hover: 0xffad75,
        selected: 0x301c10,
        selected_hover: 0x402717,
        selection: 0xff914d55,
        search_text: 0xffbc87,
        search_bg: 0xff914d33,
        filter_active_border: 0x99552e,
        filter_active_bg: 0x1c1008,
        remote_text: 0x8ccaff,
        remote_bg: 0x11283c,
        added: 0x98c99a,
        removed: 0xed8c86,
        added_bg: 0x0e1c12,
        removed_bg: 0x251211,
        added_highlight: 0x70b87844,
        removed_highlight: 0xed8c8644,
        syntax_colors: [
            0x929da8, 0xa8d59b, 0xc7a0e8, 0x89c9ee, 0xe8cc91, 0xeab38b, 0xeab38b, 0xa6c9ee,
            0xc9c2bb, 0xb9b2ac, 0xeee9e4, 0xefad94, 0xe8cc91, 0xe8cc91, 0xeab38b, 0xeee9e4,
            0x89c9ee, 0x89c9ee, 0xa8d59b,
        ],
        colors: [
            0xff914d, 0x66d9ef, 0xc792ff, 0xa6e36d, 0xff6f91, 0xffd866, 0x6c9fff, 0x5de0b5,
            0xf184df, 0xd9e1e8,
        ],
        on_accent: 0x000000,
        path_bg: 0x674125,
        pending_line: 0x996039,
        scroll_thumb: 0x514941,
        pending_bg: 0x241508,
        pending_selected: 0x4a2b15,
        warning: 0xe5a04b,
        success: 0x38975c,
        caution: 0x90a83d,
        danger: 0xcf4949,
        primary_active: 0xe57838,
    }
};

// Charcoal surfaces and Dark+ syntax colors, with Rustle's orange accents.
pub const DARK: Palette = Palette {
    bg: 0x1e1e1e,
    input_bg: 0x121212,
    panel: 0x252526,
    control: 0x333333,
    hover: 0x2d2926,
    line: 0x454545,
    text: 0xd4d4d4,
    muted: 0xaaaaaa,
    inspector_line: 0x3c3c3c,
    inspector_muted: 0xaaaaaa,
    accent: 0xff914d,
    accent_hover: 0xffad75,
    selected: 0x3b2a20,
    selected_hover: 0x4a3325,
    selection: 0xff914d55,
    search_text: 0xffcc80,
    search_bg: 0xea9f3544,
    filter_active_border: 0xbb7545,
    filter_active_bg: 0x241b16,
    remote_text: 0x9cdcfe,
    remote_bg: 0x203b50,
    added: 0xb5cea8,
    removed: 0xf0a5a5,
    added_bg: 0x203024,
    removed_bg: 0x372326,
    added_highlight: 0x89d18533,
    removed_highlight: 0xf4877133,
    syntax_colors: [
        0x91ad80, 0xce9178, 0xc586c0, 0xdcdcaa, 0x4ec9b0, 0xb5cea8, 0x9cdcfe, 0x9cdcfe, 0xd4d4d4,
        0xd4d4d4, 0x9cdcfe, 0x75b4e8, 0x9cdcfe, 0xdcdcaa, 0x75b4e8, 0xd4d4d4, 0x75b4e8, 0x75beff,
        0xce9178,
    ],
    colors: [
        0xff914d, 0x4ec9b0, 0xc586c0, 0xb5cea8, 0xf0a5a5, 0xdcdcaa, 0x9cdcfe, 0x89d185, 0xd7a6dc,
        0xd4d4d4,
    ],
    on_accent: 0x24160e,
    path_bg: 0x69462f,
    pending_line: 0xa86a40,
    scroll_thumb: 0x686868,
    pending_bg: 0x2d241f,
    pending_selected: 0x493123,
    warning: 0xe5c07b,
    success: 0x89d185,
    caution: 0xdcdcaa,
    danger: 0xf48771,
    primary_active: 0xe57838,
};

pub const LIGHT: Palette = {
    Palette {
        bg: 0xfaf9f6,
        input_bg: 0xfaf9f6,
        panel: 0xf2f0eb,
        control: 0xeae7e1,
        hover: 0xeee9e1,
        line: 0xd5d0c7,
        text: 0x292724,
        muted: 0x69635b,
        inspector_line: 0xe1ddd5,
        inspector_muted: 0x686157,
        accent: 0xa84612,
        accent_hover: 0x87350c,
        selected: 0xf7e5d5,
        selected_hover: 0xefdbc9,
        selection: 0xce6b2838,
        search_text: 0x77320c,
        search_bg: 0xf3be6555,
        filter_active_border: 0xb76b35,
        filter_active_bg: 0xf9eadc,
        remote_text: 0x24618a,
        remote_bg: 0xe2edf5,
        added: 0x29683d,
        removed: 0xac3439,
        added_bg: 0xe8f3e8,
        removed_bg: 0xfbe9e7,
        added_highlight: 0x64ac6d44,
        removed_highlight: 0xdd777044,
        syntax_colors: [
            0x586352, 0x38683b, 0x79429e, 0x14627f, 0x78570c, 0x954614, 0x954614, 0x315e97,
            0x524b43, 0x655c51, 0x292724, 0x9a3f39, 0x78570c, 0x78570c, 0x954614, 0x292724,
            0x14627f, 0x14627f, 0x38683b,
        ],
        colors: [
            0xa84612, 0x08798c, 0x8450b4, 0x4c7e25, 0xbb3d67, 0x8b6b06, 0x396dbe, 0x168063,
            0xa34294, 0x687784,
        ],
        on_accent: 0xffffff,
        path_bg: 0xf2d5b9,
        pending_line: 0xbb7945,
        scroll_thumb: 0xafa79b,
        pending_bg: 0xfcf0e2,
        pending_selected: 0xf5dfc6,
        warning: 0x8c5a12,
        success: 0x29683d,
        caution: 0x626b20,
        danger: 0xac3439,
        primary_active: 0x74300e,
    }
};

thread_local! { static THEME: Cell<ThemeChoice> = const { Cell::new(ThemeChoice::Black) }; }

#[inline]
pub fn current_theme() -> ThemeChoice {
    THEME.get()
}

#[inline]
pub fn palette() -> &'static Palette {
    match current_theme() {
        ThemeChoice::Black => &BLACK,
        ThemeChoice::Dark => &DARK,
        ThemeChoice::Light => &LIGHT,
    }
}

pub fn apply(choice: ThemeChoice, cx: &mut App) {
    THEME.set(choice);
    Theme::change(
        if choice == ThemeChoice::Light {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        },
        None,
        cx,
    );
    let p = palette();
    let theme = Theme::global_mut(cx);
    theme.background = rgb(p.bg).into();
    theme.foreground = rgb(p.text).into();
    theme.border = rgb(p.line).into();
    theme.input = rgb(p.line).into();
    theme.caret = rgb(p.accent).into();
    theme.ring = rgb(p.accent).into();
    theme.selection = rgba(p.selection).into();
    theme.primary = rgb(p.accent).into();
    theme.primary_hover = rgb(p.accent_hover).into();
    theme.primary_active = rgb(p.primary_active).into();
    theme.primary_foreground = rgb(p.on_accent).into();
    theme.accent = rgb(p.selected).into();
    theme.accent_foreground = rgb(p.accent_hover).into();
    theme.secondary = rgb(p.control).into();
    theme.secondary_hover = rgb(p.hover).into();
    theme.secondary_active = rgb(p.selected).into();
    theme.secondary_foreground = rgb(p.text).into();
    theme.muted = rgb(p.control).into();
    theme.muted_foreground = rgb(p.muted).into();
    theme.popover = rgb(p.panel).into();
    theme.popover_foreground = rgb(p.text).into();
    theme.scrollbar = rgb(p.bg).into();
    theme.scrollbar_thumb = rgb(p.scroll_thumb).into();
    theme.scrollbar_thumb_hover = rgb(p.accent).into();
    theme.list = rgb(p.bg).into();
    theme.list_hover = rgb(p.hover).into();
    theme.list_active = rgb(p.selected).into();
    theme.list_active_border = rgb(p.accent).into();
    theme.title_bar = rgb(p.bg).into();
    theme.title_bar_border = rgb(p.line).into();
    theme.link = rgb(p.accent).into();
    theme.link_hover = rgb(p.accent_hover).into();
    theme.link_active = rgb(p.accent).into();
    theme.switch = rgb(p.line).into();
    theme.switch_thumb = rgb(0xffffff).into();
    // Code-editor inputs have their own surface, separate from normal inputs.
    let editor = &mut std::sync::Arc::make_mut(&mut theme.highlight_theme).style;
    editor.editor_background = Some(rgb(p.input_bg).into());
    editor.editor_foreground = Some(rgb(p.text).into());
    editor.editor_active_line = Some(rgb(p.hover).into());
    editor.editor_line_number = Some(rgb(p.muted).into());
    editor.editor_active_line_number = Some(rgb(p.accent).into());
    cx.refresh_windows();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(color: u32) -> f64 {
        let channel = |shift: u32| {
            let c = ((color >> shift) & 255u32) as f64 / 255.;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        channel(16) * 0.2126 + channel(8) * 0.7152 + channel(0) * 0.0722
    }

    fn contrast(foreground: u32, background: u32) -> f64 {
        let (a, b) = (luminance(foreground), luminance(background));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn light_text_and_statuses_remain_readable_on_their_surfaces() {
        assert_readable(LIGHT);
    }

    #[test]
    fn dark_text_and_syntax_remain_readable_on_their_surfaces() {
        assert_readable(DARK);
    }

    fn assert_readable(p: Palette) {
        for background in [
            p.bg,
            p.input_bg,
            p.panel,
            p.control,
            p.hover,
            p.selected,
            p.pending_bg,
            p.pending_selected,
        ] {
            for foreground in [
                p.text,
                p.muted,
                p.inspector_muted,
                p.accent,
                p.added,
                p.removed,
                p.warning,
            ] {
                assert!(
                    contrast(foreground, background) >= 4.5,
                    "{foreground:06x} on {background:06x}"
                );
            }
        }
        assert!(contrast(p.on_accent, p.accent) >= 4.5);
        assert!(contrast(p.remote_text, p.remote_bg) >= 4.5);
        for foreground in p.syntax_colors {
            for background in [p.bg, p.added_bg, p.removed_bg] {
                assert!(
                    contrast(foreground, background) >= 4.5,
                    "syntax {foreground:06x} on {background:06x}"
                );
            }
        }
    }
}
