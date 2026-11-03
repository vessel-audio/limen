//! Shared motion vocabulary so every transition in the window feels alike.

use std::time::Duration;

use gpui_kit::*;

/// Slightly under-damped spring for elements that follow the selection.
pub const SPRING: SpringConfig = SpringConfig::new(420., 38., 1.);

/// One-shot entrance used when content first appears.
pub fn enter() -> Animation {
    Animation::new(Duration::from_millis(240)).with_easing(ease_out_quint())
}

/// The entrance of the `index`th item of a group, which starts a beat after
/// the one before it so the group arrives as a wave.
pub fn stagger(index: usize) -> Animation {
    const STEP: u64 = 45;
    let delay = STEP * index as u64;
    let total = 240 + delay;
    let start = delay as f32 / total as f32;
    let ease = ease_out_quint();
    Animation::new(Duration::from_millis(total))
        .with_easing(move |t| ease(((t - start) / (1. - start)).clamp(0., 1.)))
}

/// Endless breathing used to show that work is in progress.
///
/// Each frame re-renders the whole `AppView`, so the slow fade is capped well
/// below the display rate.
pub fn pulse() -> Animation {
    Animation::new(Duration::from_millis(1600))
        .repeat()
        .with_easing(pulsating_between(0.35, 1.))
        .with_max_fps(20.)
}
