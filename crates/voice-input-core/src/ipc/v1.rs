//! Protocol v1: one JSON object per line, tagged by `type`. Requests carry
//! the protocol version as `"v":1`; replies are unversioned.
//!
//! | Request                                  | Reply                                    |
//! |------------------------------------------|------------------------------------------|
//! | `{"v":1,"type":"hold","edge":"press"}`   | `{"type":"ok"}`, before the take starts  |
//! | `{"v":1,"type":"hold","edge":"release"}` | `{"type":"ok"}`                          |
//! | `{"v":1,"type":"shortcut_rebind"}`       | `{"type":"ok"}` once the new state shows |
//! | `{"v":1,"type":"shortcut_status"}`       | `{"type":"shortcut_status",...}`         |
//! | anything that fails                      | `{"type":"error","message":"..."}`       |

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use super::Command;
use crate::shortcut::HoldEdge;
use crate::shortcut::status::ShortcutStatus;

/// The protocol version this daemon speaks.
pub const VERSION: u64 = 1;

/// A rebind may wait for the old portal request and session to close.
const REBIND_REPLY_TIMEOUT: Duration = Duration::from_secs(5);

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
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
}

/// A successful, data-bearing, or error response to a protocol request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    /// The request completed without additional data.
    Ok,
    /// A start or stop intent was accepted for this take.
    Accepted { take: u64 },
    /// The current global-shortcut state.
    ShortcutStatus(ShortcutStatus),
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

/// Answer a request; whatever fails becomes an error reply.
pub(super) async fn answer(text: &str, commands: &mpsc::Sender<Command>) -> Reply {
    match parse(text) {
        Err(message) => Reply::Error { message },
        Ok(request) => {
            let (reply, answer) = oneshot::channel();
            let (command, reply_timeout) = match request {
                Request::Start => (Command::Start(reply), super::CONN_TIMEOUT),
                Request::Stop { delivery } => {
                    (Command::Stop { delivery, reply }, super::CONN_TIMEOUT)
                }
                Request::Hold { edge } => (Command::Hold(edge, reply), super::CONN_TIMEOUT),
                Request::ShortcutRebind => (Command::ShortcutRebind(reply), REBIND_REPLY_TIMEOUT),
                Request::ShortcutStatus => (Command::ShortcutStatus(reply), super::CONN_TIMEOUT),
            };
            super::forward(commands, command, answer, reply_timeout)
                .await
                .unwrap_or_else(|error| Reply::Error {
                    message: format!("{error:#}"),
                })
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

    use super::{Delivery, Envelope, Reply, Request, answer, parse};
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
        assert_eq!(answered.await.unwrap(), Reply::Ok);
    }
}
