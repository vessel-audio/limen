//! The text-input worker and its history-correlated jobs and reports.

use anyhow::Result;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::dictation::InputOutcome;
use crate::input_method::{InputMethod, default_backend, sanitize_control_characters};
use crate::polish::Polisher;

pub(crate) const MAX_OUTSTANDING_INPUT: usize = 17;

/// One transcript admitted by the dictation actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputJob {
    pub(crate) entry: Option<u64>,
    pub(crate) text: String,
}

/// The exact text selected by the worker and its core-facing outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputReport {
    pub(crate) entry: Option<u64>,
    pub(crate) text: String,
    pub(crate) outcome: InputOutcome,
}

/// Start the serial input worker, returning its job and report mailboxes.
pub(crate) fn spawn(
    config: &Config,
) -> Result<(
    mpsc::UnboundedSender<InputJob>,
    mpsc::UnboundedReceiver<InputReport>,
)> {
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<InputJob>();
    let (report_tx, reports) = mpsc::unbounded_channel::<InputReport>();
    let backend = default_backend(config.input_method.backend);
    let polisher = Polisher::from_config(&config.polish)?;
    tracing::info!(?backend, "input method backend");
    tokio::spawn(async move {
        let mut input_method = InputMethod::new(backend);
        while let Some(job) = input_rx.recv().await {
            let original = job.text;
            // Cloud work is optional; queued originals take priority.
            let corrected = polisher
                .polish_when_idle(original.clone(), input_rx.is_empty())
                .await;
            let selected = input_method.compatible_text(&original, &corrected);
            let text = sanitize_control_characters(selected).trim().to_string();
            let outcome = match input_method.commit_text(&text).await {
                Ok(()) => InputOutcome::Succeeded,
                Err(error) => {
                    tracing::warn!(error = %format!("{error:#}"), "text input failed");
                    InputOutcome::Failed(format!("{error:#}"))
                }
            };
            let report = InputReport {
                entry: job.entry,
                text,
                outcome,
            };
            if report_tx.send(report).is_err() {
                break;
            }
        }
    });
    Ok((input_tx, reports))
}
