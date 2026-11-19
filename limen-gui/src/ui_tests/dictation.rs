//! The dictation control strip and the request list below it. The Linux
//! desktop suite keeps one take run through a daemon onto the clipboard.

use gpui_kit::{ScrollDelta, TestAppContext, point, px};
use limen_core::{Control, DictationState};
use rust_i18n::t;

use super::daemon::{failed_request, idle, kept, since};
use super::{Daemon, Ui};
use crate::bridge::{BridgeCommand, BridgeEvent};
use crate::model::Msg;

/// The labels of the dictation status and its primary button.
fn strip(ui: &mut Ui) -> (String, String) {
    (ui.label("dictation-status"), ui.label("dictation-primary"))
}

/// Record take `take` as the daemon does on a start.
fn recording(ui: &mut Ui, take: u64) {
    ui.publish(|state| {
        state.take = take;
        state.dictation = since(DictationState::Recording {
            control: Control::Toggle,
        });
    });
}

/// Accept the stop of take `take` and start transcribing it, as the
/// daemon and the bridge do.
fn transcribing(ui: &mut Ui, take: u64) {
    ui.receive(BridgeEvent::Msg(Msg::StopAccepted { take }));
    ui.publish(|state| state.dictation = since(DictationState::Transcribing));
}

/// Keep take `take`'s transcript and go idle.
fn transcribed(ui: &mut Ui, take: u64, text: &str) {
    ui.publish(|state| {
        state.history.push(kept(take, text));
        state.dictation = idle();
    });
}

/// Starting records; stopping transcribes and keeps the text, which lands
/// on the clipboard with a toast instead of being typed.
#[gpui_kit::test]
fn start_and_stop_copy_the_transcript(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    let idle = strip(&mut ui);
    ui.click("dictation-primary");
    let started = ui.sent();
    recording(&mut ui, 1);
    let recorded = strip(&mut ui);
    ui.click("dictation-primary");
    let stopped = ui.sent();
    transcribing(&mut ui, 1);
    transcribed(&mut ui, 1, "a transcript");
    assert_eq!(
        (
            idle,
            started,
            recorded,
            stopped,
            ui.label("dictation-status"),
            ui.clipboard(),
            ui.label("toast"),
        ),
        (
            (t!("status.idle").into(), t!("action.start").into()),
            vec![BridgeCommand::Start],
            (t!("status.recording").into(), t!("action.stop").into()),
            vec![BridgeCommand::Stop],
            t!("status.idle").into(),
            Some("a transcript".to_string()),
            t!("toast.copied").into(),
        )
    );
}

/// The newest transcript shows below the strip, the one the window opens
/// on at once, and copies whole; a new take's replaces it.
#[gpui_kit::test]
fn the_newest_transcript_shows_and_copies(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(
        cx,
        Daemon::with(vec![kept(1, "an earlier take")], Vec::new()),
    );
    let opened = ui.label("transcript");
    ui.click("copy-transcript");
    let copied = (ui.clipboard(), ui.label("toast"));
    recording(&mut ui, 2);
    transcribing(&mut ui, 2);
    transcribed(&mut ui, 2, "a newer take");
    assert_eq!(
        (opened, copied, ui.label("transcript")),
        (
            "an earlier take".to_string(),
            (
                Some("an earlier take".to_string()),
                t!("toast.copied").into()
            ),
            "a newer take".to_string(),
        )
    );
}

/// An error long enough to need several lines once unfolded.
const LONG_ERROR: &str = "provider returned 503: the upstream speech service is overloaded \
     and refused the request; retry after the queue drains or switch to another \
     endpoint in the configuration file";

/// A failed request describes its whole error and unfolds to show it when
/// pressed; pressing again folds it back.
#[gpui_kit::test]
fn failed_requests_expand_to_the_full_error(cx: &mut TestAppContext) {
    let request = failed_request(1, LONG_ERROR);
    let id = format!("request-1-{}", request.started_ms);
    let mut ui = Ui::connected(cx, Daemon::with(Vec::new(), vec![request]));
    let expanded = |ui: &mut Ui| ui.find(&id).expanded();
    let described = ui.find(&id).description().map(str::to_string);
    let folded = expanded(&mut ui);
    ui.click(&id);
    let unfolded = expanded(&mut ui);
    ui.click(&id);
    assert_eq!(
        (described.as_deref(), folded, unfolded, expanded(&mut ui)),
        (Some(LONG_ERROR), Some(false), Some(true), Some(false))
    );
}

/// The wheel scrolls a long request list, bringing the oldest into view,
/// where it still unfolds.
#[gpui_kit::test]
fn the_request_list_scrolls_with_the_wheel(cx: &mut TestAppContext) {
    let requests = (1..=20)
        .map(|take| failed_request(take, LONG_ERROR))
        .collect::<Vec<_>>();
    let (newest, oldest) = (
        format!("request-20-{}", requests[19].started_ms),
        format!("request-1-{}", requests[0].started_ms),
    );
    let mut ui = Ui::connected(cx, Daemon::with(Vec::new(), requests));
    let before = ui.find(&oldest).bounds();
    ui.scroll(&newest, ScrollDelta::Pixels(point(px(0.), px(-2000.))));
    let after = ui.find(&oldest).bounds();
    ui.click(&oldest);
    assert!(
        after.top() + px(200.) < before.top(),
        "the list did not scroll: {before:?} then {after:?}"
    );
    assert_eq!(ui.find(&oldest).expanded(), Some(true));
}

/// While a take transcribes, the button records another after it: the
/// press asks for a take, the button waits, and recording resumes once the
/// first transcript lands. Pressing the waiting button sends nothing.
#[gpui_kit::test]
fn recording_another_take_queues_it(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    ui.click("dictation-primary");
    recording(&mut ui, 1);
    ui.click("dictation-primary");
    transcribing(&mut ui, 1);
    let transcribing = strip(&mut ui);
    ui.click("dictation-primary");
    ui.publish(|state| state.queued = true);
    let queued = ui.label("dictation-primary");
    ui.click("dictation-primary");
    let sent = ui.sent();
    ui.publish(|state| {
        state.history.push(kept(1, "the first take"));
        state.queued = false;
        state.take = 2;
        state.dictation = since(DictationState::Recording {
            control: Control::Toggle,
        });
    });
    assert_eq!(
        (
            transcribing,
            queued,
            sent,
            ui.label("dictation-status"),
            ui.clipboard()
        ),
        (
            (
                t!("status.transcribing").into(),
                t!("action.record_another").into()
            ),
            t!("status.queued").into(),
            vec![
                BridgeCommand::Start,
                BridgeCommand::Stop,
                BridgeCommand::Start
            ],
            t!("status.recording").into(),
            Some("the first take".to_string()),
        )
    );
}
