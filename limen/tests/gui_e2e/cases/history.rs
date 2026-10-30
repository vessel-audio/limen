//! The transcript history page.

use anyhow::{Context, Result, ensure};
use atspi::Role;
use limen_core::v1;

use super::kept;
use crate::daemon::Scenario;
use crate::desktop::{Button, Node};
use crate::session::Session;

fn notifications(nodes: &[Node]) -> usize {
    nodes
        .iter()
        .filter(|node| node.role == Role::Notification)
        .count()
}

/// Each copy button puts its own transcript on the clipboard and shows a
/// notification that its close button dismisses.
pub fn copy_puts_a_transcript_on_the_clipboard() -> Result<()> {
    let session = Session::fake(Scenario {
        history: vec![kept(1, "first transcript"), kept(2, "second transcript")],
        ..Scenario::default()
    })?;
    session.click("history-page")?;
    session.named("page-title", "History · 2")?;
    session.click("copy-1")?;
    session.clipboard("first transcript")?;
    session.click("copy-2")?;
    session.clipboard("second transcript")?;
    let shown = session.wait("both notifications", |nodes| {
        (notifications(nodes) == 2).then_some(2)
    })?;
    let close = |nodes: &[Node]| {
        let notification = nodes
            .iter()
            .position(|node| node.role == Role::Notification)?;
        nodes
            .iter()
            .find(|node| node.parent == Some(notification) && node.role == Role::Button)
            .cloned()
    };
    session.click_hovered("the notification's close button", close)?;
    session.wait("one notification to close", |nodes| {
        (notifications(nodes) == shown - 1).then_some(())
    })?;
    Ok(())
}

/// Clearing asks first: cancelling keeps history, confirming clears it.
pub fn clearing_history_asks_for_confirmation() -> Result<()> {
    let session = Session::fake(Scenario {
        history: vec![kept(1, "keep me for now")],
        ..Scenario::default()
    })?;
    session.click("history-page")?;
    session.click("clear-history")?;
    session.node("clear-confirmation")?;
    session.click("cancel-clear")?;
    session.absent("clear-confirmation")?;
    session.named("page-title", "History · 1")?;
    ensure!(
        !session.requests()?.contains(&v1::Request::ClearHistory),
        "cancelling cleared history"
    );
    session.click("clear-history")?;
    session.click("confirm-clear")?;
    session.named("page-title", "History · 0")?;
    session.absent("clear-confirmation")?;
    ensure!(
        session.requests()?.contains(&v1::Request::ClearHistory),
        "confirming did not clear history"
    );
    Ok(())
}

/// The wheel scrolls a long history down to the oldest transcript.
pub fn the_history_scrolls_with_the_wheel() -> Result<()> {
    let session = Session::fake(Scenario {
        history: (1..=30)
            .map(|id| kept(id, &format!("transcript number {id}")))
            .collect(),
        ..Scenario::default()
    })?;
    session.click("history-page")?;
    let before = session.settled("copy-1")?.bounds.context("no bounds")?;
    let (x, y) = session
        .settled("copy-30")?
        .bounds
        .context("no bounds")?
        .center();
    session.x11().scroll(x, y, Button::WheelDown, 15)?;
    let after = session.settled("copy-1")?.bounds.context("no bounds")?;
    ensure!(
        after.y + 300 < before.y,
        "the history did not scroll: {before:?} then {after:?}"
    );
    session.click("copy-1")?;
    session.clipboard("transcript number 1")
}
