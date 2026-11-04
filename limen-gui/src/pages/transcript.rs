//! The newest transcript on the dictation page, revealed a few characters
//! at a time as if it were being written, with a button that copies it.
//!
//! A take is transcribed in one request, so its text arrives whole; the
//! reveal only paces how it appears. Polished text that replaces the
//! original keeps the part they share and reveals the rest.

use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme, Sizable, button::ButtonVariants};
use gpui_kit::*;
use limen_core::DictationState;

use crate::a11y;
use crate::app::AppView;
use crate::model::Toast;

/// How many characters appear each second.
const PACE: f32 = 45.;
/// The longest a reveal takes; longer text appears faster.
const LONGEST: Duration = Duration::from_millis(2500);
/// How tall the text grows before it scrolls.
const MAX_HEIGHT: f32 = 168.;

/// The transcript shown, and how far its reveal has come.
#[derive(Default)]
pub struct TranscriptState {
    /// A snapshot has been seen; until then, nothing new has arrived.
    primed: bool,
    shown: Option<Shown>,
    scroll: ScrollHandle,
}

/// One history entry's text: the polished one once there is one.
struct Shown {
    entry: u64,
    take: u64,
    text: String,
    /// Characters showing when the reveal began, and when it began; `None`
    /// once all of them show.
    reveal: Option<(usize, Instant)>,
}

/// How many of `total` characters show `elapsed` into a reveal that began
/// with `from` of them showing.
fn revealed(total: usize, from: usize, elapsed: Duration) -> usize {
    let pace = PACE.max((total - from) as f32 / LONGEST.as_secs_f32());
    (from + (elapsed.as_secs_f32() * pace).ceil() as usize).min(total)
}

/// How many leading characters `old` and `new` share.
fn shared(old: &str, new: &str) -> usize {
    old.chars()
        .zip(new.chars())
        .take_while(|(old, new)| old == new)
        .count()
}

impl Shown {
    /// How many characters show now.
    fn showing(&self, now: Instant) -> usize {
        let total = self.text.chars().count();
        match self.reveal {
            Some((from, started)) => revealed(total, from, now - started),
            None => total,
        }
    }
}

/// Follow the newest history entry. What the window opens on shows at once,
/// as does everything while motion is reduced; later text is revealed.
pub fn follow(view: &mut AppView, cx: &App) {
    let Some(snapshot) = &view.model.snapshot else {
        return;
    };
    let now = Instant::now();
    let state = &mut view.transcript;
    let latest = snapshot.state.history.last().map(|entry| {
        (
            entry.id,
            entry.take,
            entry.text.clone().unwrap_or_else(|| entry.original.clone()),
        )
    });
    let animate = state.primed && !cx.reduce_motion();
    state.primed = true;
    state.shown = match (state.shown.take(), latest) {
        (_, None) => None,
        (Some(shown), Some((entry, _, text))) if shown.entry == entry && shown.text == text => {
            let done = shown.showing(now) == text.chars().count();
            Some(Shown {
                reveal: shown.reveal.filter(|_| !done),
                ..shown
            })
        }
        (previous, Some((entry, take, text))) => {
            // Text replacing its own entry's keeps what already shows of it.
            let from = previous
                .filter(|previous| previous.entry == entry)
                .map(|previous| {
                    let showing: String =
                        previous.text.chars().take(previous.showing(now)).collect();
                    shared(&showing, &text)
                })
                .unwrap_or_default();
            Some(Shown {
                entry,
                take,
                text,
                reveal: animate.then_some((from, now)),
            })
        }
    };
}

/// The transcript card: the text as far as it is revealed, dimmed while a
/// newer take is under way, and a button that copies all of it.
pub fn transcript(view: &AppView, window: &mut Window, cx: &mut Context<AppView>) -> Stateful<Div> {
    let state = &view.transcript;
    let snapshot = view.model.snapshot.as_ref();
    let card = div()
        .relative()
        .flex_shrink_0()
        .rounded_xl()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().background)
        .pl_5()
        .pr_12()
        .py_4();
    let Some(shown) = &state.shown else {
        return a11y::status("transcript", "").child(
            card.text_color(cx.theme().muted_foreground)
                .child(rust_i18n::t!("transcript.empty").to_string()),
        );
    };
    let showing = shown.showing(Instant::now());
    let revealing = showing < shown.text.chars().count();
    if revealing {
        window.request_animation_frame();
        // Text revealed past the bottom scrolls into view as it appears.
        state.scroll.scroll_to_bottom();
    }
    let text: String = shown.text.chars().take(showing).collect();
    // The caret follows the text as a character, so it stays on its line.
    let caret = match revealing {
        true => "▍",
        false => "",
    };
    let written = text.len()..text.len() + caret.len();
    let newer = snapshot.is_some_and(|snapshot| {
        snapshot.state.take > shown.take
            && !matches!(snapshot.state.dictation.state, DictationState::Idle)
    });
    let full = shown.text.clone();
    a11y::status("transcript", shown.text.clone()).child(
        card.child(
            div()
                .id("transcript-text")
                .max_h(px(MAX_HEIGHT))
                .overflow_y_scroll()
                .track_scroll(&state.scroll)
                .text_base()
                .line_height(rems(1.6))
                .opacity(match newer {
                    true => 0.5,
                    false => 1.,
                })
                .child(StyledText::new(format!("{text}{caret}")).with_highlights([(
                    written,
                    HighlightStyle {
                        color: Some(cx.theme().primary),
                        ..HighlightStyle::default()
                    },
                )])),
        )
        .child(
            div().absolute().top_3().right_3().child(
                a11y::button("copy-transcript")
                    .ghost()
                    .small()
                    .icon(IconName::Copy)
                    .tooltip(rust_i18n::t!("action.copy").to_string())
                    .accessibility_label(rust_i18n::t!("action.copy").to_string())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(full.clone()));
                        this.show_toast(Toast::CopiedToClipboard);
                    })),
            ),
        ),
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{revealed, shared};

    #[test]
    fn short_text_is_paced_and_long_text_hurried() {
        let at = |total, from, ms| revealed(total, from, Duration::from_millis(ms));
        assert_eq!(
            [
                at(90, 0, 0),
                at(90, 0, 1000),
                at(90, 0, 5000),
                at(450, 0, 1000),
                at(450, 0, 2500),
                at(90, 60, 200),
            ],
            [0, 45, 90, 180, 450, 69]
        );
    }

    #[test]
    fn polished_text_keeps_what_it_shares_with_the_original() {
        assert_eq!(
            [
                shared("他说明天见", "他说：明天见。"),
                shared("hello wrld", "hello world"),
                shared("", "text"),
            ],
            [2, 7, 0]
        );
    }
}
