//! The audio requests: the daemon's plugin library, the routing graph, and
//! the test recording.

use limen_core::{AudioGraphView, PluginLibrary, TestFiles, request, v1};
use plugin_graph::{GraphSpec, NodeId};
use tokio::sync::mpsc;

use super::lifecycle::is_outdated;
use super::{BridgeEvent, Env};

/// What is asked of the plugin library. Only a scan, adding a bundle or
/// restoring what was removed reads any plugin; the rest read and write the
/// library file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryRequest {
    /// Read the library as saved.
    Get,
    /// Look for plugins in these folders from now on.
    Folders(Vec<String>),
    /// Read the folders and added bundles again.
    Scan,
    /// Add the plugin bundle at this path.
    Add(String),
    /// Take a plugin out of the library.
    Remove { path: String, class_id: String },
    /// Bring back the plugins that were taken out.
    Restore,
}

impl From<LibraryRequest> for v1::Request {
    fn from(request: LibraryRequest) -> Self {
        match request {
            LibraryRequest::Get => Self::AudioLibraryGet,
            LibraryRequest::Folders(folders) => Self::AudioLibraryFolders { folders },
            LibraryRequest::Scan => Self::AudioLibraryScan,
            LibraryRequest::Add(path) => Self::AudioLibraryAdd { path },
            LibraryRequest::Remove { path, class_id } => {
                Self::AudioLibraryRemove { path, class_id }
            }
            LibraryRequest::Restore => Self::AudioLibraryRestore,
        }
    }
}

/// A request about audio; `id` comes back in the reply so an
/// older one cannot replace a newer one.
#[derive(Debug, Clone, PartialEq)]
pub enum AudioRequest {
    /// Read the saved graph and the state of its nodes.
    Get { id: u64 },
    /// Save a graph, for the next take on.
    Set { id: u64, graph: GraphSpec },
    /// Something about the plugin library.
    Library { id: u64, request: LibraryRequest },
    /// Show a plugin's own editor on the daemon's display.
    OpenEditor { node: NodeId },
    /// Record the microphone for `seconds`, then process the recording.
    Test { id: u64, seconds: u32 },
}

/// Start `audio` off the bridge loop; its reply arrives on `events`.
pub(super) fn handle(env: &Env, events: &mpsc::Sender<BridgeEvent>, audio: AudioRequest) {
    let (paths, events) = (env.paths.clone(), events.clone());
    tokio::spawn(async move {
        let event = match audio {
            AudioRequest::Get { id } => BridgeEvent::AudioGraph {
                id,
                result: graph(request(&paths, &v1::Request::AudioGraphGet).await),
            },
            AudioRequest::Set { id, graph: wanted } => BridgeEvent::AudioGraph {
                id,
                result: graph(request(&paths, &v1::Request::AudioGraphSet { graph: wanted }).await),
            },
            AudioRequest::Library {
                id,
                request: wanted,
            } => BridgeEvent::AudioLibrary {
                id,
                result: library(request(&paths, &wanted.into()).await),
            },
            AudioRequest::OpenEditor { node } => BridgeEvent::AudioEditor {
                result: ok(request(&paths, &v1::Request::AudioEditorOpen { node }).await),
            },
            AudioRequest::Test { id, seconds } => BridgeEvent::AudioTest {
                id,
                result: test(request(&paths, &v1::Request::AudioTest { seconds }).await),
            },
        };
        let _ = events.send(event).await;
    });
}

const UNEXPECTED: &str = "the daemon answered something else";

/// A daemon too old to know the request refuses it as invalid; the page says
/// the background service is outdated, as it does for the rest of the app.
fn refusal(message: String) -> String {
    match is_outdated(&message) {
        true => format!("{}: {message}", rust_i18n::t!("connection.outdated")),
        false => message,
    }
}

fn graph(reply: anyhow::Result<v1::Reply>) -> Result<AudioGraphView, String> {
    match reply.map_err(|error| format!("{error:#}"))? {
        v1::Reply::AudioGraph(view) => Ok(view),
        v1::Reply::Error { message } => Err(refusal(message)),
        other => Err(format!("{UNEXPECTED}: {other:?}")),
    }
}

fn library(reply: anyhow::Result<v1::Reply>) -> Result<PluginLibrary, String> {
    match reply.map_err(|error| format!("{error:#}"))? {
        v1::Reply::AudioLibrary(library) => Ok(library),
        v1::Reply::Error { message } => Err(refusal(message)),
        other => Err(format!("{UNEXPECTED}: {other:?}")),
    }
}

fn ok(reply: anyhow::Result<v1::Reply>) -> Result<(), String> {
    match reply.map_err(|error| format!("{error:#}"))? {
        v1::Reply::Ok => Ok(()),
        v1::Reply::Error { message } => Err(refusal(message)),
        other => Err(format!("{UNEXPECTED}: {other:?}")),
    }
}

fn test(reply: anyhow::Result<v1::Reply>) -> Result<TestFiles, String> {
    match reply.map_err(|error| format!("{error:#}"))? {
        v1::Reply::AudioTest(files) => Ok(files),
        v1::Reply::Error { message } => Err(refusal(message)),
        other => Err(format!("{UNEXPECTED}: {other:?}")),
    }
}
