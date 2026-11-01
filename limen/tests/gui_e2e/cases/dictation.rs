//! The dictation control strip and the request list below it.

use anyhow::{Context, Result, ensure};
use atspi::Role;
use limen_core::{Config, Delivery, v1};

use super::{failed_request, retype};
use crate::daemon::Scenario;
use crate::desktop::{Button, Key};
use crate::session::{Session, Target};

/// Starting records; stopping transcribes and keeps the text, which lands
/// on the clipboard with a toast instead of being typed.
pub fn start_and_stop_copy_the_transcript() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.named("dictation-status", "Idle")?;
    session.click("dictation-primary")?;
    session.named("dictation-status", "Recording")?;
    session.named("dictation-primary", "Stop and transcribe")?;
    session.click("dictation-primary")?;
    session.named("dictation-status", "Idle")?;
    session.clipboard("e2e transcript 1")?;
    session.named("toast", "Copied to clipboard")?;
    ensure!(
        session.requests()?
            == [
                v1::Request::Start,
                v1::Request::Stop {
                    delivery: Delivery::Keep
                }
            ],
        "unexpected requests"
    );
    Ok(())
}

/// An error long enough to need several lines once unfolded.
const LONG_ERROR: &str = "provider returned 503: the upstream speech service is overloaded \
     and refused the request; retry after the queue drains or switch to another \
     endpoint in the configuration file";

/// The height of a node once it has settled.
fn height(session: &Session, id: &str) -> Result<i32> {
    Ok(session.settled(id)?.bounds.context("no bounds")?.height)
}

/// A failed request shows one line of its error until pressed, then all of
/// it; pressing again folds it back. AccessKit's AT-SPI adapter drops the
/// expanded state, so the row's height tells.
pub fn failed_requests_expand_to_the_full_error() -> Result<()> {
    let request = failed_request(1, LONG_ERROR);
    let id = format!("request-1-{}", request.started_ms);
    let session = Session::fake(Scenario {
        requests: vec![request],
        ..Scenario::default()
    })?;
    let row = session.node(id.as_str())?;
    ensure!(
        row.description == LONG_ERROR,
        "the row describes {:?}",
        row.description
    );
    let folded = height(&session, &id)?;
    session.click(id.as_str())?;
    let unfolded = height(&session, &id)?;
    ensure!(
        unfolded > folded + 20,
        "the row grew from {folded}px to {unfolded}px"
    );
    session.click(id.as_str())?;
    ensure!(height(&session, &id)? == folded, "the row did not fold");
    Ok(())
}

/// The wheel scrolls a long request list, bringing the oldest into view.
pub fn the_request_list_scrolls_with_the_wheel() -> Result<()> {
    let requests = (1..=20)
        .map(|take| failed_request(take, LONG_ERROR))
        .collect::<Vec<_>>();
    let (newest, oldest) = (
        format!("request-20-{}", requests[19].started_ms),
        format!("request-1-{}", requests[0].started_ms),
    );
    let session = Session::fake(Scenario {
        requests,
        ..Scenario::default()
    })?;
    let before = session
        .settled(oldest.as_str())?
        .bounds
        .context("no bounds")?;
    let (x, y) = session
        .settled(newest.as_str())?
        .bounds
        .context("no bounds")?
        .center();
    session.x11().scroll(x, y, Button::WheelDown, 10)?;
    let after = session
        .settled(oldest.as_str())?
        .bounds
        .context("no bounds")?;
    ensure!(
        after.y + 200 < before.y,
        "the list did not scroll: {before:?} then {after:?}"
    );
    let folded = height(&session, &oldest)?;
    session.click(oldest.as_str())?;
    ensure!(
        height(&session, &oldest)? > folded + 20,
        "the oldest request did not unfold"
    );
    Ok(())
}

