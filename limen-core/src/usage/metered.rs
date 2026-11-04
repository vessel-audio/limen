//! Model clients that count each request they send on a [`Meter`].

use std::sync::Arc;

use anyhow::Result;

use super::{Meter, Tally};
use crate::backend::{BoxFuture, ChatModel, Exchange, Reply, Take, Transcriber, Transcript};
use crate::config::Service;
use crate::models::ModelRole;

/// One request being counted. Dropped before it is settled, as when its
/// try times out or its take is abandoned, it counts as a failure.
struct Pending {
    meter: Meter,
    role: ModelRole,
    service: Service,
    model: Arc<str>,
    settled: bool,
}

impl Pending {
    fn settle(mut self, tally: Tally) {
        self.settled = true;
        self.meter
            .record(self.role, self.service, &self.model, tally);
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if !self.settled {
            self.meter
                .record(self.role, self.service, &self.model, Tally::failure());
        }
    }
}

/// A transcriber whose requests, the audio each answered, and its cost
/// are counted: the vendor's price per minute where Limen knows it, or else
/// what the backend priced the reported tokens at.
pub(crate) struct MeteredTranscriber {
    pub(crate) inner: Arc<dyn Transcriber>,
    pub(crate) meter: Meter,
    pub(crate) service: Service,
    pub(crate) model: Arc<str>,
}

impl Transcriber for MeteredTranscriber {
    fn transcribe(&self, take: Take) -> BoxFuture<'_, Result<Transcript>> {
        // The take is 16-bit mono PCM in a WAV container.
        let audio_ms = hound::WavReader::new(std::io::Cursor::new(&take.wav)).map_or(0, |reader| {
            u64::from(reader.duration()) * 1000 / u64::from(reader.spec().sample_rate.max(1))
        });
        Box::pin(async move {
            // Counted from its first poll, when the request goes out.
            let pending = Pending {
                meter: self.meter.clone(),
                role: ModelRole::Asr,
                service: self.service,
                model: self.model.clone(),
                settled: false,
            };
            let transcript = self.inner.transcribe(take).await;
            pending.settle(match &transcript {
                Ok(transcript) => {
                    let cost = super::prices::per_minute(self.service, &self.model, audio_ms)
                        .or(transcript.usage.cost_usd);
                    Tally {
                        requests: 1,
                        audio_ms,
                        input_tokens: transcript.usage.input_tokens.unwrap_or(0),
                        output_tokens: transcript.usage.output_tokens.unwrap_or(0),
                        cost_usd: cost.unwrap_or(0.),
                        unpriced: u64::from(cost.is_none()),
                        ..Tally::default()
                    }
                }
                Err(_) => Tally::failure(),
            });
            transcript
        })
    }
}

/// A chat model whose requests, and the tokens and cost each reported,
/// are counted.
pub(crate) struct MeteredChat {
    pub(crate) inner: Box<dyn ChatModel>,
    pub(crate) meter: Meter,
    pub(crate) service: Service,
    pub(crate) model: Arc<str>,
}

impl ChatModel for MeteredChat {
    fn reply<'a>(&'a self, exchange: Exchange<'a>) -> BoxFuture<'a, Result<Reply>> {
        Box::pin(async move {
            // Counted from its first poll, when the request goes out.
            let pending = Pending {
                meter: self.meter.clone(),
                role: ModelRole::Polish,
                service: self.service,
                model: self.model.clone(),
                settled: false,
            };
            let reply = self.inner.reply(exchange).await;
            pending.settle(match &reply {
                Ok(reply) => Tally {
                    requests: 1,
                    input_tokens: reply.usage.input_tokens.unwrap_or(0),
                    output_tokens: reply.usage.output_tokens.unwrap_or(0),
                    cost_usd: reply.usage.cost_usd.unwrap_or(0.),
                    unpriced: u64::from(reply.usage.cost_usd.is_none()),
                    ..Tally::default()
                },
                Err(_) => Tally::failure(),
            });
            reply
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::MeteredChat;
    use crate::backend::{BoxFuture, ChatModel, Exchange, Finish, Reply, Usage};
    use crate::config::{Paths, Service};
    use crate::models::ModelRole;
    use crate::usage::{Meter, Tally, UsageRow, read_usage, today};

    /// A chat model that answers `reply`, or never answers when it is `None`.
    struct Answering(Option<Reply>);

    impl ChatModel for Answering {
        fn reply<'a>(&'a self, _: Exchange<'a>) -> BoxFuture<'a, anyhow::Result<Reply>> {
            let reply = self.0.clone();
            Box::pin(async move {
                match reply {
                    Some(reply) => Ok(reply),
                    None => std::future::pending().await,
                }
            })
        }
    }

    const EXCHANGE: Exchange<'static> = Exchange {
        instructions: "",
        message: "text",
        max_tokens: 16,
    };

    #[tokio::test]
    async fn answers_add_their_tokens_and_cost_and_an_abandoned_request_fails() {
        let dir = std::env::temp_dir().join(format!("limen-meter-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = Paths {
            socket: dir.join("limen.sock"),
            log_file: dir.join("daemon.log"),
            cache_dir: dir.join("cache"),
            config_file: dir.join("config.toml"),
        };
        let meter = Meter::open(&paths);
        let chat = |reply| MeteredChat {
            inner: Box::new(Answering(reply)),
            meter: meter.clone(),
            service: Service::DeepSeek,
            model: Arc::from("flash"),
        };
        let answered = Reply {
            text: "Text.".to_string(),
            finish: Some(Finish::Stop),
            usage: Usage {
                input_tokens: Some(120),
                cached_input_tokens: None,
                output_tokens: Some(30),
                cost_usd: Some(0.5),
            },
        };
        chat(Some(answered.clone())).reply(EXCHANGE).await.unwrap();
        chat(Some(Reply {
            usage: Usage::default(),
            ..answered
        }))
        .reply(EXCHANGE)
        .await
        .unwrap();
        // Cut short, as a timed-out try is.
        let unanswered = chat(None);
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(10),
            unanswered.reply(EXCHANGE),
        )
        .await;

        assert_eq!(
            read_usage(&paths).unwrap().since(today()),
            vec![UsageRow {
                role: ModelRole::Polish,
                service: Service::DeepSeek,
                model: "flash".to_string(),
                tally: Tally {
                    requests: 3,
                    failures: 1,
                    input_tokens: 120,
                    output_tokens: 30,
                    audio_ms: 0,
                    cost_usd: 0.5,
                    unpriced: 1,
                },
            }]
        );
    }
}
