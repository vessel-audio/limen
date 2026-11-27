//! Protocol v1: one JSON object per line, tagged by `type`. Requests carry
//! the protocol version as `"v":1`; replies are unversioned.
//!
//! | Request                                  | Reply                                    |
//! |------------------------------------------|------------------------------------------|
//! | `{"v":1,"type":"hold","edge":"press"}`   | `{"type":"ok"}`, before the take starts  |
//! | `{"v":1,"type":"hold","edge":"release"}` | `{"type":"ok"}`                          |
//! | `{"v":1,"type":"start"}`                 | `{"type":"accepted","take":N}`          |
//! | `{"v":1,"type":"stop","delivery":...}`   | `{"type":"accepted","take":N}`          |
//! | `{"v":1,"type":"clear_history"}`         | `{"type":"ok"}`                          |
//! | `{"v":1,"type":"shutdown"}`              | `{"type":"ok"}`, then stop               |
//! | `{"v":1,"type":"subscribe"}`             | hello, snapshots, and levels             |
//! | `{"v":1,"type":"shortcut_rebind"}`       | `{"type":"ok"}` once the new state shows |
//! | `{"v":1,"type":"shortcut_status"}`       | `{"type":"shortcut_status",...}`         |
//! | `{"v":1,"type":"provider_reload"}`       | `{"type":"ok"}`, for the next take       |
//! | `{"v":1,"type":"polish_reload"}`         | `{"type":"ok"}`, for the next transcript |
//! | `{"v":1,"type":"audio_scan"}`            | `{"type":"audio_plugins",...}`           |
//! | `{"v":1,"type":"audio_graph_get"}`       | `{"type":"audio_graph",...}`             |
//! | `{"v":1,"type":"audio_graph_set",...}`   | `{"type":"audio_graph",...}`, next take  |
//! | `{"v":1,"type":"audio_editor_open",...}` | `{"type":"ok"}`, once the window opens   |
//! | `{"v":1,"type":"audio_test","seconds":N}`| `{"type":"audio_test",...}` after N s    |
//! | anything that fails                      | `{"type":"error","message":"..."}`       |

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use plugin_graph::{GraphSpec, NodeId};

use super::Command;
use super::audio::{AudioGraphView, PluginEntry, TestFiles};
use crate::shortcut::HoldEdge;
use crate::shortcut::status::ShortcutStatus;

/// The protocol version this daemon speaks.
pub const VERSION: u64 = 1;

/// A rebind may wait for the old portal request and session to close.
const REBIND_REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// Scanning opens every plugin bundle in a worker of its own, and a graph
/// change loads the plugins it adds.
const AUDIO_REPLY_TIMEOUT: Duration = Duration::from_secs(90);

/// A test recording is answered once it has been recorded and processed.
const AUDIO_TEST_PROCESSING: Duration = Duration::from_secs(30);

/// How a stopped take is delivered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// Commit the polished text to the focused application.
    #[default]
    Input,
    /// Keep the polished text in history without typing it.
    Keep,
}

/// A request, without its version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Subscribe to hello, coalesced snapshots, and recording levels.
    Subscribe,
    /// Stop the daemon after the connection handler acknowledges the request.
    Shutdown,
    /// Remove completed history while retaining pending entries.
    ClearHistory,
    /// Start a take, or queue one behind output that is finishing.
    Start,
    /// Stop the current recording with the requested delivery mode.
    Stop { delivery: Delivery },
    /// The hold-to-talk key went down or came up.
    Hold { edge: HoldEdge },
    /// Apply the `[shortcut]` section of the config file again.
    ShortcutRebind,
    /// Read the live global-shortcut backend and bindings.
    ShortcutStatus,
    /// Apply the `[provider]` section of the config file again, from the
    /// next take on.
    ProviderReload,
    /// Apply the `[polish]` section of the config file again, from the next
    /// transcript on.
    PolishReload,
    /// List the VST3 plugins installed on this machine.
    AudioScan,
    /// Read the routing graph and the state of its nodes.
    AudioGraphGet,
    /// Save a routing graph, loading its plugins, for the next take on.
    /// Refused while a take is using the plugins, or when the graph cannot
    /// be used; nothing is saved then.
    AudioGraphSet { graph: GraphSpec },
    /// Open a plugin's own editor window on the daemon's display.
    AudioEditorOpen { node: NodeId },
    /// Record the microphone for this many seconds, and answer with it
    /// before and after the graph. Refused while dictating.
    AudioTest { seconds: u32 },
}

/// A successful, data-bearing, or error response to a protocol request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    /// The request completed without additional data.
    Ok,
    /// A start or stop intent was accepted for this take.
    Accepted { take: u64 },
    /// The current global-shortcut state.
    ShortcutStatus(ShortcutStatus),
    /// The plugins installed on this machine.
    AudioPlugins { plugins: Vec<PluginEntry> },
    /// The saved routing graph.
    AudioGraph(AudioGraphView),
    /// A test recording.
    AudioTest(TestFiles),
    /// The request failed; the message remains English for CLI compatibility.
    Error { message: String },
}

