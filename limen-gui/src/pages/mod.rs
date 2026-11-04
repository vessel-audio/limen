//! GPUI pages rendered from the pure GUI model.

use gpui_kit::component::{ActiveTheme, StyledExt};
use gpui_kit::*;

mod dictation;
mod models;
mod shortcuts;
mod takes;
mod transcript;
mod usage;
pub use dictation::dictation;
pub use models::{ModelForms, follow as follow_models, providers};
pub use shortcuts::shortcuts;
pub use takes::RequestKey;
pub use transcript::{TranscriptState, follow as follow_transcript};
pub use usage::{UsageState, follow as follow_usage, usage};

/// Serif page heading shared by every non-dictation page.
pub fn title(text: impl Into<SharedString>) -> Stateful<Div> {
    let text = text.into();
    div()
        .id("page-title")
        .accessibility_id("page-title")
        .role(Role::Heading)
        .aria_label(text.clone())
        .text_xl()
        .font_family("serif")
        .child(text)
}

/// A dictation page section's heading row: its title, with room on the
/// right for what callers append.
fn section_header(title: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .h_flex()
        .h(px(24.))
        .items_center()
        .justify_between()
        .gap_4()
        .px_1()
        .child(
            div()
                .flex_shrink_0()
                .text_sm()
                .font_medium()
                .text_color(cx.theme().muted_foreground)
                .child(title.into()),
        )
}

fn format_time(at_ms: u64) -> String {
    let seconds = (at_ms / 1000) as libc::time_t;
    let mut broken_down = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: localtime_r initializes `broken_down` when it returns a non-null
    // pointer. The input points to a plain integer and the output is POD.
    let result = unsafe { libc::localtime_r(&seconds, broken_down.as_mut_ptr()) };
    if result.is_null() {
        return "--:--:--".to_string();
    }
    // SAFETY: the successful call initialized every field used here.
    let broken_down = unsafe { broken_down.assume_init() };
    format!(
        "{:02}:{:02}:{:02}",
        broken_down.tm_hour, broken_down.tm_min, broken_down.tm_sec
    )
}

fn elapsed(since_ms: u64) -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    now.saturating_sub(since_ms) / 1000
}
