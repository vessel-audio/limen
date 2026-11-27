//! Answering the audio routing requests.

use tokio::sync::oneshot;

use super::DictationActor;
use crate::config::Config;
use crate::dictation::DictationState;
use crate::ipc::v1::{Reply, Request};

impl DictationActor {
    /// Answer an `audio_*` request. The work is blocking or takes seconds
    /// (reading plugins, loading workers, recording), so it runs off the
    /// actor, which carries on with dictation meanwhile; only the choice of
    /// whether to run it is made here, where the state of the take is known.
    pub(super) fn audio_request(&mut self, request: Request, reply: oneshot::Sender<Reply>) {
        let Some(audio) = self.audio.clone() else {
            let _ = reply.send(error("audio routing is not available"));
            return;
        };
        match request {
            Request::AudioTest { seconds } => {
                match self.dictation.view().state {
                    DictationState::Idle => {}
                    DictationState::Starting { .. }
                    | DictationState::Recording { .. }
                    | DictationState::Transcribing
                    | DictationState::Committing => {
                        let _ = reply.send(error("stop dictating before a test recording"));
                        return;
                    }
                }
                // Claim the microphone here, where a dictation that starts
                // next is handled in turn, so that it finds the test to cancel.
                let cancel = match audio.begin_test(seconds) {
                    Ok(cancel) => cancel,
                    Err(message) => {
                        let _ = reply.send(error(&message));
                        return;
                    }
                };
                let device = Config::load(&self.config_file)
                    .ok()
                    .and_then(|config| config.audio.device);
                tokio::spawn(async move {
                    let answer = match audio.record_test(cancel, seconds, device).await {
                        Ok(files) => Reply::AudioTest(files),
                        Err(failure) => error(&format!("{failure:#}")),
                    };
                    let _ = reply.send(answer);
                });
            }
            Request::AudioScan => blocking(reply, move || {
                audio.scan().map(|plugins| Reply::AudioPlugins { plugins })
            }),
            Request::AudioGraphGet => blocking(reply, move || audio.view().map(Reply::AudioGraph)),
            Request::AudioGraphSet { graph } => {
                blocking(reply, move || audio.set_graph(graph).map(Reply::AudioGraph))
            }
            Request::AudioEditorOpen { node } => {
                blocking(reply, move || audio.open_editor(node).map(|()| Reply::Ok))
            }
            Request::Subscribe
            | Request::Shutdown
            | Request::ClearHistory
            | Request::Start
            | Request::Stop { .. }
            | Request::Hold { .. }
            | Request::ShortcutRebind
            | Request::ShortcutStatus
            | Request::ProviderReload
            | Request::PolishReload => {
                let _ = reply.send(error("not an audio request"));
            }
        }
    }
}

fn error(message: &str) -> Reply {
    Reply::Error {
        message: message.to_string(),
    }
}

/// Run `work` on a blocking thread and answer with what it returns.
fn blocking(
    reply: oneshot::Sender<Reply>,
    work: impl FnOnce() -> Result<Reply, String> + Send + 'static,
) {
    tokio::spawn(async move {
        let answer = match tokio::task::spawn_blocking(work).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(message)) => Reply::Error { message },
            Err(_) => error("the audio request stopped unexpectedly"),
        };
        let _ = reply.send(answer);
    });
}
