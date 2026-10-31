//! The window as the window manager sees it: moving, maximizing, closing,
//! and launching the control panel while it is already open.

use anyhow::{Context, Result, ensure};
use atspi::Role;

use crate::daemon::Scenario;
use crate::desktop::Bounds;
use crate::session::{APP_ID, Session};

/// `_NET_WM_MOVERESIZE` direction for a keyboard-less move.
const MOVE: u32 = 8;
/// `_NET_WM_STATE` action that flips a state.
const TOGGLE: u32 = 2;

/// A point on the title bar between the page name and the status, where
/// nothing but the bar itself takes the press.
fn title_bar_gap(session: &Session) -> Result<(i32, i32)> {
    let bounds = |id| -> Result<Bounds> {
        session
            .settled(id)?
            .bounds
            .context("title bar item is off screen")
    };
    let (page, status) = (bounds("current-page")?, bounds("connection-status")?);
    Ok(((page.x + page.width + status.x) / 2, page.center().1))
}

/// Dragging the title bar hands the move to the window manager.
pub fn dragging_the_title_bar_moves_the_window() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    let from = title_bar_gap(&session)?;
    session.x11().drag(from, (from.0 + 60, from.1 + 40))?;
    session.until("a move request", || {
        Ok(session
            .wm_requests("_NET_WM_MOVERESIZE")?
            .iter()
            .any(|request| request.data[2] == MOVE))
    })
}

/// Double-clicking the title bar asks to toggle maximization.
pub fn double_clicking_the_title_bar_toggles_maximize() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    let (vertical, horizontal) = (
        session.x11().atom("_NET_WM_STATE_MAXIMIZED_VERT")?,
        session.x11().atom("_NET_WM_STATE_MAXIMIZED_HORZ")?,
    );
    session.double_click(title_bar_gap(&session)?)?;
    session.until("a maximize toggle", || {
        Ok(session
            .wm_requests("_NET_WM_STATE")?
            .iter()
            .any(|request| request.data[..3] == [TOGGLE, vertical, horizontal]))
    })
}

/// Closing the window ends the control panel but not dictation.
pub fn closing_the_window_keeps_the_daemon_running() -> Result<()> {
    let mut session = Session::fake(Scenario::default())?;
    session.x11().close_window(APP_ID)?;
    let status = session.gui_exit()?;
    ensure!(status.success(), "the control panel exited with {status}");
    let daemon = session.fake_daemon()?;
    ensure!(daemon.exit_status()?.is_none(), "the daemon stopped too");
    ensure!(
        !daemon
            .requests()?
            .contains(&limen_core::v1::Request::Shutdown),
        "closing asked the daemon to shut down"
    );
    Ok(())
}

/// A second launch reports the open window and leaves it alone.
pub fn a_second_launch_keeps_the_open_window() -> Result<()> {
    let mut session = Session::fake(Scenario::default())?;
    let output = session.launch_again()?;
    ensure!(
        output.status.success(),
        "the second launch failed: {output:?}"
    );
    ensure!(
        String::from_utf8(output.stdout)?.contains("already open"),
        "the second launch did not report the open window"
    );
    ensure!(session.gui_running()?, "the first window closed");
    let frames = session.wait("the window list", |nodes| {
        Some(nodes.iter().filter(|node| node.role == Role::Frame).count())
    })?;
    ensure!(frames == 1, "{frames} windows are open");
    Ok(())
}