impl Reply {
    /// The reply as written on the wire, without its trailing `\n`.
    pub(super) fn to_line(&self) -> String {
        serde_json::to_string(self).expect("replies hold only strings")
    }
}

/// A request as it travels on the wire, with its version first.
#[derive(Serialize)]
pub(super) struct Envelope<'a> {
    v: u64,
    #[serde(flatten)]
    request: &'a Request,
}

impl<'a> Envelope<'a> {
    pub(super) fn new(request: &'a Request) -> Self {
        Self {
            v: VERSION,
            request,
        }
    }
}

/// The connection handler's response to a parsed request.
#[derive(Debug, PartialEq)]
pub(super) enum Action {
    Reply(Box<Reply>),
    Subscribe,
    Shutdown,
}

/// Answer a request; whatever fails becomes an error reply.
pub(super) async fn answer(text: &str, commands: &mpsc::Sender<Command>) -> Action {
    match parse(text) {
        Err(message) => Action::Reply(Box::new(Reply::Error { message })),
        Ok(Request::Subscribe) => Action::Subscribe,
        Ok(Request::Shutdown) => Action::Shutdown,
        Ok(request) => {
            let (reply, answer) = oneshot::channel();
            let (command, reply_timeout) = match request {
                Request::Subscribe | Request::Shutdown => {
                    unreachable!("long-lived requests are handled before command dispatch")
                }
                Request::ClearHistory => (Command::ClearHistory(reply), super::CONN_TIMEOUT),
                Request::Start => (Command::Start(reply), super::CONN_TIMEOUT),
                Request::Stop { delivery } => {
                    (Command::Stop { delivery, reply }, super::CONN_TIMEOUT)
                }
                Request::Hold { edge } => (Command::Hold(edge, reply), super::CONN_TIMEOUT),
                Request::ShortcutRebind => (Command::ShortcutRebind(reply), REBIND_REPLY_TIMEOUT),
                Request::ShortcutStatus => (Command::ShortcutStatus(reply), super::CONN_TIMEOUT),
                Request::ProviderReload => (Command::ProviderReload(reply), super::CONN_TIMEOUT),
                Request::PolishReload => (Command::PolishReload(reply), super::CONN_TIMEOUT),
                Request::AudioTest { seconds } => (
                    Command::Audio(request, reply),
                    Duration::from_secs(u64::from(seconds)) + AUDIO_TEST_PROCESSING,
                ),
                Request::AudioScan
                | Request::AudioGraphGet
                | Request::AudioGraphSet { .. }
                | Request::AudioEditorOpen { .. } => {
                    (Command::Audio(request, reply), AUDIO_REPLY_TIMEOUT)
                }
            };
            Action::Reply(Box::new(
                super::forward(commands, command, answer, reply_timeout)
                    .await
                    .unwrap_or_else(|error| Reply::Error {
                        message: format!("{error:#}"),
                    }),
            ))
        }
    }
}

