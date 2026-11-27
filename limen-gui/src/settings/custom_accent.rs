//! The custom accent: a plus button after the swatches opens a panel,
//! right-aligned beneath it with an arrow pointing back at it, holding a
//! preview, a hex field, and hue, saturation, and lightness sliders. Every
//! colour committed there becomes the accent.

use std::f32::consts::FRAC_PI_4;

use gpui_kit::assets::IconName;
use gpui_kit::component::color_picker::{ColorPickerEvent, ColorPickerState};
use gpui_kit::component::input::Input;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::slider::{Slider, SliderState};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, StyledExt};
use gpui_kit::*;

use super::accent::Accent;
use crate::a11y;
use crate::app::AppView;

/// Bands in a slider's track, enough for the hue and lightness tracks to
/// read as smooth.
const TRACK_STEPS: usize = 96;

/// The panel's state, whose every committed colour, from a slider drag or
/// a hex entered, becomes the accent and its swatch.
pub fn custom_picker(
    window: &mut Window,
    cx: &mut Context<AppView>,
) -> (Entity<ColorPickerState>, Subscription) {
    let state = cx.new(|cx| ColorPickerState::new(window, cx));
    let subscription = cx.subscribe_in(
        &state,
        window,
        |this, _, event: &ColorPickerEvent, _, cx| match event {
            ColorPickerEvent::Change(Some(color)) => {
                this.custom_accent = Some(*color);
                this.choose_accent(Accent::Custom(*color), cx);
            }
            ColorPickerEvent::Change(None) => {}
        },
    );
    (state, subscription)
}

/// The plus button and the panel it opens. While open, the button is ringed
/// as selected and its plus turns into a cross.
pub fn button(view: &AppView, cx: &mut Context<AppView>) -> Popover {
    let state = view.accent_picker.clone();
    let open = state.read(cx).is_open();
    let current = view.accent.shades().0;
    let label = rust_i18n::t!("settings.accent_pick").to_string();
    let popover = Popover::new("accent-panel")
        .anchor(Anchor::TopRight)
        .arrow(true)
        .w_72()
        .open(open)
        .on_open_change({
            let state = state.clone();
            move |open, window, cx| {
                state.update(cx, |state, cx| {
                    // Each opening starts from the accent in use.
                    if *open {
                        state.set_value(current, window, cx);
                    }
                    state.set_open(*open, cx);
                })
            }
        })
        .trigger(
            a11y::button("accent-custom-pick")
                .outline()
                .small()
                .rounded(px(12.))
                .icon(Icon::new(IconName::Plus).rotate(radians(match open {
                    true => FRAC_PI_4,
                    false => 0.,
                })))
                .tooltip(label.clone())
                .accessibility_label(label),
        );
    match open {
        true => popover.child(panel(&state, cx)),
        false => popover,
    }
}

/// The colour being picked beside its hex, above its component sliders.
fn panel(state: &Entity<ColorPickerState>, cx: &App) -> Div {
    let picker = state.read(cx);
    let color = picker.displayed_color().unwrap_or_default();
    let sliders = picker.sliders().clone();
    let muted = cx.theme().muted_foreground;
    let row = |key: &str, slider: &Entity<SliderState>, track: Vec<Hsla>, value: f32| {
        div()
            .h_flex()
            .gap_2()
            .items_center()
            .child(
                div()
                    .w_16()
                    .text_xs()
                    .text_color(muted)
                    .child(rust_i18n::t!(key).to_string()),
            )
            .child(
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .flex_1()
                    .h_6()
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .h_2()
                            .rounded_full()
                            .overflow_hidden()
                            .h_flex()
                            .children(
                                track
                                    .into_iter()
                                    .map(|band| div().flex_1().h_full().bg(band)),
                            ),
                    )
                    .child(Slider::new(slider).flex_1().bg(transparent_black())),
            )
            .child(
                div()
                    .w_8()
                    .text_xs()
                    .text_color(muted)
                    .text_align(TextAlign::Right)
                    .child(format!("{value:.0}")),
            )
    };
    div()
        .v_flex()
        .gap_3()
        .child(
            div()
                .h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .size_7()
                        .flex_shrink_0()
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(color),
                )
                .child(Input::new(picker.hex_input()).small()),
        )
        .child(row(
            "settings.accent_hue",
            sliders.hue(),
            steps(|hue| hsla(hue, 1., 0.5, 1.)),
            color.h * 360.,
        ))
        .child(row(
            "settings.accent_saturation",
            sliders.saturation(),
            steps(|saturation| hsla(color.h, saturation, color.l, 1.)),
            color.s * 100.,
        ))
        .child(row(
            "settings.accent_lightness",
            sliders.lightness(),
            steps(|lightness| hsla(color.h, color.s, lightness, 1.)),
            color.l * 100.,
        ))
}

/// A track's bands, painted from 0 to 1 along it.
fn steps(paint: impl Fn(f32) -> Hsla) -> Vec<Hsla> {
    (0..TRACK_STEPS)
        .map(|step| paint(step as f32 / (TRACK_STEPS - 1) as f32))
        .collect()
}
