//! Eased wheel scrolling for scroll containers.
//!
//! gpui moves a container by whole lines per wheel notch, which reads as a
//! jump. A [`SmoothScroll`] moves a target per notch instead and eases the
//! offset toward it on each frame. Precise touchpad deltas already arrive in
//! small steps and apply directly, as does every delta in lightweight mode.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::*;

/// Seconds for the offset to cover about two thirds of the remaining way.
const TIME_CONSTANT: f32 = 0.07;

/// A scroll handle whose wheel input glides instead of jumping.
///
/// Wrap the scroll area with [`Self::container`], attach [`Self::on_scroll`]
/// to the area's single content child, and call [`Self::step`] on every
/// render.
#[derive(Clone, Default)]
pub struct SmoothScroll {
    handle: ScrollHandle,
    glide: Rc<RefCell<Option<Glide>>>,
}

/// An eased scroll still on its way to `target`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Glide {
    target: Pixels,
    /// The offset this glide last wrote; any other offset means something
    /// else, such as the scrollbar or a layout change, moved the container.
    written: Pixels,
    last_frame: Instant,
}

/// What one frame does to a glide.
#[derive(Debug, PartialEq)]
enum Frame {
    /// Keep easing: write the offset and ask for another frame.
    Move(Glide),
    /// Arrived: write the final offset and stop.
    Settle(Pixels),
    /// The offset moved under the glide, which yields to that move.
    Abandon,
}

/// Ease `glide` toward its target as of `now`, keeping the target inside the
/// current range `[-max_offset, 0]` in case the content shrank meanwhile.
fn advance(glide: Glide, offset: Pixels, max_offset: Pixels, now: Instant) -> Frame {
    let target = glide.target.clamp(-max_offset, px(0.));
    let progress = 1. - (-(now - glide.last_frame).as_secs_f32() / TIME_CONSTANT).exp();
    let next = offset + (target - offset) * progress;
    match (
        (offset - glide.written).abs() < px(0.5),
        (target - next).abs() < px(0.5),
    ) {
        (false, _) => Frame::Abandon,
        (true, true) => Frame::Settle(target),
        (true, false) => Frame::Move(Glide {
            target,
            written: next,
            last_frame: now,
        }),
    }
}

impl SmoothScroll {
    /// Wheel listener for the content child. Children dispatch before their
    /// container, so stopping propagation here keeps gpui's own line jump
    /// from running.
    pub fn on_scroll(&self) -> impl Fn(&ScrollEvent, &mut Window, &mut App) + 'static {
        let handle = self.handle.clone();
        let glide = Rc::clone(&self.glide);
        move |event, window, cx| {
            cx.stop_propagation();
            let delta = event.delta.pixel_delta(window.line_height()).y;
            let offset = handle.offset();
            let clamp = |y: Pixels| y.clamp(-handle.max_offset().y, px(0.));
            let mut glide = glide.borrow_mut();
            match (event.delta.precise(), cx.reduce_motion()) {
                (true, _) | (false, true) => {
                    *glide = None;
                    handle.set_offset(point(offset.x, clamp(offset.y + delta)));
                }
                (false, false) => {
                    let from = glide.map_or(offset.y, |glide| glide.target);
                    *glide = Some(Glide {
                        target: clamp(from + delta),
                        written: offset.y,
                        last_frame: Instant::now(),
                    });
                }
            }
            window.refresh();
        }
    }

    /// Advance an ongoing glide by the time since the last frame.
    pub fn step(&self, window: &mut Window) {
        let mut glide = self.glide.borrow_mut();
        let Some(current) = *glide else {
            return;
        };
        let offset = self.handle.offset();
        match advance(
            current,
            offset.y,
            self.handle.max_offset().y,
            Instant::now(),
        ) {
            Frame::Move(next) => {
                *glide = Some(next);
                self.handle.set_offset(point(offset.x, next.written));
                window.request_animation_frame();
            }
            Frame::Settle(target) => {
                *glide = None;
                self.handle.set_offset(point(offset.x, target));
            }
            Frame::Abandon => *glide = None,
        }
    }

    /// Make `area` this handle's scroll container and overlay a vertical
    /// scrollbar shown per `mode`; callers size and style the returned wrapper.
    ///
    /// The scrollbar is the area's sibling: inside the area it would scroll
    /// away with the content and misreport the position.
    pub fn container(&self, area: Stateful<Div>, mode: ScrollbarMode) -> Div {
        div()
            .relative()
            .flex()
            .flex_col()
            .min_h_0()
            .child(
                area.flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.handle),
            )
            .child(
                div().absolute().inset_0().child(
                    Scrollbar::vertical(&self.handle)
                        .mode(mode)
                        .viewport_from_layout(),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use gpui_kit::px;

    use super::{Frame, Glide, TIME_CONSTANT, advance};

    fn glide(target: f32, written: f32, start: Instant) -> Glide {
        Glide {
            target: px(target),
            written: px(written),
            last_frame: start,
        }
    }

    #[test]
    fn a_frame_covers_the_eased_share_of_the_remaining_way() {
        let start = Instant::now();
        let now = start + Duration::from_secs_f32(TIME_CONSTANT);
        let Frame::Move(next) = advance(glide(-100., 0., start), px(0.), px(500.), now) else {
            panic!("a glide one time constant in should still be moving");
        };
        assert!((next.written - px(-63.2)).abs() < px(0.1), "{next:?}");
        assert_eq!((next.target, next.last_frame), (px(-100.), now));
    }

    #[test]
    fn a_target_beyond_a_shrunken_range_settles_at_its_end() {
        let start = Instant::now();
        let later = start + Duration::from_secs(1);
        assert_eq!(
            advance(glide(-400., -150., start), px(-150.), px(150.), later),
            Frame::Settle(px(-150.))
        );
    }

    #[test]
    fn an_offset_moved_by_something_else_ends_the_glide() {
        let start = Instant::now();
        assert_eq!(
            advance(glide(-300., -100., start), px(-40.), px(500.), start),
            Frame::Abandon
        );
    }
}
