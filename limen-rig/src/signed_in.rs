//! Subscriptions signed in to through rig's authenticators: ChatGPT's
//! browser or device-code sign-in, and Copilot's GitHub device code. Each
//! sign-in is kept in a file of its own under the backend's sign-in
//! directory, which only the user can read; every request reads it afresh,
//! refreshing it when it has expired, so a sign-in made in the GUI reaches
//! the daemon without a restart.

use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use limen_core::{SignInStep, Subscription, Transient};
use rig_core::DynModel;
use rig_core::catalog::Catalog;
use rig_core::operation::Completion;
use rig_core::providers::chatgpt::auth::{
    AuthContext, AuthError, AuthSource as ChatGptSource, Authenticator as ChatGptAuth,
    DeviceCodeHandler, FileTokenStore, SignInMethod, SignInPrompt, TokenStore,
};
use rig_core::providers::copilot::auth::{
    AuthSource as CopilotSource, Authenticator as CopilotAuth,
};
use rig_core::providers::openai::wire::OpenAIConfig;
use rig_core::providers::{chatgpt, copilot};
use rig_reqwest::ReqwestClient;

/// A subscription's sign-in, as one request reads it.
pub(crate) enum Account {
    ChatGpt(ChatGptAuth, ReqwestClient),
    Copilot(CopilotAuth, ReqwestClient),
}

/// What a sign-in tells the user, shared by its browser and device-code
/// steps.
type Prompt = Arc<dyn Fn(SignInStep) + Send + Sync>;

/// The name rig keeps `subscription`'s sign-in under, and the name the
/// user knows it by.
fn names(subscription: Subscription) -> (&'static str, &'static str) {
    match subscription {
        Subscription::ChatGpt => (chatgpt::PROVIDER_NAME, "ChatGPT"),
        Subscription::Copilot => (copilot::PROVIDER_NAME, "Copilot"),
    }
}

impl Account {
    /// The sign-in to `subscription` kept under `dir`. With a `prompt`, a
    /// missing sign-in starts one that asks the user; without, a request
    /// fails instead, as the daemon's must.
    pub(crate) fn new(
        subscription: Subscription,
        dir: &Path,
        http: ReqwestClient,
        prompt: Option<Prompt>,
    ) -> Self {
        let store = FileTokenStore::new(dir);
        let codes = match &prompt {
            Some(prompt) => {
                let prompt = Arc::clone(prompt);
                DeviceCodeHandler::new(move |code| {
                    prompt(SignInStep::DeviceCode {
                        url: code.verification_uri,
                        code: code.user_code,
                    })
                })
            }
            None => DeviceCodeHandler::default(),
        };
        let asks = prompt.is_some();
        match subscription {
            Subscription::ChatGpt => Self::ChatGpt(
                ChatGptAuth::new(ChatGptSource::OAuth, store, codes, asks),
                http,
            ),
            Subscription::Copilot => Self::Copilot(
                CopilotAuth::new(CopilotSource::OAuth, store, codes, asks),
                http,
            ),
        }
    }

    /// The credential to send now, refreshed when it has expired.
    async fn context(&self) -> Result<AuthContext> {
        let (context, name) = match self {
            Self::ChatGpt(auth, http) => (auth.auth_context(http).await, "ChatGPT"),
            Self::Copilot(auth, http) => (auth.auth_context(http).await, "Copilot"),
        };
        context.map_err(|error| match error {
            AuthError::Http(_) => {
                Transient(format!("{name}'s sign-in could not be refreshed")).into()
            }
            AuthError::Message(_) | AuthError::Io(_) | AuthError::Json(_) | AuthError::Store(_) => {
                anyhow::anyhow!(
                    "not signed in to {name}, or the sign-in expired: sign in on the Polish page"
                )
            }
        })
    }

