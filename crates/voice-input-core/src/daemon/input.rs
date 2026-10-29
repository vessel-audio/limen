//! The text-input worker and its history-correlated jobs and reports.

use anyhow::Result;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::dictation::InputOutcome;
use crate::input_method::{InputMethod, default_backend, sanitize_control_characters};
use crate::ipc::v1::Delivery;
use crate::polish::Polisher;

pub(crate) const MAX_OUTSTANDING_INPUT: usize = 17;

/// One transcript admitted by the dictation actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputJob {
    pub(crate) entry: Option<u64>,
    pub(crate) delivery: Delivery,
    pub(crate) text: String,
}

/// The exact text selected by the worker and its core-facing outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputReport {
    pub(crate) entry: Option<u64>,
    pub(crate) delivery: Delivery,
    pub(crate) text: String,
    pub(crate) outcome: InputOutcome,
}

async fn process(
    job: InputJob,
    input_rx: &mpsc::UnboundedReceiver<InputJob>,
    input_method: &mut InputMethod,
    polisher: &Polisher,
) -> InputReport {
    let original = job.text;
    // Cloud work is optional; queued originals take priority.
    let corrected = polisher
        .polish_when_idle(original.clone(), input_rx.is_empty())
        .await;
    let (text, outcome) = match job.delivery {
        Delivery::Input => {
            let selected = input_method.compatible_text(&original, &corrected);
            let text = sanitize_control_characters(selected).trim().to_string();
            let outcome = match input_method.commit_text(&text).await {
                Ok(()) => InputOutcome::Succeeded,
                Err(error) => {
                    tracing::warn!(error = %format!("{error:#}"), "text input failed");
                    InputOutcome::Failed(format!("{error:#}"))
                }
            };
            (text, outcome)
        }
        Delivery::Keep => (
            sanitize_control_characters(&corrected).trim().to_string(),
            InputOutcome::Succeeded,
        ),
    };
    InputReport {
        entry: job.entry,
        delivery: job.delivery,
        text,
        outcome,
    }
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
            let report = process(job, &input_rx, &mut input_method, &polisher).await;
            if report_tx.send(report).is_err() {
                break;
            }
        }
    });
    Ok((input_tx, reports))
}

#[cfg(test)]
mod tests {
    use super::{InputJob, InputReport, process};
    use crate::config::Config;
    use crate::dictation::InputOutcome;
    use crate::input_method::{Backend, InputMethod};
    use crate::ipc::v1::Delivery;
    use crate::polish::Polisher;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn keep_polishes_history_without_calling_the_input_backend() {
        let config: Config = toml::from_str(
            r#"
            [provider]
            url = "http://127.0.0.1:8000/v1/audio/transcriptions"
            model = "test-model"
            "#,
        )
        .unwrap();
        let polisher = Polisher::from_config(&config.polish).unwrap();
        // Dotool rejects Unicode before spawning a process, so this test also
        // proves that Keep never reaches backend compatibility or commit logic.
        let mut input_method = InputMethod::new(Backend::Dotool);
        let (_jobs, input_rx) = mpsc::unbounded_channel();

        let report = process(
            InputJob {
                entry: Some(7),
                delivery: Delivery::Keep,
                text: "原始文本".to_string(),
            },
            &input_rx,
            &mut input_method,
            &polisher,
        )
        .await;

        assert_eq!(
            report,
            InputReport {
                entry: Some(7),
                delivery: Delivery::Keep,
                text: "原始文本".to_string(),
                outcome: InputOutcome::Succeeded,
            }
        );
    }
}
