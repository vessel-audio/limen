//! The list of takes on the dictation page: copying a transcript, clearing
//! the history, scrolling a long list, and failed requests among the
//! transcripts. The Linux desktop suite does not repeat them.

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, Pixels, Point, ScrollDelta, SharedString, TestAppContext, point, px};
use rust_i18n::t;

use super::daemon::{failed_request, kept};
use super::{Daemon, Ui};
use crate::bridge::BridgeCommand;

/// The centres of the elements with this ID that can be seen, top to bottom.
/// Notifications and their close buttons share IDs, so they cannot be
/// found one by one.
fn shown(ui: &mut Ui, id: &str) -> Vec<Point<Pixels>> {
    let id = SharedString::from(id.to_string());
    ui.cx
        .update_window(ui.window, |_, window, cx| {
            window.render_frame(cx);
            window
                .find_all(id)
                .iter()
                .filter(|element| element.visible())
                .map(|element| element.bounds().center())
                .collect()
        })
        .expect("the control panel closed")
}

/// Each copy button puts its own transcript on the clipboard and shows a
/// notification, which its close button dismisses.
#[gpui_kit::test]
fn copy_puts_a_transcript_on_the_clipboard(cx: &mut TestAppContext) {
    let history = vec![kept(1, "first transcript"), kept(2, "second transcript")];
    let mut ui = Ui::connected(cx, Daemon::with(history, Vec::new()));
    ui.click("copy-1");
    let first = (ui.clipboard(), shown(&mut ui, "notification").len());
    ui.click("copy-2");
    let second = (ui.clipboard(), shown(&mut ui, "notification").len());
    // Notifications share their IDs, which the harness cannot click, so the
    // pointer goes by position: pressing a notification leaves it resting
    // there, which shows its close button.
    let notification = shown(&mut ui, "notification")[0];
    ui.drag(notification, notification);
    let hovered = shown(&mut ui, "notification").len();
    let close = shown(&mut ui, "close")[0];
    ui.drag(close, close);
    assert_eq!(
        (first, second, hovered, shown(&mut ui, "notification").len()),
        (
            (Some("first transcript".to_string()), 1),
            (Some("second transcript".to_string()), 2),
            2,
            1,
        )
    );
}

/// Clearing asks first: cancelling keeps the history and sends nothing,
/// confirming asks the daemon to clear it, and the list empties once the
/// daemon has.
#[gpui_kit::test]
fn clearing_history_asks_for_confirmation(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(
        cx,
        Daemon::with(vec![kept(1, "keep me for now")], Vec::new()),
    );
    ui.click("clear-history");
    let asked = (
        ui.label("clear-confirmation"),
        ui.try_find("clear-history").is_some(),
    );
    ui.click("cancel-clear");
    let cancelled = (
        ui.try_find("clear-confirmation").is_some(),
        ui.try_find("copy-1").is_some(),
        ui.sent(),
    );
    ui.click("clear-history");
    ui.click("confirm-clear");
    let confirmed = (ui.try_find("clear-confirmation").is_some(), ui.sent());
    ui.publish(|state| state.history.clear());
    assert_eq!(
        (asked, cancelled, confirmed, ui.try_find("copy-1").is_some()),
        (
            (t!("history.clear_confirm").into(), false),
            (false, true, Vec::new()),
            (false, vec![BridgeCommand::ClearHistory]),
            false,
        )
    );
}

/// The wheel scrolls a long history down to the oldest transcript, which
/// still copies.
#[gpui_kit::test]
fn the_history_scrolls_with_the_wheel(cx: &mut TestAppContext) {
    let history = (1..=30)
        .map(|id| kept(id, &format!("transcript number {id}")))
        .collect();
    let mut ui = Ui::connected(cx, Daemon::with(history, Vec::new()));
    let before = ui.find("copy-1").bounds();
    ui.scroll("copy-30", ScrollDelta::Pixels(point(px(0.), px(-4000.))));
    let after = ui.find("copy-1").bounds();
    // Pressing the button needs it in view.
    ui.click("copy-1");
    assert_eq!(
        (after.top() + px(300.) < before.top(), ui.clipboard()),
        (true, Some("transcript number 1".to_string())),
        "the history did not scroll: {before:?} then {after:?}"
    );
}

/// Transcripts and failed requests share one list, newest take first.
#[gpui_kit::test]
fn transcripts_and_failures_share_one_list(cx: &mut TestAppContext) {
    let failure = failed_request(2, "timed out");
    let failed = format!("request-2-{}", failure.started_ms);
    let mut ui = Ui::connected(
        cx,
        Daemon::with(vec![kept(1, "an earlier take")], vec![failure]),
    );
    let (list, failure, transcript) = (
        ui.find("takes").bounds(),
        ui.find(&failed).bounds(),
        ui.find("copy-1").bounds(),
    );
    assert_eq!(
        (
            list.contains(&failure.origin),
            list.contains(&transcript.origin),
            failure.top() < transcript.top(),
        ),
        (true, true, true),
        "the failure at {failure:?} is not above the transcript at {transcript:?} in {list:?}"
    );
}
