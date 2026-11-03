//! Trying a provider's model with one real request before it is saved.

use std::time::Duration;

use anyhow::{Result, anyhow, bail};

use super::{Provider, edited_off_thread};
use crate::audio::SAMPLE_RATE;
use crate::backend::{Exchange, Finish, ModelBackend};
use crate::config::{KeyChange, Paths};
use crate::connection;
use crate::polish::{CONSERVATIVE_PROMPT, MAX_TOKENS};
use crate::provider::{self, SessionConfig, TranscriptEvent};

/// How long the model may take to answer.
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

/// Half a second of silence at the capture rate: a take every ASR model
/// accepts, whose transcript may be empty.
const SILENCE: [i16; SAMPLE_RATE as usize / 2] = [0; SAMPLE_RATE as usize / 2];

/// Ask the model `provider` names to do its job once, as the daemon would:
/// transcribe half a second of silence, or correct a short sentence. The
/// page asks before saving, so `provider` and `key` are its unsaved fields,
/// and a stored key goes only where `edited` lets it.
pub async fn check_model(
    paths: &Paths,
    provider: &Provider,
    key: &KeyChange,
    backend: &dyn ModelBackend,
) -> Result<()> {
    let model = match provider {
        Provider::Asr { model, .. } | Provider::Polish { model, .. } => model.trim(),
    };
    if model.is_empty() {
        bail!("choose a model first");
    }
    let check = async {
        let config = edited_off_thread(paths, provider, key).await?;
        match provider {
            Provider::Asr { .. } => {
                let session = provider::from_config(&config, backend)?
                    .start_session(&SessionConfig {
                        language: config.provider.language.clone(),
                        sample_rate: SAMPLE_RATE,
                    })
                    .await?;
                let (mut sink, mut events) = session.split();
                sink.send_audio(&SILENCE).await?;
                sink.finalize().await?;
                loop {
                    match events.next_event().await {
                        Some(TranscriptEvent::Stable(_)) => {}
                        Some(TranscriptEvent::Completed) => return Ok(()),
                        Some(TranscriptEvent::Failed(error)) => return Err(error),
                        None => bail!("the model gave no transcript"),
                    }
                }
            }
            Provider::Polish { .. } => {
                let connection = connection::polish(&config.polish, config.polish.api_key())?;
                let reply = backend
                    .chat(connection, model.to_string())?
                    .reply(Exchange {
                        instructions: CONSERVATIVE_PROMPT,
                        message: "this is a test of the polishing model",
                        max_tokens: MAX_TOKENS,
                    })
                    .await?;
                match (reply.finish, reply.text.trim().is_empty()) {
                    (Some(Finish::Stop), false) => Ok(()),
                    (Some(Finish::Stop), true) => bail!("the model answered with no text"),
                    (finish, _) => bail!("the model stopped with {finish:?}"),
                }
            }
        }
    };
    tokio::time::timeout(CHECK_TIMEOUT, check)
        .await
        .map_err(|_| anyhow!("the model did not answer within {CHECK_TIMEOUT:?}"))?
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::check_model;
    use crate::backend::testing::Answering;
    use crate::backend::{Finish, Reply};
    use crate::config::{Format, KeyChange, Paths, Service};
    use crate::models::{Polishing, Provider};

    /// Paths in a fresh directory holding an empty config.
    fn scratch(name: &str) -> Paths {
        let dir = std::env::temp_dir().join(format!("limen-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.toml"), "[provider]\nurl = \"\"\n").unwrap();
        Paths {
            socket: dir.join("limen.sock"),
            log_file: dir.join("daemon.log"),
            cache_dir: dir.join("cache"),
            config_file: dir.join("config.toml"),
        }
    }

    fn answering(finish: Finish) -> Answering {
        Answering {
            transcript: String::new(),
            reply: Reply {
                text: "This is a test.".to_string(),
                finish: Some(finish),
            },
        }
    }

    fn polish(model: &str) -> Provider {
        Provider::Polish {
            polishing: Polishing::Off,
            service: Service::Custom,
            format: Format::OpenAiCompatible,
            url: "http://127.0.0.1:8080/v1/chat/completions".to_string(),
            model: model.to_string(),
        }
    }

    #[tokio::test]
    async fn a_model_passes_only_with_a_complete_answer() {
        let paths = scratch("check-polish");
        let check = |finish, model: &'static str| {
            let paths = paths.clone();
            async move {
                check_model(&paths, &polish(model), &KeyChange::Keep, &answering(finish))
                    .await
                    .map_err(|error| error.to_string())
            }
        };

        assert_eq!(
            [
                check(Finish::Stop, "corrector").await,
                check(Finish::Length, "corrector").await,
                check(Finish::Stop, " ").await,
            ],
            [
                Ok(()),
                Err("the model stopped with Some(Length)".to_string()),
                Err("choose a model first".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn an_asr_model_passes_once_it_transcribes_a_take() {
        let paths = scratch("check-asr");
        let asr = Provider::Asr {
            service: Service::Custom,
            format: Format::OpenAiCompatible,
            url: "http://127.0.0.1:8000/v1/audio/transcriptions".to_string(),
            model: "asr".to_string(),
        };

        check_model(&paths, &asr, &KeyChange::Keep, &answering(Finish::Stop))
            .await
            .unwrap();
    }
}
