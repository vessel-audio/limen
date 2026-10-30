//! Shared motion vocabulary so every transition in the window feels alike.

use std::time::Duration;

use gpui_kit::*;

/// Slightly under-damped spring for elements that follow the selection.
pub const SPRING: SpringConfig = SpringConfig::new(420., 38., 1.);

/// One-shot entrance used when content first appears.
pub fn enter() -> Animation {
    Animation::new(Duration::from_millis(240)).with_easing(ease_out_quint())
}

/// Endless breathing used to show that work is in progress.
pub fn pulse() -> Animation {
    Animation::new(Duration::from_millis(1600))
        .repeat()
        .with_easing(pulsating_between(0.35, 1.))
}
