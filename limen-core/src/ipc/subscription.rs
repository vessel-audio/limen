//! Long-lived subscriptions driven directly by daemon watch channels.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{Semaphore, watch};

use super::wire::{Push, WireSnapshot};
use crate::audio::Level;
use crate::daemon::snapshot::{MAX_PUSH_BYTES, StateSnapshot};
use crate::dictation::DictationState;
use crate::shortcut::status::ShortcutStatus;

const MAX_SUBSCRIBERS: usize = 8;
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const LEVEL_INTERVAL: Duration = Duration::from_millis(100);

/// Shared watch sources and the process-wide subscriber limit.
#[derive(Clone)]
pub(crate) struct Watchers {
    snapshots: watch::Receiver<StateSnapshot>,
    shortcuts: watch::Receiver<ShortcutStatus>,
    levels: watch::Receiver<Level>,
    subscribers: Arc<Semaphore>,
}

impl Watchers {
    pub(crate) fn new(
        snapshots: watch::Receiver<StateSnapshot>,
        shortcuts: watch::Receiver<ShortcutStatus>,
        levels: watch::Receiver<Level>,
    ) -> Self {
        Self {
            snapshots,
            shortcuts,
            levels,
            subscribers: Arc::new(Semaphore::new(MAX_SUBSCRIBERS)),
        }
    }
}

#[cfg(test)]
impl Watchers {
    pub(crate) fn idle() -> Self {
        use crate::daemon::snapshot::{ConfigSummary, DictationStatus};
        use crate::dictation::DictationState;
        use crate::shortcut::status::{PerAction, SessionState};
        use crate::shortcut::{Backend, Shortcut};

        let state = StateSnapshot {
            take: 0,
            dictation: DictationStatus {
                state: DictationState::Idle,
                since_ms: None,
            },
            queued: false,
            history: Vec::new(),
            last_error: None,
            requests: Vec::new(),
            config: ConfigSummary {
                input_method: "fcitx5".to_string(),
                provider_model: "test".to_string(),
                provider_url: "http://127.0.0.1".to_string(),
                polish_enabled: false,
                polish_model: String::new(),
                polish_url: String::new(),
            },
        };
        let shortcut = ShortcutStatus {
            backend: Backend::None,
            session: SessionState::Disabled,
            preferred: PerAction {
                toggle: Shortcut::DEFAULT_TOGGLE,
                hold: Shortcut::DEFAULT_HOLD,
            },
            actual: PerAction::default(),
            last_error: None,
        };
        Self::new(
            watch::channel(state).1,
            watch::channel(shortcut).1,
            watch::channel(Level::SILENCE).1,
        )
    }
}

/// Own one long-lived connection after its subscribe request was parsed.
pub(super) async fn run(
    mut read: OwnedReadHalf,
    mut write: OwnedWriteHalf,
    watchers: Watchers,
) -> Result<()> {
    let _permit = match Arc::clone(&watchers.subscribers).try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            let reply = super::v1::Reply::Error {
                message: "too many subscribers".to_string(),
            };
            write
                .write_all(format!("{}\n", reply.to_line()).as_bytes())
                .await?;
            write.flush().await?;
            return Ok(());
        }
    };
    write_push(&mut write, &Push::hello()).await?;
    let mut snapshots = watchers.snapshots;
    let mut shortcuts = watchers.shortcuts;
    let levels = watchers.levels;
    let mut seq = 1;
    push_snapshot(&mut write, &mut snapshots, &mut shortcuts, &mut seq).await?;
    let mut level_tick = tokio::time::interval(LEVEL_INTERVAL);
    level_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_level = Level::SILENCE;
    let mut buffer = [0; 1024];
    loop {
        tokio::select! {
            changed = snapshots.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
                push_snapshot(&mut write, &mut snapshots, &mut shortcuts, &mut seq).await?;
            }
            changed = shortcuts.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
                push_snapshot(&mut write, &mut snapshots, &mut shortcuts, &mut seq).await?;
            }
            _ = level_tick.tick() => {
                let recording = matches!(snapshots.borrow().dictation.state, DictationState::Recording { .. });
                match recording {
                    false => last_level = Level::SILENCE,
                    true => {
                        let level = *levels.borrow();
                        if level != last_level {
                            write_push(&mut write, &Push::level(level)).await?;
                            last_level = level;
                        }
                    }
                }
            }
            result = read.read(&mut buffer) => {
                match result {
                    Ok(0) | Err(_) => return Ok(()),
                    Ok(_) => {}
                }
            }
        }
    }
}