    /// The models the subscription serves, as rig's catalog lists them.
    pub(crate) fn models(&self) -> Vec<String> {
        let vendor = match self {
            Self::ChatGpt(..) => chatgpt::PROVIDER_NAME,
            Self::Copilot(..) => copilot::PROVIDER_NAME,
        };
        Catalog::builtin()
            .iter()
            .filter(|spec| spec.provider.vendor() == vendor)
            .map(|spec| spec.id.clone())
            .collect()
    }

    /// `model` with the credential the sign-in holds now.
    pub(crate) async fn completion(&self, model: &str) -> Result<DynModel<Completion>> {
        let context = self.context().await?;
        let (dialect, http) = match self {
            Self::ChatGpt(_, http) => (&chatgpt::DIALECT, http),
            Self::Copilot(_, http) => (&copilot::DIALECT, http),
        };
        let mut config = OpenAIConfig::with_key(dialect, context.api_key.expose().to_string());
        if let Some(api_base) = &context.api_base {
            config = config.with_base_url(api_base);
        }
        config.account_id = context.account_id;
        Ok(config.connect(http.clone()).completion(model).erase())
    }
}

/// Whether a sign-in to `subscription` is kept under `dir`.
pub(crate) fn kept(dir: &Path, subscription: Subscription) -> bool {
    FileTokenStore::new(dir)
        .path(names(subscription).0)
        .exists()
}

/// Why a sign-in failed, without what a service sent back: rig's own
/// messages can quote a reply, which can echo a credential.
fn sign_in_failed(name: &str, error: AuthError) -> anyhow::Error {
    match error {
        AuthError::Http(_) => {
            anyhow::anyhow!("{name} could not be reached, or refused the sign-in")
        }
        AuthError::Json(_) => anyhow::anyhow!("{name} sent a sign-in reply that could not be read"),
        AuthError::Message(_) => anyhow::anyhow!("{name} did not complete the sign-in; try again"),
        // This computer's own failures, such as a port in use or a file
        // that could not be written.
        error @ (AuthError::Io(_) | AuthError::Store(_)) => {
            anyhow::anyhow!("signing in to {name} failed: {error}")
        }
    }
}

/// Sign in to `subscription` again, keeping the sign-in under `dir`.
pub(crate) async fn sign_in(
    dir: &Path,
    subscription: Subscription,
    http: ReqwestClient,
    prompt: Prompt,
) -> Result<()> {
    // Only the user may read the sign-ins, in a directory made before as
    // much as in a new one.
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("failed to create {}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to make {} private", dir.display()))?;
    let (key, name) = names(subscription);
    match Account::new(subscription, dir, http, Some(Arc::clone(&prompt))) {
        Account::ChatGpt(auth, http) => auth
            .sign_in(&http, SignInMethod::Browser, |step| match step {
                SignInPrompt::Browser(page) => prompt(SignInStep::Browser {
                    url: page.authorize_url,
                }),
                // A device code follows, through the handler.
                SignInPrompt::BrowserUnavailable(_) => {}
            })
            .await
            .map(|_| ()),
        Account::Copilot(auth, http) => {
            // Copilot asks only when it holds no sign-in: forget the old one.
            FileTokenStore::new(dir)
                .delete(key)
                .await
                .map_err(AuthError::Store)
                .map_err(|error| sign_in_failed(name, error))?;
            auth.auth_context(&http).await.map(|_| ())
        }
    }
    .map_err(|error| sign_in_failed(name, error))
}

/// Forget the sign-in to `subscription` kept under `dir`; ChatGPT revokes
/// it too.
pub(crate) async fn sign_out(
    dir: &Path,
    subscription: Subscription,
    http: ReqwestClient,
) -> Result<()> {
    let (key, name) = names(subscription);
    match Account::new(subscription, dir, http, None) {
        Account::ChatGpt(auth, http) => auth.sign_out(&http).await.map(|_| ()),
        Account::Copilot(..) => FileTokenStore::new(dir)
            .delete(key)
            .await
            .map_err(AuthError::Store),
    }
    .map_err(|error| sign_in_failed(name, error))
}
