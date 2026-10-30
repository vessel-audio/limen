//! The dictation page: a control strip with a live waveform above the
//! daemon's recent transcription requests.

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, StyledExt,
    button::{Button, ButtonVariants},
};
use gpui_kit::*;
use voice_input_core::{Control, DictationState};

use crate::app::{AppView, LEVEL_BARS};
use crate::bridge::BridgeCommand;
use crate::model::{DictationButton, DisabledReason};
use crate::motion;

const WAVEFORM_HEIGHT: f32 = 28.;

pub fn dictation(view: &AppView, cx: &mut Context<AppView>) -> Div {
    let state = view
        .model
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.state.dictation);
    let title = match state.map(|status| status.state) {
        None => rust_i18n::t!("connection.connecting"),
        Some(DictationState::Idle) => rust_i18n::t!("status.idle"),
        Some(DictationState::Starting { .. }) => rust_i18n::t!("status.starting"),
        Some(DictationState::Recording {
            control: Control::Toggle,
        }) => rust_i18n::t!("status.recording"),
        Some(DictationState::Recording {
            control: Control::Hold,
        }) => rust_i18n::t!("status.hold_recording"),
        Some(DictationState::Transcribing) => rust_i18n::t!("status.transcribing"),
        Some(DictationState::Committing) => rust_i18n::t!("status.committing"),
    };
    let timer = state.and_then(|status| status.since_ms).map(|since| {
        format!(
            "{:02}:{:02}",
            super::elapsed(since) / 60,
            super::elapsed(since) % 60
        )
    });
    let recording = matches!(
        state.map(|status| status.state),
        Some(DictationState::Recording { .. })
    );
    let holding_long = state.is_some_and(|status| {
        matches!(
            status.state,
            DictationState::Recording {
                control: Control::Hold
            }
        ) && status
            .since_ms
            .is_some_and(|since| super::elapsed(since) >= 45)
    });
    let error = view
        .model
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.state.last_error.clone())
        .map(|error| error.message)
        .or_else(|| view.model.command_error.clone());
    let button = view.model.button();
    // The second line, most urgent first.
    let (note, note_color) = match (holding_long, recording && view.no_sound_detected(), error) {
        (true, _, _) => (
            rust_i18n::t!("hint.hold_timeout").to_string(),
            cx.theme().warning,
        ),
        (false, true, _) => (
            rust_i18n::t!("hint.no_sound").to_string(),
            cx.theme().warning,
        ),
        (false, false, Some(error)) => (error, cx.theme().danger),
        (false, false, None) => (
            match button {
                DictationButton::Disabled(DisabledReason::ReleaseShortcut) => {
                    rust_i18n::t!("hint.release_shortcut").to_string()
                }
                DictationButton::Start
                | DictationButton::Stop
                | DictationButton::RecordAnother
                | DictationButton::Queued
                | DictationButton::Disabled(_) => rust_i18n::t!("hint.keep_delivery").to_string(),
            },
            cx.theme().muted_foreground,
        ),
    };
    let (label, icon, command) = match button {
        DictationButton::Start => (
            rust_i18n::t!("action.start"),
            IconName::Mic,
            Some(BridgeCommand::Start),
        ),
        DictationButton::Stop => (
            rust_i18n::t!("action.stop"),
            IconName::Square,
            Some(BridgeCommand::Stop),
        ),
        DictationButton::RecordAnother => (
            rust_i18n::t!("action.record_another"),
            IconName::Mic,
            Some(BridgeCommand::Start),
        ),
        DictationButton::Queued => (rust_i18n::t!("status.queued"), IconName::Clock, None),
        DictationButton::Disabled(_) => (title.clone(), IconName::Mic, None),
    };
    let primary = cx.theme().primary;
    let halo = div()
        .flex_shrink_0()
        .size(px(44.))
        .rounded_full()
        .bg(primary.opacity(0.12))
        .flex()
        .items_center()
        .justify_center()
        .child(Icon::new(IconName::Mic).size(px(20.)).text_color(primary));
    // The newest levels as bars, oldest on the left; a flat baseline when idle.
    let bar_color = match recording {
        true => primary,
        false => cx.theme().border,
    };
    let waveform = div()
        .h_flex()
        .h(px(WAVEFORM_HEIGHT))
        .items_center()
        .justify_between()
        .children(
            std::iter::repeat_n(0., LEVEL_BARS.saturating_sub(view.level_history.len()))
                .chain(view.level_history.iter().copied())
                .map(|rms: f32| {
                    // Speech RMS rarely exceeds 0.25; the square root lifts
                    // quiet speech so it stays visible.
                    let height = 3. + (rms * 4.).sqrt().min(1.) * (WAVEFORM_HEIGHT - 3.);
                    div().w(px(3.)).h(px(height)).rounded_full().bg(bar_color)
                }),
        );
    let commands = view.commands.clone();
    let strip = div()
        .v_flex()
        .flex_shrink_0()
        .gap_4()
        .rounded_xl()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().muted.opacity(0.18))
        .px_5()
        .py_4()
        .child(
            div()
                .h_flex()
                .items_center()
                .gap_4()
                .child(match recording {
                    true => halo
                        .with_animation("recording-halo", motion::pulse(), move |halo, t| {
                            halo.bg(primary.opacity(0.32 * t))
                        })
                        .into_any_element(),
                    false => halo.into_any_element(),
                })
                .child(
                    div()
                        .v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(
                            div()
                                .h_flex()
                                .items_baseline()
                                .gap_2()
                                .child(
                                    div()
                                        .text_lg()
                                        .font_family("serif")
                                        .child(title.to_string()),
                                )
                                .children(timer.map(|timer| {
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(timer)
                                })),
                        )
                        // One line keeps the strip narrow; the request list
                        // below holds the full error.
                        .child(
                            div()
                                .text_sm()
                                .truncate()
                                .text_color(note_color)
                                .child(note),
                        ),
                )
                .child(
                    Button::new("dictation-primary")
                        .primary()
                        .icon(icon)
                        .label(label.to_string())
                        .flex_shrink_0()
                        .disabled(command.is_none())
                        .on_click(move |_, _, _| {
                            if let Some(command) = command.clone() {
                                let _ = commands.try_send(command);
                            }
                        }),
                ),
        )
        .child(waveform);
    div()
        .v_flex()
        .flex_1()
        .min_h_0()
        .w_full()
        .max_w(px(720.))
        .mx_auto()
        .gap_6()
        .p_8()
        .child(strip)
        .child(super::requests::requests(view, cx))
}