async fn push_snapshot(
    write: &mut OwnedWriteHalf,
    snapshots: &mut watch::Receiver<StateSnapshot>,
    shortcuts: &mut watch::Receiver<ShortcutStatus>,
    seq: &mut u64,
) -> Result<()> {
    let state = snapshots.borrow_and_update().clone();
    let shortcut = shortcuts.borrow_and_update().clone();
    write_push(
        write,
        &Push::Snapshot(Box::new(WireSnapshot::new(*seq, state, shortcut))),
    )
    .await?;
    *seq += 1;
    Ok(())
}

async fn write_push(write: &mut OwnedWriteHalf, push: &Push) -> Result<()> {
    let mut line = serde_json::to_vec(push).context("serialize subscription push")?;
    if line.len() > MAX_PUSH_BYTES {
        bail!("subscription push exceeds {MAX_PUSH_BYTES} bytes");
    }
    line.push(b'\n');
    tokio::time::timeout(WRITE_TIMEOUT, async {
        write.write_all(&line).await?;
        write.flush().await
    })
    .await
    .context("subscription write timeout")??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;
    use tokio::sync::{mpsc, watch};

    use super::{Level, Watchers};
    use crate::daemon::snapshot::{
        ConfigSummary, DictationStatus, FIELD_CAP, HistoryEntry, HistoryOutcome, StateSnapshot,
    };
    use crate::dictation::{Control, DictationState};
    use crate::ipc::wire::Push;
    use crate::ipc::{Command, handle_conn};
    use crate::shortcut::status::{PerAction, SessionState, ShortcutStatus};
    use crate::shortcut::{Backend, Shortcut};

    fn state() -> StateSnapshot {
        StateSnapshot {
            take: 0,
            dictation: DictationStatus {
                state: DictationState::Idle,
                since_ms: None,
            },
            queued: false,
            history: Vec::new(),
            last_error: None,
            requests: Vec::new(),
            config: ConfigSummary {
                input_method: "fcitx5".to_string(),
                provider_model: "test".to_string(),
                provider_url: "http://127.0.0.1".to_string(),
                polish_enabled: false,
                polish_model: String::new(),
                polish_url: String::new(),
            },
        }
    }

    fn shortcut() -> ShortcutStatus {
        ShortcutStatus {
            backend: Backend::None,
            session: SessionState::Disabled,
            preferred: PerAction {
                toggle: Shortcut::DEFAULT_TOGGLE,
                hold: Shortcut::DEFAULT_HOLD,
            },
            actual: PerAction::default(),
            last_error: None,
        }
    }

    fn sources() -> (
        Watchers,
        watch::Sender<StateSnapshot>,
        watch::Sender<ShortcutStatus>,
        watch::Sender<Level>,
    ) {
        let (snapshots_tx, snapshots) = watch::channel(state());
        let (shortcuts_tx, shortcuts) = watch::channel(shortcut());
        let (levels_tx, levels) = watch::channel(Level::SILENCE);
        (
            Watchers::new(snapshots, shortcuts, levels),
            snapshots_tx,
            shortcuts_tx,
            levels_tx,
        )
    }

    async fn read_push(reader: &mut BufReader<UnixStream>) -> Push {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[tokio::test]
    async fn hello_snapshot_and_coalesced_changes_arrive_in_order() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let (watchers, snapshots, shortcuts, _levels) = sources();
        let (commands, _commands) = mpsc::channel(1);
        tokio::spawn(handle_conn(server, commands, watchers));
        client
            .write_all(b"{\"v\":1,\"type\":\"subscribe\"}\n")
            .await
            .unwrap();
        let mut reader = BufReader::new(client);
        assert!(matches!(
            read_push(&mut reader).await,
            Push::Hello { protocol: 1, .. }
        ));
        assert!(matches!(
            read_push(&mut reader).await,
            Push::Snapshot(snapshot) if snapshot.seq == 1
        ));

        snapshots.send_modify(|state| state.take = 2);
        shortcuts.send_modify(|status| {
            status.last_error = Some("x".repeat(5000));
            status.actual.toggle = Some(crate::shortcut::status::Trigger {
                description: "d".repeat(300),
                shortcuts: vec![Shortcut::DEFAULT_TOGGLE; 5],
            });
        });
        let snapshot = match read_push(&mut reader).await {
            Push::Snapshot(snapshot) => snapshot,
            other => panic!("expected snapshot, got {other:?}"),
        };
        assert_eq!(snapshot.seq, 2);
        assert_eq!(snapshot.state.take, 2);
        assert_eq!(snapshot.shortcut.last_error.unwrap().len(), 4096);
        let trigger = snapshot.shortcut.actual.toggle.unwrap();
        assert_eq!(trigger.description.len(), 256);
        assert_eq!(trigger.shortcuts.len(), 4);
    }

    #[tokio::test]
    async fn levels_are_rate_limited_and_only_pushed_while_recording() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let (watchers, snapshots, _shortcuts, levels) = sources();
        let (commands, _commands) = mpsc::channel(1);
        tokio::spawn(handle_conn(server, commands, watchers));
        client
            .write_all(b"{\"v\":1,\"type\":\"subscribe\"}\n")
            .await
            .unwrap();
        let mut reader = BufReader::new(client);
        let _hello = read_push(&mut reader).await;
        let _snapshot = read_push(&mut reader).await;
        snapshots.send_modify(|state| {
            state.dictation.state = DictationState::Recording {
                control: Control::Toggle,
            };
        });
        levels.send_replace(Level {
            rms: 0.5,
            dbfs: -6.0,
        });
        loop {
            if matches!(read_push(&mut reader).await, Push::Level { .. }) {
                break;
            }
        }
        snapshots.send_modify(|state| {
            state.dictation.state = DictationState::Idle;
        });
        levels.send_replace(Level {
            rms: 0.75,
            dbfs: -2.5,
        });
        loop {
            if matches!(read_push(&mut reader).await, Push::Snapshot(_)) {
                break;
            }
        }
        let mut line = String::new();
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(150),
            reader.read_line(&mut line),
        )
        .await;
        assert!(result.is_err(), "level pushed outside recording: {line}");
    }

    #[tokio::test]
    async fn the_ninth_subscriber_is_rejected() {
        let (watchers, _snapshots, _shortcuts, _levels) = sources();
        let (commands, _commands) = mpsc::channel(8);
        let mut tasks = Vec::new();
        let mut readers = Vec::new();
        for _ in 0..8 {
            let (mut client, server) = UnixStream::pair().unwrap();
            tasks.push(tokio::spawn(handle_conn(
                server,
                commands.clone(),
                watchers.clone(),
            )));
            client
                .write_all(b"{\"v\":1,\"type\":\"subscribe\"}\n")
                .await
                .unwrap();
            let mut reader = BufReader::new(client);
            let _hello = read_push(&mut reader).await;
            let _snapshot = read_push(&mut reader).await;
            readers.push(reader);
        }
        let (mut client, server) = UnixStream::pair().unwrap();
        tasks.push(tokio::spawn(handle_conn(server, commands, watchers)));
        client
            .write_all(b"{\"v\":1,\"type\":\"subscribe\"}\n")
            .await
            .unwrap();
        let mut reply = String::new();
        BufReader::new(client).read_line(&mut reply).await.unwrap();
        assert_eq!(
            reply,
            "{\"type\":\"error\",\"message\":\"too many subscribers\"}\n"
        );
        drop(readers);
        for task in tasks {
            task.abort();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_reader_is_disconnected_without_blocking_other_clients() {
        let (watchers, snapshots, _shortcuts, _levels) = sources();
        snapshots.send_modify(|state| {
            state.history = (0..50)
                .map(|id| HistoryEntry {
                    id,
                    take: 1,
                    at_ms: id,
                    original: "x".repeat(FIELD_CAP),
                    text: None,
                    outcome: HistoryOutcome::Committed,
                    truncated: false,
                })
                .collect();
        });
        let (commands, mut received) = mpsc::channel(8);
        tokio::spawn(async move {
            while let Some(command) = received.recv().await {
                if let Command::Toggle(reply) = command {
                    let _ = reply.send("recording".to_string());
                }
            }
        });
        let (mut slow, server) = UnixStream::pair().unwrap();
        let task = tokio::spawn(handle_conn(server, commands.clone(), watchers.clone()));
        slow.write_all(b"{\"v\":1,\"type\":\"subscribe\"}\n")
            .await
            .unwrap();
        let (mut toggle, server) = UnixStream::pair().unwrap();
        tokio::spawn(handle_conn(server, commands, watchers));
        toggle.write_all(b"toggle\n").await.unwrap();
        let mut reply = String::new();
        BufReader::new(toggle).read_line(&mut reply).await.unwrap();
        assert_eq!(reply, "recording\n");

        tokio::time::advance(std::time::Duration::from_secs(3)).await;
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn closing_the_client_ends_the_subscription() {
        let (watchers, _snapshots, _shortcuts, _levels) = sources();
        let (commands, _commands) = mpsc::channel(1);
        let (client, server) = UnixStream::pair().unwrap();
        let task = tokio::spawn(handle_conn(server, commands, watchers));
        drop(client);
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
    }
}
