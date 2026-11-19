//! A take run through the daemon onto the desktop's clipboard. The rest of
//! the dictation page is tested in-process in `limen-gui`'s UI tests.

use anyhow::{Result, ensure};
use limen_core::{Delivery, v1};

use crate::daemon::Scenario;
use crate::session::Session;

/// Starting records; stopping transcribes and keeps the text, which lands
/// on the clipboard with a toast instead of being typed.
pub fn start_and_stop_copy_the_transcript() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.named("dictation-status", "Idle")?;
    session.click("dictation-primary")?;
    session.named("dictation-status", "Recording")?;
    session.named("dictation-primary", "Stop and transcribe")?;
    session.click("dictation-primary")?;
    session.named("dictation-status", "Idle")?;
    session.clipboard("e2e transcript 1")?;
    session.named("toast", "Copied to clipboard")?;
    ensure!(
        session.requests()?
            == [
                v1::Request::Start,
                v1::Request::Stop {
                    delivery: Delivery::Keep
                }
            ],
        "unexpected requests"
    );
    Ok(())
}
