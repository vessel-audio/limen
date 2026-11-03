//! A subscription's sign-in as its dialog shows it: whether one is kept,
//! and the sign-in under way with what it asks the user to do.

use limen_core::{ModelRole, SignInStep, Subscription};

use super::{ModelForms, ProviderForm};
use crate::app::AppView;
use crate::bridge::{BridgeCommand, ModelRequest};

/// What the dialog knows of the sign-in to its subscription.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Account {
    /// Not known since the dialog opened, or since a sign-out was asked.
    #[default]
    Unknown,
    SignedOut,
    SignedIn,
    /// The sign-in `id` is under way; `step` is what it asks the user to
    /// do, once it asks.
    Signing {
        id: u64,
        step: Option<SignInStep>,
    },
}

impl ModelForms {
    /// Record whether a sign-in to `subscription` is kept, unless the
    /// dialog is signing in again.
    pub fn signed_in(&mut self, subscription: Subscription, signed_in: bool) {
        let form = &mut self.polish;
        match (&form.account, form.chosen_service.subscription()) {
            (Account::Signing { .. }, _) => {}
            (_, Some(chosen)) if chosen == subscription => {
                form.account = match signed_in {
                    true => Account::SignedIn,
                    false => Account::SignedOut,
                };
            }
            (_, Some(_) | None) => {}
        }
    }

    /// Show what the current sign-in asks now.
    pub fn sign_in_step(&mut self, id: u64, step: SignInStep) {
        if let Account::Signing { id: current, .. } = self.polish.account
            && current == id
        {
            self.polish.account = Account::Signing {
                id,
                step: Some(step),
            };
        }
    }

    /// Record how the current sign-in ended; a failure says why.
    pub fn sign_in_ended(&mut self, id: u64, result: Result<(), String>) {
        let form = &mut self.polish;
        if let Account::Signing { id: current, .. } = form.account
            && current == id
        {
            (form.account, form.error) = match result {
                Ok(()) => (Account::SignedIn, None),
                Err(message) => (Account::SignedOut, Some(message)),
            };
        }
    }
}

impl ProviderForm {
    /// Ask whether the dialog's subscription is signed in to, and forget
    /// what was known of another.
    pub fn ask_account(&mut self, commands: &tokio::sync::mpsc::Sender<BridgeCommand>) {
        self.account = Account::Unknown;
        if let Some(subscription) = self.chosen_service.subscription() {
            let _ = commands.try_send(BridgeCommand::Model(ModelRequest::SignedIn {
                subscription,
            }));
        }
    }

    /// Abandon a sign-in under way, as closing the dialog does.
    pub(super) fn abandon_sign_in(&mut self, commands: &tokio::sync::mpsc::Sender<BridgeCommand>) {
        if let Account::Signing { .. } = self.account {
            let _ = commands.try_send(BridgeCommand::Model(ModelRequest::CancelSignIn));
            self.account = Account::Unknown;
        }
    }
}

/// Sign in to the subscription `role`'s dialog shows.
pub fn sign_in(view: &mut AppView, role: ModelRole) {
    let commands = view.commands.clone();
    let form = view.models.form(role);
    let Some(subscription) = form.chosen_service.subscription() else {
        return;
    };
    form.next_request += 1;
    let id = form.next_request;
    match commands.try_send(BridgeCommand::Model(ModelRequest::SignIn {
        subscription,
        id,
    })) {
        Ok(()) => {
            form.account = Account::Signing { id, step: None };
            form.error = None;
        }
        Err(error) => form.error = Some(error.to_string()),
    }
}

/// Stop the sign-in under way in `role`'s dialog.
pub fn cancel_sign_in(view: &mut AppView, role: ModelRole) {
    let commands = view.commands.clone();
    let form = view.models.form(role);
    form.abandon_sign_in(&commands);
    form.ask_account(&commands);
}

/// Sign out of the subscription `role`'s dialog shows.
pub fn sign_out(view: &mut AppView, role: ModelRole) {
    let commands = view.commands.clone();
    let form = view.models.form(role);
    if let Some(subscription) = form.chosen_service.subscription() {
        match commands.try_send(BridgeCommand::Model(ModelRequest::SignOut { subscription })) {
            Ok(()) => form.account = Account::Unknown,
            Err(error) => form.error = Some(error.to_string()),
        }
    }
}
