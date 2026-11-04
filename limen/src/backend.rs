//! The program's model backend: rig for the APIs it reaches, genai for the
//! chat vendors only genai does, both priced from rig's catalog.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use limen_core::{
    BoxFuture, ChatModel, Connection, ModelBackend, Proxy, SignInPrompt, Subscription, Transcriber,
};
use limen_genai::Genai;
use limen_rig::Rig;

pub struct Backends {
    rig: Rig,
    genai: Genai,
}

impl Backends {
    /// The backends, with subscriptions' sign-ins kept under `sign_ins`.
    pub fn new(sign_ins: PathBuf) -> Self {
        Self {
            rig: Rig::new(sign_ins),
            genai: Genai::new(Arc::new(limen_rig::Catalog)),
        }
    }

    fn reaching(&self, connection: &Connection) -> &dyn ModelBackend {
        match Genai::serves(connection.api) {
            true => &self.genai,
            false => &self.rig,
        }
    }
}

impl ModelBackend for Backends {
    fn transcriber(&self, connection: Connection, model: String) -> Result<Box<dyn Transcriber>> {
        self.reaching(&connection).transcriber(connection, model)
    }

    fn chat(&self, connection: Connection, model: String) -> Result<Box<dyn ChatModel>> {
        self.reaching(&connection).chat(connection, model)
    }

    fn models(&self, connection: Connection) -> BoxFuture<'_, Result<Vec<String>>> {
        self.reaching(&connection).models(connection)
    }

    // Subscriptions are signed in to through rig alone.
    fn sign_in(
        &self,
        subscription: Subscription,
        proxy: Proxy,
        prompt: SignInPrompt,
    ) -> BoxFuture<'_, Result<()>> {
        self.rig.sign_in(subscription, proxy, prompt)
    }

    fn sign_out(&self, subscription: Subscription, proxy: Proxy) -> BoxFuture<'_, Result<()>> {
        self.rig.sign_out(subscription, proxy)
    }

    fn signed_in(&self, subscription: Subscription) -> bool {
        self.rig.signed_in(subscription)
    }
}
