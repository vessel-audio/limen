//! The accent colour of primary buttons, highlights, and the brand mark:
//! a preset swatch, or a custom colour picked in the panel behind the plus
//! button (see `custom_accent`), which then joins the swatches.

use gpui_kit::accesskit::Toggled;
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, StyledExt};
use gpui_kit::*;

use crate::app::AppView;

/// A preset accent. Each is dark enough for white button text and stays
/// clear of the green and red that report success and failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Clay,
    Ocean,
    Iris,
    Rose,
    Teal,
    Graphite,
}

impl Preset {
    const ALL: [Self; 6] = [
        Self::Clay,
        Self::Ocean,
        Self::Iris,
        Self::Rose,
        Self::Teal,
        Self::Graphite,
    ];
}

/// The accent colour: a preset, or one picked in the custom colour panel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Accent {
    Preset(Preset),
    Custom(Hsla),
}

impl Accent {
    pub const DEFAULT: Self = Self::Preset(Preset::Clay);

    /// The accent, and the deeper shade of a hovered or pressed button.
    pub fn shades(self) -> (Hsla, Hsla) {
        match self {
            Self::Preset(preset) => {
                let (base, deep) = match preset {
                    Preset::Clay => (0xcc785c, 0xa9583e),
                    Preset::Ocean => (0x3b7fc4, 0x2c6299),
                    Preset::Iris => (0x7f63c7, 0x654ba6),
                    Preset::Rose => (0xc2577f, 0x9f4064),
                    Preset::Teal => (0x2e8a8a, 0x226d6d),
                    Preset::Graphite => (0x6b6964, 0x52504c),
                };
                (rgb(base).into(), rgb(deep).into())
            }
            Self::Custom(color) => {
                let base = color.opacity(1.);
                (
                    base,
                    Hsla {
                        l: base.l * 0.8,
                        ..base
                    },
                )
            }
        }
    }

    /// The text on a primary button: white on a preset; on a custom
    /// colour, white or near-black, whichever contrasts more.
    pub fn foreground(self) -> Hsla {
        let (white, ink): (Hsla, Hsla) = (rgb(0xffffff).into(), rgb(0x141413).into());
        match self {
            Self::Preset(_) => white,
            Self::Custom(_) => {
                let base = luminance(self.shades().0);
                let on_white = (luminance(white) + 0.05) / (base + 0.05);
                let on_ink = (base + 0.05) / (luminance(ink) + 0.05);
                match on_white >= on_ink {
                    true => white,
                    false => ink,
                }
            }
        }
    }

    /// A neutral drawn for Clay's warm cream, tinted to sit with this
    /// accent: the accent's hue at a fraction of the saturation, at the
    /// same lightness, so text keeps its contrast. Graphite greys it, and a
    /// custom colour tints it as the presets do, less the greyer it is.
    pub fn neutral(self, color: Hsla) -> Hsla {
        let accent = self.shades().0;
        match self {
            Self::Preset(Preset::Clay) => color,
            Self::Preset(Preset::Ocean | Preset::Iris | Preset::Rose | Preset::Teal) => Hsla {
                h: accent.h,
                s: color.s * 0.6,
                ..color
            },
            Self::Preset(Preset::Graphite) => Hsla {
                s: color.s * 0.1,
                ..color
            },
            Self::Custom(_) => Hsla {
                h: accent.h,
                s: color.s * (accent.s * 1.2).min(0.6),
                ..color
            },
        }
    }
}

/// WCAG relative luminance.
fn luminance(color: Hsla) -> f32 {
    let Rgba { r, g, b, .. } = color.into();
    let linear = |channel: f32| match channel <= 0.04045 {
        true => channel / 12.92,
        false => ((channel + 0.055) / 1.055).powf(2.4),
    };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

impl AppView {
    pub(super) fn choose_accent(&mut self, accent: Accent, cx: &mut Context<Self>) {
        self.accent = accent;
        crate::palette::apply(accent, cx);
        cx.notify();
    }
}

/// The presets, then the custom colour once one is picked, as swatches
/// with the chosen one ringed, which screen readers see as a radio group
/// of the colours' names; then the plus button that opens the custom
/// colour panel.
pub fn picker(view: &AppView, cx: &mut Context<AppView>) -> Div {
    let (ring, hover) = (cx.theme().foreground, cx.theme().muted_foreground);
    let presets = Preset::ALL.map(|preset| {
        // The name in the swatch's ID and its locale key.
        let name = match preset {
            Preset::Clay => "clay",
            Preset::Ocean => "ocean",
            Preset::Iris => "iris",
            Preset::Rose => "rose",
            Preset::Teal => "teal",
            Preset::Graphite => "graphite",
        };
        let key = format!("settings.accent_{name}");
        (
            SharedString::from(format!("accent-{name}")),
            rust_i18n::t!(&key).to_string(),
            Accent::Preset(preset),
        )
    });
    let custom = view.custom_accent.map(|color| {
        (
            SharedString::from("accent-custom"),
            rust_i18n::t!("settings.accent_custom").to_string(),
            Accent::Custom(color),
        )
    });
    let swatches = presets.into_iter().chain(custom).collect::<Vec<_>>();
    let count = swatches.len();
    let group = div()
        .id("accent")
        .accessibility_id("accent")
        .role(Role::RadioGroup)
        .aria_label(rust_i18n::t!("settings.accent").to_string())
        .h_flex()
        .gap_1p5()
        .children(
            swatches
                .into_iter()
                .enumerate()
                .map(|(position, (id, label, accent))| {
                    let chosen = accent == view.accent;
                    let swatch = div()
                        .id(id.clone())
                        .test_support()
                        .accessibility_id(id)
                        .role(Role::RadioButton)
                        .aria_label(label.clone())
                        .aria_position_in_set(position + 1)
                        .aria_size_of_set(count)
                        .aria_toggled(match chosen {
                            true => Toggled::True,
                            false => Toggled::False,
                        })
                        .size(px(24.))
                        .p(px(2.))
                        .rounded_full()
                        .border_2()
                        .cursor_pointer()
                        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
                        .child(div().size_full().rounded_full().bg(accent.shades().0))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.choose_accent(accent, cx)),
                        );
                    match chosen {
                        true => swatch.border_color(ring),
                        false => swatch
                            .border_color(transparent_black())
                            .hover(|style| style.border_color(hover)),
                    }
                }),
        );
    div()
        .h_flex()
        .items_center()
        .gap_1p5()
        .child(group)
        .child(super::custom_accent::button(view, cx))
}
