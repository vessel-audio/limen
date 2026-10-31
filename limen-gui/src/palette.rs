//! Application palette applied after each light/dark theme transition.

use gpui_kit::component::Theme;
use gpui_kit::*;

pub fn apply(cx: &mut App) {
    Theme::update(cx, |theme| {
        let dark = theme.is_dark();
        let (background, foreground, muted, muted_foreground, accent, border) = match dark {
            false => (0xfaf9f5, 0x141413, 0xf5f0e8, 0x6c6a64, 0xefe9de, 0xe6dfd8),
            true => (0x181715, 0xfaf9f5, 0x252320, 0xa09d96, 0x252320, 0x3d3d3a),
        };
        theme.colors.background = rgb(background).into();
        theme.colors.foreground = rgb(foreground).into();
        theme.colors.muted = rgb(muted).into();
        theme.colors.muted_foreground = rgb(muted_foreground).into();
        theme.colors.accent = rgb(accent).into();
        theme.colors.accent_foreground = rgb(foreground).into();
        theme.colors.border = rgb(border).into();
        theme.colors.title_bar = rgb(background).into();
        theme.colors.title_bar_border = rgb(border).into();
        theme.colors.primary = rgb(0xcc785c).into();
        theme.colors.primary_foreground = rgb(0xffffff).into();
        theme.colors.button_primary = rgb(0xcc785c).into();
        theme.colors.button_primary_hover = rgb(0xa9583e).into();
        theme.colors.button_primary_active = rgb(0xa9583e).into();
        theme.colors.button_primary_foreground = rgb(0xffffff).into();
        theme.colors.button = rgb(background).into();
        theme.colors.button_foreground = rgb(foreground).into();
        theme.colors.button_hover = rgb(accent).into();
        theme.colors.button_active = rgb(accent).into();
        theme.colors.success = rgb(0x5db872).into();
        theme.colors.warning = rgb(0xd4a017).into();
        theme.colors.danger = rgb(0xc64545).into();
    });
}
