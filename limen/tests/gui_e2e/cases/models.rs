//! The model provider pages under Models in the sidebar.

use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use anyhow::{Result, ensure};
use atspi::{Role, State};
use limen_core::{Config, v1};

use super::retype;
use crate::daemon::Scenario;
use crate::desktop::{Key, Node};
use crate::session::{Session, Target};

/// Answer every connection with an OpenAI-compatible list of `models`, and
/// return the server's address and the requests it received.
fn serve_models(models: &'static [&'static str]) -> Result<(String, Arc<Mutex<Vec<String>>>)> {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = format!("http://{}", listener.local_addr()?);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let received = Arc::clone(&requests);
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut request = [0; 4096];
            let read = stream.read(&mut request).unwrap_or_default();
            received
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&request[..read]).into_owned());
            let data: Vec<String> = models
                .iter()
                .map(|model| format!(r#"{{"id":"{model}"}}"#))
                .collect();
            let body = format!(r#"{{"object":"list","data":[{}]}}"#, data.join(","));
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    Ok((address, requests))
}

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

/// Pick the route at `index` from the list; its items have no accessible
/// names, and the list offers `/v1/audio/transcriptions`, then `/inference`.
fn choose_route(session: &Session, index: usize) -> Result<()> {
    session.click(Target::Named(Role::ComboBox, "Route"))?;
    let item = session.settled_by("a route in the list", |nodes| {
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

fn reloads(session: &Session, request: &v1::Request) -> Result<usize> {
    Ok(session
        .requests()?
        .iter()
        .filter(|sent| *sent == request)
        .count())
}

/// The Models button on the dictation page opens the ASR page.
pub fn the_dictation_button_opens_the_asr_page() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("open-models")?;
    session.state("asr-page", State::Selected, true)?;
    session.named("current-page", "ASR")?;
    Ok(())
}

/// A server, a route, a key, and a typed model are saved together: the
/// config holds the URL and model, the key goes to a private file of its
/// own, and the daemon reloads the provider once.
pub fn an_asr_provider_is_saved_and_applied() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("asr-page")?;
    choose_route(&session, 1)?;
    retype(&session, "asr-server", "http://10.0.0.2:9000")?;
    session.click("asr-key")?;
    session.x11().type_text("sk-asr")?;
    type_model(&session, "Qwen/Qwen3-ASR-1.7B")?;
    session.click("asr-save")?;
    session.named("toast", "ASR settings applied")?;
    let config_file = &session.paths().config_file;
    let config = Config::load(config_file)?;
    let keys = config_file.with_file_name("keys.toml");
    let mode = std::fs::metadata(&keys)?.permissions().mode() & 0o777;
    ensure!(
        (
            config.provider.url.as_str(),
            config.provider.model.as_str(),
            config.api_key().as_deref(),
            mode,
        ) == (
            "http://10.0.0.2:9000/inference",
            "Qwen/Qwen3-ASR-1.7B",
            Some("sk-asr"),
            0o600
        ),
        "saved {:?}, {:?}, a key {:?} in mode {mode:o}",
        config.provider.url,
        config.provider.model,
        config.api_key(),
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

/// A server that is not a bare http(s) address is reported under the page
/// and neither written nor sent to the daemon.
pub fn an_invalid_asr_server_is_reported() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("asr-page")?;
    retype(&session, "asr-server", "ftp://10.0.0.2")?;
    session.click("asr-save")?;
    let error = session.node("asr-error")?;
    ensure!(
        error.name.contains("http(s)://"),
        "the error says {:?}",
        error.name
    );
    ensure!(
        reloads(&session, &v1::Request::ProviderReload)? == 0,
        "an invalid server was applied"
    );
    ensure!(
        !session.paths().config_file.exists(),
        "an invalid server was written"
    );
    Ok(())
}

/// Polishing is turned on with a local server and a typed model. Once
/// saved, the page lists the models the new server offers, asking with the
/// saved key.
pub fn polishing_is_set_up_with_a_local_server() -> Result<()> {
    let (server, requests) = serve_models(&["listed-a", "listed-b"])?;
    let session = Session::fake(Scenario::default())?;
    session.click("polish-page")?;
    session.click("polishing-on")?;
    retype(
        &session,
        "polish-url",
        &format!("{server}/v1/chat/completions"),
    )?;
    session.click("polish-key")?;
    session.x11().type_text("sk-polish")?;
    type_model(&session, "listed-b")?;
    session.click("polish-save")?;
    session.named("toast", "Polish settings applied")?;
    let config = Config::load(&session.paths().config_file)?;
    ensure!(
        (
            config.polish.enabled,
            config.polish.model.as_str(),
            config.polish.api_key().as_deref()
        ) == (true, "listed-b", Some("sk-polish")),
        "saved {:?}",
        config.polish
    );
    ensure!(
        reloads(&session, &v1::Request::PolishReload)? == 1,
        "the daemon was not asked to reload polishing once"
    );
    // The list may arrive after the select first opens, and an open select
    // keeps its items: reopen until it offers the chosen and the other model.
    session.until("the listed models to be offered", || {
        session.click(Target::Named(Role::ComboBox, "Model"))?;
        session.node(Role::List)?;
        let offered = session.wait("the open list", |nodes| Some(list_items(nodes)))? == 2;
        session.chord(&[Key::ESCAPE])?;
        closed(&session)?;
        Ok(offered)
    })?;
    let asked = requests.lock().unwrap().clone();
    ensure!(
        asked
            .iter()
            .any(|request| request.starts_with("GET /v1/models ")
                && request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer sk-polish")),
        "the server was asked {asked:?}"
    );
    Ok(())
}

/// A saved key can be removed; the other provider's key stays.
pub fn a_saved_key_is_removed() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    let keys = session.paths().config_file.with_file_name("keys.toml");
    std::fs::create_dir_all(keys.parent().unwrap())?;
    std::fs::write(&keys, "asr = \"sk-asr\"\npolish = \"sk-polish\"\n")?;
    session.click("polish-page")?;
    session.click("polish-key-remove")?;
    session.click("polish-save")?;
    session.named("toast", "Polish settings applied")?;
    let left = std::fs::read_to_string(&keys)?;
    ensure!(left == "asr = \"sk-asr\"\n", "the key file holds {left:?}");
    Ok(())
}

/// Credentials belong in the key field, not in a provider URL. Rejecting
/// the URL writes neither the config nor the key file.
pub fn a_url_credential_is_rejected() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("polish-page")?;
    retype(
        &session,
        "polish-url",
        "https://ai.example/v1/chat/completions?api_key=do-not-save",
    )?;
    session.click("polish-save")?;
    let error = session.node("polish-error")?;
    ensure!(
        error.name.contains("must not contain credentials"),
        "the error says {:?}",
        error.name
    );
    ensure!(
        !session.paths().config_file.exists()
            && !session
                .paths()
                .config_file
                .with_file_name("keys.toml")
                .exists(),
        "a rejected provider was written"
    );
    ensure!(
        reloads(&session, &v1::Request::PolishReload)? == 0,
        "a rejected provider was applied"
    );
    Ok(())
}

/// Editing while a save is in flight keeps the newer text. The first
/// success neither clears it nor replaces it with a reload, so it can be
/// saved next.
pub fn edits_made_while_saving_survive() -> Result<()> {
    let session = Session::fake(Scenario {
        provider_reload_ms: 2000,
        ..Scenario::default()
    })?;
    session.click("asr-page")?;
    choose_route(&session, 0)?;
    retype(&session, "asr-server", "http://10.0.0.2:9000")?;
    session.click("asr-save")?;
    retype(&session, "asr-server", "http://10.0.0.3:9000")?;
    session.named("toast", "ASR settings applied")?;
    session.click("asr-save")?;
    session.until("the edits made during the first save to be saved", || {
        let url = Config::load(&session.paths().config_file)
            .map(|config| config.provider.url)
            .unwrap_or_default();
        Ok(url == "http://10.0.0.3:9000/v1/audio/transcriptions"
            && reloads(&session, &v1::Request::ProviderReload)? == 2)
    })?;
    Ok(())
}
