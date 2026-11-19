//! The model provider pages under Models in the sidebar. One custom server is
//! saved through a real daemon onto disk: the config file, the private key
//! file, and the reload the daemon is asked for are what only the whole chain
//! shows. The rest of the pages, their cards, dialogs, and the requests they
//! send, are tested in-process in limen-gui's UI tests.

use std::os::unix::fs::PermissionsExt;

use anyhow::{Result, ensure};
use atspi::Role;
use limen_core::{Config, Format, Service, v1};

use super::{reloads, retype};
use crate::daemon::Scenario;
use crate::desktop::{Key, Node};
use crate::session::{Session, Target};

fn list_items(nodes: &[Node]) -> usize {
    nodes
        .iter()
        .filter(|node| node.role == Role::ListItem)
        .count()
}

/// Wait for the open list of a select to close.
fn closed(session: &Session) -> Result<()> {
    session.wait("the list to close", |nodes| {
        (!nodes.iter().any(|node| node.role == Role::List)).then_some(())
    })
}

/// Open the dialog of the card `card`, such as `asr-provider-custom`, once
/// the page's settings have loaded, unfolding the services other than the
/// configured one when the card is among them.
fn open_card(session: &Session, card: &str, prefix: &str) -> Result<()> {
    let open = || {
        session.wait("the page", |nodes| {
            Some(nodes.iter().any(|node| node.id == format!("{prefix}-save")))
        })
    };
    // A card does nothing until the settings load; clicking it again while
    // its dialog opens would land on the backdrop and close it.
    session.until(&format!("{card} to open its dialog"), || {
        if open()? {
            return Ok(true);
        }
        let shown = session.wait("the page", |nodes| {
            Some(nodes.iter().any(|node| node.id == card))
        })?;
        match shown {
            true => session.click(card)?,
            false => session.click(format!("{prefix}-others").as_str())?,
        };
        std::thread::sleep(std::time::Duration::from_millis(400));
        open()
    })
}

/// Pick the item at `index` from the list of the select named `select`;
/// its items have no accessible names. Both type lists start with
/// OpenAI-compatible; the ASR list offers whisper.cpp next, the polishing
/// list OpenAI. The chat API list offers Chat Completions, then Responses.
fn choose(session: &Session, select: &str, index: usize) -> Result<()> {
    session.click(Target::Named(Role::ComboBox, select))?;
    let item = session.settled_by("an item in the list", |nodes| {
        nodes
            .iter()
            .filter(|node| node.role == Role::ListItem)
            .nth(index)
            .cloned()
    })?;
    session.click_node(&item)?;
    closed(session)
}

/// Type `model` into the model select's search, which then offers only it,
/// and choose it.
fn type_model(session: &Session, model: &str) -> Result<()> {
    session.click(Target::Named(Role::ComboBox, "Model"))?;
    session.node(Role::List)?;
    session.x11().type_text(model)?;
    session.wait(&format!("only {model} offered"), |nodes| {
        (list_items(nodes) == 1).then_some(())
    })?;
    session.chord(&[Key::RETURN])?;
    closed(session)
}

/// A custom whisper.cpp server, its key, and a typed model are saved from
/// the dialog together: the config holds the service, type, URL, and model,
/// the key goes to a private file of its own, the daemon reloads the
/// provider once, and the dialog closes.
pub fn an_asr_provider_is_saved_and_applied() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("providers-page")?;
    open_card(&session, "asr-provider-custom", "asr")?;
    choose(&session, "Type", 1)?;
    retype(&session, "asr-url", "http://10.0.0.2:9000/inference")?;
    session.click("asr-key")?;
    session.x11().type_text("sk-asr")?;
    type_model(&session, "Qwen/Qwen3-ASR-1.7B")?;
    session.click("asr-save")?;
    session.named("toast", "ASR settings applied")?;
    session.absent("asr-key")?;
    let config_file = &session.paths().config_file;
    let config = Config::load(config_file)?;
    let keys = config_file.with_file_name("keys.toml");
    let mode = std::fs::metadata(&keys)?.permissions().mode() & 0o777;
    ensure!(
        (
            config.provider.service,
            config.provider.format,
            config.provider.url.as_str(),
            config.provider.model.as_str(),
            config.api_key().as_deref(),
            mode,
        ) == (
            Some(Service::Custom),
            Some(Format::WhisperCpp),
            "http://10.0.0.2:9000/inference",
            "Qwen/Qwen3-ASR-1.7B",
            Some("sk-asr"),
            0o600
        ),
        "saved {:?}",
        config.provider
    );
    ensure!(
        !std::fs::read_to_string(config_file)?.contains("sk-asr"),
        "the key leaked into the config"
    );
    ensure!(
        reloads(&session, &v1::Request::ProviderReload)? == 1,
        "the daemon was not asked to reload once"
    );
    Ok(())
}
