//! The model provider pages under Models in the sidebar.

use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, ensure};
use atspi::{Role, State};
use limen_core::{Config, Format, ProviderOptions, Service, v1};

use super::retype;
use crate::daemon::Scenario;
use crate::desktop::{Button, Key, Node};
use crate::session::{Session, Target};

/// An OpenAI-compatible server that lists `models`, transcribes every
/// take to nothing, and corrects every text; returns its address and the
/// requests it received.
fn serve_models(models: &'static [&'static str]) -> Result<(String, Arc<Mutex<Vec<String>>>)> {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = format!("http://{}", listener.local_addr()?);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let received = Arc::clone(&requests);
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut request = Vec::new();
            let mut chunk = [0; 4096];
            // Read the whole request, so an upload is never cut off.
            loop {
                let read = stream.read(&mut chunk).unwrap_or_default();
                request.extend_from_slice(&chunk[..read]);
                let text = String::from_utf8_lossy(&request);
                let complete = text.split_once("\r\n\r\n").is_some_and(|(head, _)| {
                    let length = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    request.len() >= head.len() + 4 + length
                });
                if read == 0 || complete {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&request).into_owned();
            let line = request.lines().next().unwrap_or_default().to_string();
            received.lock().unwrap().push(request);
            let body = match () {
                () if line.contains("/chat/completions") => r#"{"id":"r","object":"chat.completion","model":"m","choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"This is a test."}}]}"#.to_string(),
                () if line.contains("/audio/transcriptions") => r#"{"text":""}"#.to_string(),
                () => {
                    let data: Vec<String> = models
                        .iter()
                        .map(|model| format!(r#"{{"id":"{model}"}}"#))
                        .collect();
                    format!(r#"{{"object":"list","data":[{}]}}"#, data.join(","))
                }
            };
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

/// Open the dialog of the card `card`, such as `asr-provider-custom`, once
/// the page's settings have loaded.
fn open_card(session: &Session, card: &str, prefix: &str) -> Result<()> {
    let open = || {
        session.wait("the page", |nodes| {
            Some(nodes.iter().any(|node| node.id == format!("{prefix}-key")))
        })
    };
    // A card does nothing until the settings load; clicking it again while
    // its dialog opens would land on the backdrop and close it.
    session.until(&format!("{card} to open its dialog"), || {
        if open()? {
            return Ok(true);
        }
        session.click(card)?;
        std::thread::sleep(std::time::Duration::from_millis(400));
        open()
    })
}

/// The page's polishing switch.
const POLISHING: Target = Target::Named(Role::ToggleButton, "Polishing");

/// Turn the polishing switch on, which shows the providers to polish with.
fn show_polish_providers(session: &Session) -> Result<()> {
    session.until("the polishing providers to show", || {
        session.click(POLISHING)?;
        session.wait("the page", |nodes| {
            Some(nodes.iter().any(|node| node.id == "polish-provider-custom"))
        })
    })
}

/// Pick the type at `index` from a custom server's list; its items have no
/// accessible names. Both lists start with OpenAI-compatible; the ASR list
/// offers whisper.cpp next, the polishing list OpenAI.
fn choose_format(session: &Session, index: usize) -> Result<()> {
    session.click(Target::Named(Role::ComboBox, "Type"))?;
    let item = session.settled_by("a type in the list", |nodes| {
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

/// A custom whisper.cpp server, its key, and a typed model are saved from
/// the dialog together: the config holds the service, type, URL, and model,
/// the key goes to a private file of its own, the daemon reloads the
/// provider once, and the dialog closes.
pub fn an_asr_provider_is_saved_and_applied() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("asr-page")?;
    open_card(&session, "asr-provider-custom", "asr")?;
    choose_format(&session, 1)?;
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

/// A vendor needs only its key and model: its dialog asks for nothing else,
/// and saving it names the vendor.
pub fn a_vendor_is_chosen_from_its_card() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("asr-page")?;
    open_card(&session, "asr-provider-groq", "asr")?;
    session.absent("asr-url")?;
    session.click("asr-key")?;
    session.x11().type_text("sk-groq")?;
    type_model(&session, "whisper-large-v3")?;
    session.click("asr-save")?;
    session.named("toast", "ASR settings applied")?;
    let config = Config::load(&session.paths().config_file)?;
    ensure!(
        (
            config.provider.service,
            config.provider.model.as_str(),
            config.api_key().as_deref()
        ) == (Some(Service::Groq), "whisper-large-v3", Some("sk-groq")),
        "saved {:?}",
        config.provider
    );
    Ok(())
}

/// A URL that is not an http(s) address is reported in the dialog and
/// neither written nor sent to the daemon.
pub fn an_invalid_asr_server_is_reported() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("asr-page")?;
    open_card(&session, "asr-provider-custom", "asr")?;
    retype(&session, "asr-url", "ftp://10.0.0.2")?;
    type_model(&session, "typed-model")?;
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

/// With the switch on, a local polishing server is set up in its dialog,
/// asking for its models with the typed key, and saving it turns polishing
/// on.
pub fn polishing_is_set_up_with_a_local_server() -> Result<()> {
    let (server, requests) = serve_models(&["listed-a", "listed-b"])?;
    let session = Session::fake(Scenario::default())?;
    session.click("polish-page")?;
    show_polish_providers(&session)?;
    open_card(&session, "polish-provider-custom", "polish")?;
    retype(&session, "polish-url", &format!("{server}/v1"))?;
    session.click("polish-key")?;
    session.x11().type_text("sk-polish")?;
    // The list may arrive after the select first opens, and an open select
    // keeps its items: reopen until it offers both listed models.
    session.until("the listed models to be offered", || {
        session.click(Target::Named(Role::ComboBox, "Model"))?;
        session.node(Role::List)?;
        let offered = session.wait("the open list", |nodes| Some(list_items(nodes)))? == 2;
        session.chord(&[Key::ESCAPE])?;
        closed(&session)?;
        Ok(offered)
    })?;
    type_model(&session, "listed-b")?;
    session.click("polish-save")?;
    session.named("toast", "Polish settings applied")?;
    session.absent("polish-key")?;
    let config = Config::load(&session.paths().config_file)?;
    ensure!(
        (
            config.polish.service,
            config.polish.format,
            config.polish.url.as_str(),
            config.polish.model.as_str(),
            config.polish.api_key().as_deref()
        ) == (
            Some(Service::Custom),
            Some(Format::OpenAiCompatible),
            format!("{server}/v1").as_str(),
            "listed-b",
            Some("sk-polish")
        ),
        "saved {:?}",
        config.polish
    );
    ensure!(
        config.polish.enabled && reloads(&session, &v1::Request::PolishReload)? == 1,
        "polishing was not turned on with one save"
    );
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

/// With polishing off the page offers no providers; the switch shows them
/// and saves nothing until one is set up.
pub fn the_switch_shows_the_providers() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("polish-page")?;
    session.node(POLISHING)?;
    session.absent("polish-provider-custom")?;
    show_polish_providers(&session)?;
    ensure!(
        !session.paths().config_file.exists(),
        "turning the switch on with no provider saved something"
    );
    Ok(())
}

/// A saved key can be removed; the other provider's key stays.
pub fn a_saved_key_is_removed() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    let keys = session.paths().config_file.with_file_name("keys.toml");
    std::fs::create_dir_all(keys.parent().unwrap())?;
    std::fs::write(&keys, "asr = \"sk-asr\"\npolish = \"sk-polish\"\n")?;
    std::fs::write(
        &session.paths().config_file,
        "[provider]\nurl = \"\"\n\n[polish]\nenabled = true\nservice = \"custom\"\nurl = \"http://127.0.0.1:9/v1\"\nmodel = \"m\"\n",
    )?;
    session.click("polish-page")?;
    open_card(&session, "polish-provider-custom", "polish")?;
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
    show_polish_providers(&session)?;
    open_card(&session, "polish-provider-custom", "polish")?;
    retype(
        &session,
        "polish-url",
        "https://ai.example/v1?api_key=do-not-save",
    )?;
    session.click("polish-save")?;
    let error = session.node("polish-error")?;
    ensure!(
        error.name.contains("non-secret query parameters"),
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

/// Editing while a save is in flight keeps the newer text and the dialog
/// open, so the edits can be saved next.
pub fn edits_made_while_saving_survive() -> Result<()> {
    let session = Session::fake(Scenario {
        provider_reload_ms: 1000,
        ..Scenario::default()
    })?;
    session.click("asr-page")?;
    open_card(&session, "asr-provider-custom", "asr")?;
    retype(&session, "asr-url", "http://a/v1")?;
    type_model(&session, "typed-model")?;
    session.click("asr-save")?;
    retype(&session, "asr-url", "http://b/v1")?;
    session.named("toast", "ASR settings applied")?;
    session.click("asr-save")?;
    session.until("the edits made during the first save to be saved", || {
        let url = Config::load(&session.paths().config_file)
            .map(|config| config.provider.url)
            .unwrap_or_default();
        Ok(url == "http://b/v1" && reloads(&session, &v1::Request::ProviderReload)? == 2)
    })?;
    Ok(())
}

/// A vendor's dialog has no server to name, and a custom server's does.
pub fn only_a_custom_server_has_a_url() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("polish-page")?;
    show_polish_providers(&session)?;
    open_card(&session, "polish-provider-anthropic", "polish")?;
    session.absent("polish-url")?;
    session.chord(&[Key::ESCAPE])?;
    session.absent("polish-key")?;
    open_card(&session, "polish-provider-custom", "polish")?;
    session.node("polish-url")?;
    Ok(())
}

/// The model is not filled in until there is a server to ask: typing one
/// that lists a single model chooses it, and saving keeps it.
pub fn the_model_is_detected_from_the_typed_server() -> Result<()> {
    let (server, requests) = serve_models(&["only-asr"])?;
    let session = Session::fake(Scenario::default())?;
    session.click("asr-page")?;
    open_card(&session, "asr-provider-custom", "asr")?;
    ensure!(
        requests.lock().unwrap().is_empty(),
        "a server was asked before one was entered"
    );
    retype(&session, "asr-url", &format!("{server}/v1"))?;
    // Saving closes the dialog once the detected model is in it.
    session.until("the detected model to be saved", || {
        if Config::load(&session.paths().config_file)
            .is_ok_and(|config| config.provider.model == "only-asr")
        {
            return Ok(true);
        }
        session.click("asr-save")?;
        Ok(false)
    })?;
    let provider = Config::load(&session.paths().config_file)?.provider;
    let asked = requests.lock().unwrap().clone();
    ensure!(
        provider.url == format!("{server}/v1")
            && asked
                .iter()
                .any(|request| request.starts_with("GET /v1/models ")),
        "saved {:?}; the server was asked {asked:?}",
        provider.url
    );
    Ok(())
}

/// Detect asks for the models at once, and says so when there is no
/// server to ask.
pub fn the_models_are_detected_on_request() -> Result<()> {
    let (server, requests) = serve_models(&["listed-a", "listed-b"])?;
    let session = Session::fake(Scenario::default())?;
    session.click("asr-page")?;
    open_card(&session, "asr-provider-custom", "asr")?;
    session.click("asr-detect")?;
    session.named("asr-error", "Enter the server first")?;
    retype(&session, "asr-url", &format!("{server}/v1"))?;
    session.click("asr-detect")?;
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
            .any(|request| request.starts_with("GET /v1/models ")),
        "the server was asked {asked:?}"
    );
    Ok(())
}

/// Test asks the chosen model to do its job with the fields as typed, and
/// saves nothing.
pub fn a_model_is_tested_before_it_is_saved() -> Result<()> {
    let (server, requests) = serve_models(&["only-model"])?;
    let session = Session::fake(Scenario::default())?;
    session.click("asr-page")?;
    open_card(&session, "asr-provider-custom", "asr")?;
    retype(&session, "asr-url", &format!("{server}/v1"))?;
    // Test turns on once the server's only model fills the field.
    session.state("asr-test", State::Focusable, true)?;
    session.click("asr-test")?;
    session.named("toast", "The ASR model answered")?;
    session.chord(&[Key::ESCAPE])?;
    session.absent("asr-key")?;
    session.click("polish-page")?;
    show_polish_providers(&session)?;
    open_card(&session, "polish-provider-custom", "polish")?;
    retype(&session, "polish-url", &format!("{server}/v1"))?;
    session.state("polish-test", State::Focusable, true)?;
    session.click("polish-test")?;
    session.named("toast", "The polish model answered")?;
    let asked = requests.lock().unwrap().clone();
    ensure!(
        [
            "POST /v1/audio/transcriptions ",
            "POST /v1/chat/completions "
        ]
        .iter()
        .all(|route| asked.iter().any(|request| request.starts_with(route))),
        "the server was asked {:?}",
        asked
            .iter()
            .map(|request| request.lines().next().unwrap_or_default())
            .collect::<Vec<_>>()
    );
    ensure!(
        !session.paths().config_file.exists(),
        "testing a model saved it"
    );
    Ok(())
}

/// A provider's advanced settings, unfolded and scrolled to in its
/// dialog, are saved under its own table.
pub fn advanced_settings_belong_to_their_provider() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("asr-page")?;
    open_card(&session, "asr-provider-custom", "asr")?;
    retype(&session, "asr-url", "http://10.0.0.2:9000/v1")?;
    session.click("asr-advanced")?;
    retype(&session, "asr-route", "/v2/transcribe")?;
    retype(&session, "asr-key-header", "api-key")?;
    retype(&session, "asr-headers", "X-Org: limen")?;
    // The advanced settings run past the dialog's height: scroll to the
    // rest over a row's label, as a user does; an input keeps the wheel for
    // its own text.
    let route = session
        .settled("asr-route")?
        .bounds
        .context("the route field has no bounds")?;
    let (_, y) = route.center();
    session
        .x11()
        .scroll(route.x - 60, y, Button::WheelDown, 10)?;
    retype(&session, "asr-timeout", "8000")?;
    type_model(&session, "typed-model")?;
    session.click("asr-save")?;
    session.named("toast", "ASR settings applied")?;
    session.absent("asr-key")?;
    let options = Config::load(&session.paths().config_file)?.provider.options;
    ensure!(
        options
            == [(
                Service::Custom,
                ProviderOptions {
                    path: Some("/v2/transcribe".to_string()),
                    key_header: Some("api-key".to_string()),
                    timeout_ms: Some(8000),
                    headers: [("X-Org".to_string(), "limen".to_string())].into(),
                    ..ProviderOptions::default()
                }
            )]
            .into(),
        "saved {options:?}"
    );
    Ok(())
}
