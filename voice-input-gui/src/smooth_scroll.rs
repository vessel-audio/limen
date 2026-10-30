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
/// Track [`Self::handle`] on an `overflow_y_scroll` container, attach
/// [`Self::on_scroll`] to the container's single content child, and call
/// [`Self::step`] on every render.
#[derive(Clone, Default)]
pub struct SmoothScroll {
    handle: ScrollHandle,
    glide: Rc<RefCell<Option<Glide>>>,
}

/// An eased scroll still on its way to `target`.
#[derive(Clone, Copy)]
struct Glide {
    target: Pixels,
    last_frame: Instant,
}

impl SmoothScroll {
    pub fn handle(&self) -> &ScrollHandle {
        &self.handle
    }

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
        let Some(Glide { target, last_frame }) = *glide else {
            return;
        };
        let now = Instant::now();
        let offset = self.handle.offset();
        let progress = 1. - (-(now - last_frame).as_secs_f32() / TIME_CONSTANT).exp();
        let next = offset.y + (target - offset.y) * progress;
        match (target - next).abs() < px(0.5) {
            true => {
                *glide = None;
                self.handle.set_offset(point(offset.x, target));
            }
            false => {
                *glide = Some(Glide {
                    target,
                    last_frame: now,
                });
                self.handle.set_offset(point(offset.x, next));
                window.request_animation_frame();
            }
        }
    }

    /// A vertical scrollbar overlay for the container, shown per `mode`.
    pub fn scrollbar(&self, mode: ScrollbarMode) -> Div {
        div().absolute().inset_0().child(
            Scrollbar::vertical(&self.handle)
                .mode(mode)
                .viewport_from_layout(),
        )
    }
}
