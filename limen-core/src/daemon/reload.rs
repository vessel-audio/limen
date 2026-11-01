//! Applying edited `[provider]` and `[polish]` sections to a running daemon.

use super::{DictationActor, unix_ms};
use crate::config::Config;
use crate::ipc;
use crate::session::SessionCommand;

impl DictationActor {
    /// Apply the `[provider]` section of the config again. The whole file must
    /// be valid; the take in flight, if any, finishes with the old provider.
    pub(super) fn reload_provider(&mut self) -> ipc::v1::Reply {
        match Config::load(&self.config_file) {
            Ok(config) => {
                self.projection.set_provider(&config.provider);
                self.projection.publish(self.dictation.view(), unix_ms());
                tracing::info!(url = %config.provider.url, model = %config.provider.model, "provider reloaded");
                self.session.tell(SessionCommand::Provider(config.provider));
                ipc::v1::Reply::Ok
            }
            Err(error) => ipc::v1::Reply::Error {
                message: format!("{error:#}"),
            },
        }
    }

    /// Apply the `[polish]` section of the config again. The whole file must
    /// be valid; a transcript being polished finishes with the old settings.
    pub(super) fn reload_polish(&mut self) -> ipc::v1::Reply {
        match Config::load(&self.config_file) {
            Ok(config) => {
                self.projection.set_polish(&config.polish);
                self.projection.publish(self.dictation.view(), unix_ms());
                tracing::info!(
                    enabled = config.polish.enabled,
                    model = %config.polish.model,
                    "polishing reloaded"
                );
                self.polish.send_replace(config.polish);
                ipc::v1::Reply::Ok
            }
            Err(error) => ipc::v1::Reply::Error {
                message: format!("{error:#}"),
            },
        }
    }
}
