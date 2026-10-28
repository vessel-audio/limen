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

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use super::Command;
use crate::shortcut::HoldEdge;
use crate::shortcut::status::ShortcutStatus;

/// The protocol version this daemon speaks.
pub(crate) const VERSION: u64 = 1;

/// A request, without its version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Request {
    /// The hold-to-talk key went down or came up.
    Hold {
        edge: HoldEdge,
    },
    /// Apply the `[shortcut]` section of the config file again.
    ShortcutRebind,
    ShortcutStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Reply {
    Ok,
    ShortcutStatus(ShortcutStatus),
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
            let command = match request {
                Request::Hold { edge } => Command::Hold(edge, reply),
                Request::ShortcutRebind => Command::ShortcutRebind(reply),
                Request::ShortcutStatus => Command::ShortcutStatus(reply),
            };
            super::forward(commands, command, answer)
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
    use super::{Envelope, Reply, Request, parse};
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
}