/// While a take transcribes, the button records another after it: the
/// request queues a take, the button waits, and recording resumes once
/// the first transcript lands. Transcription lasts long enough that the
/// press on the waiting button cannot land after it ends.
pub fn recording_another_take_queues_it() -> Result<()> {
    let session = Session::fake(Scenario {
        transcribe_ms: 10_000,
        ..Scenario::default()
    })?;
    session.click("dictation-primary")?;
    session.named("dictation-status", "Recording")?;
    session.click("dictation-primary")?;
    session.named("dictation-status", "Transcribing…")?;
    session.named("dictation-primary", "Record another after this take")?;
    session.click("dictation-primary")?;
    session.named("dictation-primary", "Another take queued")?;
    session.click("dictation-primary")?;
    session.named("dictation-status", "Recording")?;
    session.clipboard("e2e transcript 1")?;
    ensure!(
        session.requests()?
            == [
                v1::Request::Start,
                v1::Request::Stop {
                    delivery: Delivery::Keep
                },
                v1::Request::Start,
            ],
        "the queued button sent a request"
    );
    Ok(())
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
    session.wait("the list to close", |nodes| {
        (!nodes.iter().any(|node| node.role == Role::List)).then_some(())
    })
}

fn provider_reloads(session: &Session) -> Result<usize> {
    Ok(session
        .requests()?
        .iter()
        .filter(|request| **request == v1::Request::ProviderReload)
        .count())
}

/// A server and a route from the list land in the config file and the
/// running daemon is asked to reload them. The fake daemon's route is not in
/// the list, so one must be chosen before Enter applies.
pub fn the_asr_server_and_route_are_saved_and_applied() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    choose_route(&session, 1)?;
    retype(&session, "provider-server", "http://10.0.0.2:9000")?;
    session.chord(&[Key::RETURN])?;
    session.named("toast", "ASR server applied")?;
    let config = Config::load(&session.paths().config_file)?;
    ensure!(
        config.provider.url == "http://10.0.0.2:9000/inference",
        "the config holds {:?}",
        config.provider.url
    );
    ensure!(
        provider_reloads(&session)? == 1,
        "the daemon was not asked to reload"
    );
    Ok(())
}

/// A server that is not a bare http(s) address is reported beside the field
/// and neither written nor sent to the daemon.
pub fn an_invalid_asr_server_is_reported() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    choose_route(&session, 0)?;
    retype(&session, "provider-server", "ftp://10.0.0.2")?;
    session.click("provider-apply")?;
    let error = session.node("provider-error")?;
    ensure!(
        error.name.contains("http(s)://"),
        "the error says {:?}",
        error.name
    );
    ensure!(
        provider_reloads(&session)? == 0,
        "an invalid server was applied"
    );
    ensure!(
        !session.paths().config_file.exists(),
        "an invalid server was written"
    );
    Ok(())
}

/// A model name the servers do not list is typed into each model's search
/// and applied at once: the config holds it and the daemon reloads it.
pub fn typed_models_are_saved_and_applied() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    for (select, model, toast) in [
        ("ASR model", "Qwen/Qwen3-ASR-1.7B", "ASR model applied"),
        ("Polish model", "deepseek-chat", "Polish model applied"),
    ] {
        session.click(Target::Named(Role::ComboBox, select))?;
        session.node(Role::List)?;
        session.x11().type_text(model)?;
        session.wait(&format!("only {model} offered"), |nodes| {
            // List items carry no name; the typed one is the only match.
            (nodes
                .iter()
                .filter(|node| node.role == Role::ListItem)
                .count()
                == 1)
                .then_some(())
        })?;
        session.chord(&[Key::RETURN])?;
        session.named("toast", toast)?;
    }
    let config = Config::load(&session.paths().config_file)?;
    ensure!(
        (config.provider.model.as_str(), config.polish.model.as_str())
            == ("Qwen/Qwen3-ASR-1.7B", "deepseek-chat"),
        "the config holds {:?} and {:?}",
        config.provider.model,
        config.polish.model
    );
    let reloads: Vec<_> = session
        .requests()?
        .into_iter()
        .filter(|request| {
            matches!(
                request,
                v1::Request::ProviderReload | v1::Request::PolishReload
            )
        })
        .collect();
    ensure!(
        reloads == [v1::Request::ProviderReload, v1::Request::PolishReload],
        "the daemon was asked for {reloads:?}"
    );
    Ok(())
}
