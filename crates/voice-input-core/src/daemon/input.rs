//! The text-input worker and its history-correlated jobs and reports.

use std::future::Future;

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

/// Backend contract used by the input worker. Implementations select text
/// they can commit and update cross-segment spacing only when `commit_text`
/// is actually called; Keep deliberately bypasses both methods.
pub(crate) trait TextInput: Send {
    /// Select corrected text only when this backend can commit it.
    fn compatible_text<'a>(&self, original: &'a str, corrected: &'a str) -> &'a str;

    /// Commit text to the focused application.
    fn commit_text<'a>(&'a mut self, text: &'a str)
    -> impl Future<Output = Result<()>> + Send + 'a;
}

impl TextInput for InputMethod {
    fn compatible_text<'a>(&self, original: &'a str, corrected: &'a str) -> &'a str {
        InputMethod::compatible_text(self, original, corrected)
    }

    fn commit_text<'a>(
        &'a mut self,
        text: &'a str,
    ) -> impl Future<Output = Result<()>> + Send + 'a {
        InputMethod::commit_text(self, text)
    }
}

async fn process(
    job: InputJob,
    input_rx: &mpsc::UnboundedReceiver<InputJob>,
    input_method: &mut impl TextInput,
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
    use super::{InputJob, InputReport, TextInput, process};
    use crate::config::Config;
    use crate::dictation::InputOutcome;
    use crate::input_method::{Backend, InputMethod};
    use crate::ipc::v1::Delivery;
    use crate::polish::Polisher;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    fn config() -> Config {
        toml::from_str(
            r#"
            [provider]
            url = "http://127.0.0.1:8000/v1/audio/transcriptions"
            model = "test-model"
            "#,
        )
        .unwrap()
    }

    fn job(text: &str, delivery: Delivery) -> InputJob {
        InputJob {
            entry: Some(1),
            delivery,
            text: text.to_string(),
        }
    }

    #[derive(Default)]
    struct RecordingInput {
        committed: Vec<String>,
        last_was_latin: bool,
    }

    impl TextInput for RecordingInput {
        fn compatible_text<'a>(&self, _original: &'a str, corrected: &'a str) -> &'a str {
            corrected
        }

        async fn commit_text<'a>(&'a mut self, text: &'a str) -> anyhow::Result<()> {
            let needs_space = self.last_was_latin
                && text
                    .chars()
                    .next()
                    .is_some_and(|character| character.is_ascii_alphanumeric());
            self.committed.push(match needs_space {
                true => format!(" {text}"),
                false => text.to_string(),
            });
            self.last_was_latin = text
                .chars()
                .last()
                .is_some_and(|character| character.is_ascii_alphanumeric());
            Ok(())
        }
    }

    async fn committed(jobs: impl IntoIterator<Item = InputJob>) -> Vec<String> {
        let polisher = Polisher::from_config(&config().polish).unwrap();
        let mut input = RecordingInput::default();
        let (_jobs, input_rx) = mpsc::unbounded_channel();
        for job in jobs {
            process(job, &input_rx, &mut input, &polisher).await;
        }
        input.committed
    }

    #[tokio::test]
    async fn keep_preserves_the_backend_spacing_state() {
        let baseline = committed([job("one", Delivery::Input), job("two", Delivery::Input)]).await;
        let with_keep = committed([
            job("one", Delivery::Input),
            job("保留", Delivery::Keep),
            job("two", Delivery::Input),
        ])
        .await;

        assert_eq!(with_keep, baseline);
        assert_eq!(with_keep, ["one", " two"]);
    }

    #[tokio::test]
    async fn keep_retains_polishing_without_backend_compatibility_or_commit() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 16 * 1024];
            let _ = stream.read(&mut request).await.unwrap();
            let body = serde_json::json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": "修正"}
                }]
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
        });
        let mut polish = config().polish;
        polish.enabled = true;
        polish.url = format!("http://{address}/chat/completions");
        polish.timeout_ms = 1000;
        let polisher = Polisher::new(polish, "test-key".to_string()).unwrap();
        // Dotool rejects any Unicode commit before spawning a process. A
        // compatibility fallback would return the original and fail the test.
        let mut input_method = InputMethod::new(Backend::Dotool);
        let (_jobs, input_rx) = mpsc::unbounded_channel();

        let report = process(
            InputJob {
                entry: Some(7),
                delivery: Delivery::Keep,
                text: "原文".to_string(),
            },
            &input_rx,
            &mut input_method,
            &polisher,
        )
        .await;
        server.await.unwrap();

        assert_eq!(
            report,
            InputReport {
                entry: Some(7),
                delivery: Delivery::Keep,
                text: "修正".to_string(),
                outcome: InputOutcome::Succeeded,
            }
        );
    }
}
