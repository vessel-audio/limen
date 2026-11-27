//! Application palette applied after each light/dark theme transition. The
//! neutrals are drawn for the default accent's warm cream and take on the
//! chosen accent's temperature.

use gpui_kit::component::Theme;
use gpui_kit::*;

use crate::settings::Accent;

pub fn apply(accent: Accent, cx: &mut App) {
    let (primary, deep) = accent.shades();
    Theme::update(cx, |theme| {
        let dark = theme.is_dark();
        let [
            background,
            foreground,
            muted,
            muted_foreground,
            hover,
            border,
            shade,
        ] = match dark {
            false => [
                0xfaf9f5, 0x141413, 0xf5f0e8, 0x6c6a64, 0xefe9de, 0xe6dfd8, 0x141413,
            ],
            true => [
                0x181715, 0xfaf9f5, 0x252320, 0xa09d96, 0x252320, 0x3d3d3a, 0x141413,
            ],
        }
        .map(|neutral| accent.neutral(rgb(neutral).into()));
        theme.colors.background = background;
        theme.colors.foreground = foreground;
        theme.colors.muted = muted;
        theme.colors.muted_foreground = muted_foreground;
        theme.colors.accent = hover;
        theme.colors.accent_foreground = foreground;
        theme.colors.border = border;
        // The page behind a dialog dims well back, so the dialog reads as
        // the one thing to answer.
        theme.colors.overlay = shade.opacity(match dark {
            false => 0.32,
            true => 0.55,
        });
        theme.colors.title_bar = background;
        theme.colors.title_bar_border = border;
        theme.colors.primary = primary;
        theme.colors.primary_foreground = accent.foreground();
        theme.colors.button_primary = primary;
        theme.colors.button_primary_hover = deep;
        theme.colors.button_primary_active = deep;
        theme.colors.button_primary_foreground = accent.foreground();
        theme.colors.button = background;
        theme.colors.button_foreground = foreground;
        theme.colors.button_hover = hover;
        theme.colors.button_active = hover;
        theme.colors.success = rgb(0x5db872).into();
        theme.colors.warning = rgb(0xd4a017).into();
        theme.colors.danger = rgb(0xc64545).into();
    });
}