/// Parse a request line: its version first, then the request itself.
fn parse(text: &str) -> Result<Request, String> {
    let mut value: Value =
        serde_json::from_str(text).map_err(|error| format!("invalid request: {error}"))?;
    let version = value.as_object_mut().and_then(|object| object.remove("v"));
    match version {
        None => Err("missing protocol version".to_string()),
        Some(version) if version == VERSION => {
            serde_json::from_value(value).map_err(|error| format!("invalid request: {error}"))
        }
        Some(version) => Err(format!(
            "unsupported protocol version {version}; supported: {VERSION}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::sync::mpsc;

    use super::{Action, Delivery, Envelope, Reply, Request, answer, parse};
    use crate::shortcut::HoldEdge;

    #[test]
    fn requests_carry_their_version_first() {
        assert_eq!(
            serde_json::to_string(&Envelope::new(&Request::Hold {
                edge: HoldEdge::Press
            }))
            .unwrap(),
            r#"{"v":1,"type":"hold","edge":"press"}"#
        );
    }

    #[test]
    fn replies_are_tagged_and_unversioned() {
        assert_eq!(
            [
                Reply::Ok,
                Reply::Error {
                    message: "say \"hi\"\n".to_string()
                }
            ]
            .map(|reply| serde_json::to_string(&reply).unwrap()),
            [
                r#"{"type":"ok"}"#,
                r#"{"type":"error","message":"say \"hi\"\n"}"#
            ]
        );
    }

    #[test]
    fn audio_requests_and_replies_keep_their_shape_on_the_wire() {
        use plugin_graph::GraphSpec;

        use crate::ipc::audio::{AudioGraphView, NodeStatus, PluginEntry, TestFiles};

        let requests = [
            Request::AudioScan,
            Request::AudioGraphGet,
            Request::AudioGraphSet {
                graph: GraphSpec::passthrough(),
            },
            Request::AudioEditorOpen { node: 2 },
            Request::AudioTest { seconds: 5 },
        ];
        let lines = requests
            .clone()
            .map(|request| serde_json::to_string(&Envelope::new(&request)).unwrap());
        let replies = [
            Reply::AudioPlugins {
                plugins: vec![PluginEntry {
                    path: "/Library/Audio/Plug-Ins/VST3/X.vst3".to_string(),
                    class_id: "00ff".to_string(),
                    name: "X".to_string(),
                    vendor: "V".to_string(),
                    category: "Fx|Dynamics".to_string(),
                    effect: true,
                }],
            },
            Reply::AudioGraph(AudioGraphView {
                graph: GraphSpec::passthrough(),
                nodes: vec![NodeStatus {
                    node: 0,
                    main_inputs: 0,
                    sidechain_inputs: 0,
                    outputs: 1,
                    problem: Some("did not load".to_string()),
                }],
            }),
            Reply::AudioTest(TestFiles {
                dry: "/tmp/dry.wav".to_string(),
                wet: "/tmp/wet.wav".to_string(),
            }),
        ];
        assert_eq!(
            (
                lines[0].clone(),
                lines.clone().map(|line| parse(&line)),
                replies
                    .clone()
                    .map(|reply| serde_json::from_str::<Reply>(&reply.to_line()).unwrap()),
            ),
            (
                r#"{"v":1,"type":"audio_scan"}"#.to_string(),
                requests.map(Ok),
                replies
            )
        );
    }

    #[test]
    fn hold_requests_parse() {
        assert_eq!(
            [
                r#"{"v":1,"type":"hold","edge":"press"}"#,
                r#"{"type":"hold","edge":"release","v":1}"#
            ]
            .map(parse),
            [
                Ok(Request::Hold {
                    edge: HoldEdge::Press
                }),
                Ok(Request::Hold {
                    edge: HoldEdge::Release
                })
            ]
        );
    }

    #[test]
    fn intents_carry_delivery_and_accepted_take_ids() {
        assert_eq!(
            [
                r#"{"v":1,"type":"start"}"#,
                r#"{"v":1,"type":"stop","delivery":"keep"}"#,
            ]
            .map(parse),
            [
                Ok(Request::Start),
                Ok(Request::Stop {
                    delivery: Delivery::Keep,
                }),
            ]
        );
        assert_eq!(
            Reply::Accepted { take: 7 }.to_line(),
            r#"{"type":"accepted","take":7}"#
        );
    }

    #[test]
    fn version_errors_name_the_problem() {
        assert_eq!(
            [
                r#"{"v":2,"type":"hold","edge":"press"}"#,
                r#"{"type":"hold","edge":"press"}"#
            ]
            .map(parse),
            [
                Err("unsupported protocol version 2; supported: 1".to_string()),
                Err("missing protocol version".to_string())
            ]
        );
    }

    #[test]
    fn invalid_requests_are_errors() {
        for text in [
            r#"{"v":1,"type":"dance"}"#,
            r#"{"v":1,"type":"toggle"}"#,
            r#"{"v":1,"type":"hold","edge":"down"}"#,
            r#"{"v":1,"type":"hold""#,
        ] {
            assert!(
                parse(text).is_err_and(|message| message.starts_with("invalid request: ")),
                "{text}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn ordinary_requests_fail_after_two_seconds() {
        let (commands, _received) = mpsc::channel(1);
        let answered =
            tokio::spawn(async move { answer(r#"{"v":1,"type":"start"}"#, &commands).await });
        tokio::time::advance(Duration::from_millis(1999)).await;
        assert!(!answered.is_finished());
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(
            answered.await.unwrap(),
            Action::Reply(Box::new(Reply::Error {
                message: "reply timeout: deadline has elapsed".to_string(),
            }))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn rebind_fails_after_five_seconds() {
        let (commands, _received) = mpsc::channel(1);
        let answered =
            tokio::spawn(
                async move { answer(r#"{"v":1,"type":"shortcut_rebind"}"#, &commands).await },
            );
        tokio::time::advance(Duration::from_millis(4999)).await;
        assert!(!answered.is_finished());
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(
            answered.await.unwrap(),
            Action::Reply(Box::new(Reply::Error {
                message: "reply timeout: deadline has elapsed".to_string(),
            }))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn rebind_waits_for_portal_cleanup() {
        let (commands, mut received) = mpsc::channel(1);
        tokio::spawn(async move {
            let Some(super::Command::ShortcutRebind(reply)) = received.recv().await else {
                panic!("expected a rebind");
            };
            // Closing an unanswered request and then the session can take over
            // the ordinary two-second reply budget.
            tokio::time::sleep(Duration::from_secs(3)).await;
            let _ = reply.send(Reply::Ok);
        });
        let answered =
            tokio::spawn(
                async move { answer(r#"{"v":1,"type":"shortcut_rebind"}"#, &commands).await },
            );
        tokio::time::advance(Duration::from_secs(3)).await;
        assert_eq!(answered.await.unwrap(), Action::Reply(Box::new(Reply::Ok)));
    }
}
