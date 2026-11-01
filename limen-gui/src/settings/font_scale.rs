//! Interface text size as a scale of the base size.

use gpui_kit::*;

use super::segmented::{Segment, segmented};
use crate::app::AppView;

/// Interface text size at 100%.
const BASE_SIZE: f32 = 16.;

/// Interface text size as a percentage of the base size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontScale(u16);

impl FontScale {
    pub const DEFAULT: Self = Self(100);
    const ALL: [Self; 4] = [Self(100), Self(110), Self(125), Self(150)];

    pub fn font_size(self) -> Pixels {
        px(BASE_SIZE * f32::from(self.0) / 100.)
    }
}

/// The scales on a segmented control.
pub fn picker(view: &AppView, cx: &mut Context<AppView>) -> Stateful<Div> {
    segmented(
        "font-scale",
        rust_i18n::t!("settings.font_size").to_string(),
        rems(4.),
        FontScale::ALL
            .map(|scale| Segment {
                id: format!("font-scale-{}", scale.0).into(),
                label: format!("{}%", scale.0),
                value: scale,
            })
            .into(),
        view.font_scale,
        |this, scale, _, cx| {
            this.font_scale = scale;
            this.apply_typography(cx);
        },
        cx,
    )
}
