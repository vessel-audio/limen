//! A segmented control: options side by side on a track, the chosen one
//! raised on a chip that springs between them. Screen readers see a radio
//! group.

use gpui_kit::accesskit::Toggled;
use gpui_kit::component::{ActiveTheme, StyledExt};
use gpui_kit::*;

use crate::app::AppView;
use crate::motion;

/// Space between the track's edge, the options, and the chip.
const INSET: f32 = 2.;

/// One option: its accessible ID, its label, and the value it chooses.
pub struct Segment<T> {
    pub id: SharedString,
    pub label: String,
    pub value: T,
}

/// A radio group named `label` of equally wide `segments`, with `selected`
/// chosen. Options are `width` wide so the chip's place follows from its
/// index; `select` runs when the user picks another.
pub fn segmented<T: Copy + PartialEq + 'static>(
    id: &'static str,
    label: String,
    width: Rems,
    segments: Vec<Segment<T>>,
    selected: T,
    select: impl Fn(&mut AppView, T, &mut Window, &mut Context<AppView>) + Clone + 'static,
    cx: &mut Context<AppView>,
) -> Stateful<Div> {
    let width = width.to_pixels(cx.theme().font_size);
    let index = segments
        .iter()
        .position(|segment| segment.value == selected)
        .unwrap_or_default();
    let (text, muted) = (cx.theme().foreground, cx.theme().muted_foreground);
    let chip = div()
        .absolute()
        .top(px(INSET))
        .bottom(px(INSET))
        .w(width)
        .rounded_md()
        .bg(cx.theme().background)
        .shadow_sm()
        .with_spring(
            SharedString::from(format!("{id}-chip")),
            SpringAnimation::new(motion::SPRING).to(px(INSET) + (width + px(INSET)) * index as f32),
            |chip, left| chip.left(left),
        );
    let count = segments.len();
    div()
        .id(id)
        .accessibility_id(id)
        .role(Role::RadioGroup)
        .aria_label(label)
        .relative()
        .h_flex()
        .p(px(INSET))
        .gap(px(INSET))
        .rounded_lg()
        .bg(cx.theme().muted)
        .child(chip)
        .children(segments.into_iter().enumerate().map(|(position, segment)| {
            let chosen = segment.value == selected;
            let option = div()
                .id(segment.id.clone())
                .accessibility_id(segment.id)
                .role(Role::RadioButton)
                .aria_label(segment.label.clone())
                .aria_position_in_set(position + 1)
                .aria_size_of_set(count)
                .aria_toggled(match chosen {
                    true => Toggled::True,
                    false => Toggled::False,
                })
                .w(width)
                .py_1()
                .flex()
                .justify_center()
                .whitespace_nowrap()
                .text_sm()
                .cursor_pointer()
                .child(segment.label)
                .on_click({
                    let (select, value) = (select.clone(), segment.value);
                    cx.listener(move |this, _, window, cx| select(this, value, window, cx))
                });
            match chosen {
                true => option.text_color(text),
                false => option
                    .text_color(muted)
                    .hover(|style| style.text_color(text)),
            }
        }))
}
