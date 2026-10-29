//! The resident daemon: translate I/O into events, apply pure dictation
//! transitions, and interpret the resulting effects.
//!
//! Actor model: the dictation loop in [`run`] is an actor that alone owns the
//! `Dictation` state. It handles one message at a time from its mailboxes —
//! IPC commands, session actor reports, text input outcomes, and the finalize
//! and hold deadlines — and never awaits slow work itself. Effects become messages to
//! the actors that own the slow resources:
//! - the session actor (`session`) owns microphone capture and the ASR session;
//! - the text input worker owns the input method and commits final text.
//!
//! Their outcomes come back as events, so the loop stays responsive to IPC
//! even while pw-record is being reaped or a take is being finalized.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use crate::config::{Config, Paths};
use crate::dictation::{self, Dictation, Effect, Event, InputOutcome, Notification};
use crate::input_method::InputMethod;
use crate::ipc;
use crate::notify::notify;
use crate::polish::Polisher;
use crate::session::{SessionCommand, SessionHandle};
use crate::shortcut::portal::{Bus, ShortcutHandle};
use crate::shortcut::{Activation, ActivationReceipt, HoldEdge};

/// Give up on a finalize after this long without a terminal event.
const FINALIZE_TIMEOUT: Duration = Duration::from_secs(10);

type Deadline = Pin<Box<tokio::time::Sleep>>;

/// State of the dictation actor: business state plus the addresses of the
/// actors that perform its effects.
struct DictationActor {
    dictation: Dictation,
    session: SessionHandle,
    /// Mailbox of the text input worker.
    input_tx: mpsc::Sender<String>,
    finalize_deadline: Option<Deadline>,
    /// Stops a hold recording whose release never arrived.
    hold_deadline: Option<Deadline>,
    /// When the running hold recording started; how long it lasted decides
    /// whether its release keeps the take.
    hold_started: Option<Instant>,
    /// When the hold key went down; `Some` until it is released.
    hold_pressed: Option<Instant>,
    /// IPC caller of a shortcut start, answered once the session actor reports
    /// whether the take started.
    start_caller: Option<oneshot::Sender<String>>,
    /// Shows a desktop notification: `(summary, body)`.
    notify: fn(&str, &str),
    /// Read again when a client asks to apply the `[shortcut]` section.
    config_file: PathBuf,
    shortcuts: ShortcutHandle,
}

/// Whether the message being handled came from an IPC caller awaiting a reply.
enum ReplyTarget {
    NotRequested,
    Requested(oneshot::Sender<String>),
}

/// The dictation actor's mailboxes; its loop ends when one of them closes.
struct Mailboxes {
    commands: mpsc::Receiver<ipc::Command>,
    input_reports: mpsc::UnboundedReceiver<InputOutcome>,
    session_reports: mpsc::UnboundedReceiver<Event>,
    activations: mpsc::UnboundedReceiver<ActivationReceipt>,
}

impl DictationActor {
    fn new(
        session: SessionHandle,
        input_tx: mpsc::Sender<String>,
        notify: fn(&str, &str),
        config_file: PathBuf,
        shortcuts: ShortcutHandle,
    ) -> Self {
        Self {
            dictation: Dictation::default(),
            session,
            input_tx,
            finalize_deadline: None,
            hold_deadline: None,
            hold_started: None,
            hold_pressed: None,
            start_caller: None,
            notify,
            config_file,
            shortcuts,
        }
    }

    /// Handle one message: apply its event and drain any follow-up events
    /// produced by its effects. Never awaits; slow effects are messages to
    /// other actors.
    fn dispatch(&mut self, event: Event, mut reply: ReplyTarget) {
        let mut events = VecDeque::from([event]);
        while let Some(event) = events.pop_front() {
            let answers_start =
                matches!(event, Event::SessionStarted | Event::SessionStartFailed(_));
            let transition = dictation::transition(self.dictation, event);
            self.dictation = transition.state;
            // Only the message itself can come from an IPC caller.
            let caller = std::mem::replace(&mut reply, ReplyTarget::NotRequested);
            match (transition.reply, caller) {
                (Some(message), ReplyTarget::Requested(caller)) => {
                    let _ = caller.send(message);
                }
                // A take started by an IPC toggle answers the caller parked for it.
                (Some(message), ReplyTarget::NotRequested) if answers_start => {
                    match self.start_caller.take() {
                        Some(caller) => {
                            let _ = caller.send(message);
                        }
                        None => tracing::debug!(message, "the take was not started over IPC"),
                    }
                }
                // Nobody asked, e.g. a toggle from a global shortcut.
                (Some(message), ReplyTarget::NotRequested) => {
                    tracing::debug!(message, "reply without an IPC caller");
                }
                // Only a shortcut start defers its answer: the core replies once
                // the session actor reports whether the take started.
                (None, ReplyTarget::Requested(caller)) => {
                    self.start_caller = Some(caller);
                }
                (None, ReplyTarget::NotRequested) => {}
            }

            for effect in transition.effects {
                match effect {
                    Effect::StartSession => self.session.tell(SessionCommand::Start),
                    Effect::StopSession => self.session.tell(SessionCommand::Stop),
                    Effect::TeardownSession => self.session.tell(SessionCommand::Teardown),
                    Effect::QueueInput(text) => match self.input_tx.try_send(text) {
                        Ok(()) => events.push_back(Event::InputQueued),
                        Err(error) => {
                            tracing::warn!(error = %error, "text input queue full or dead");
                            events.push_back(Event::InputDropped);
                        }
                    },
                    Effect::ArmFinalizeDeadline => {
                        self.finalize_deadline =
                            Some(Box::pin(tokio::time::sleep(FINALIZE_TIMEOUT)));
                    }
                    Effect::DisarmFinalizeDeadline => {
                        self.finalize_deadline = None;
                    }
                    Effect::ArmHoldDeadline => {
                        self.hold_started = Some(Instant::now());
                        self.hold_deadline =
                            Some(Box::pin(tokio::time::sleep(dictation::MAX_HOLD)));
                    }
                    Effect::DisarmHoldDeadline => {
                        self.hold_started = None;
                        self.hold_deadline = None;
                    }
                    Effect::Notify(Notification::Info(message)) => {
                        (self.notify)("voice-input", &message);
                    }
                    Effect::Notify(Notification::Error(message)) => {
                        (self.notify)("voice-input error", &message);
                    }
                }
            }
        }
    }

    /// Dispatch an edge of the hold-to-talk key. Nobody waits for the reply.
    fn hold(&mut self, edge: HoldEdge) {
        self.hold_at(edge, Instant::now());
    }

    /// Apply an edge as received at `now`, not when a later mailbox turn
    /// happens to handle it.
    fn hold_at(&mut self, edge: HoldEdge, now: Instant) {
        match (edge, self.hold_pressed) {
            // Key repeat while the key is down. Held across the end of a take,
            // it must not start the next one: that takes a new press.
            (HoldEdge::Press, Some(_)) => {}
            (HoldEdge::Press, None) => {
                self.hold_pressed = Some(now);
                self.dispatch(Event::HoldPressed, ReplyTarget::NotRequested);
            }
            (HoldEdge::Release, pressed) => {
                self.hold_pressed = None;
                let effective = effective_hold(self.hold_started, now);
                let held = pressed.map(|pressed| now.saturating_duration_since(pressed));
                tracing::info!(?effective, ?held, "hold key released");
                self.dispatch(Event::HoldReleased { effective }, ReplyTarget::NotRequested);
            }
        }
    }

    /// Stop after a lost release and rebuild the portal session. The old
    /// generation's repeats are dropped, so clearing the pressed marker cannot
    /// restart recording while the key is still physically held.
    fn hold_timed_out(&mut self) {
        match Config::load(&self.config_file) {
            Ok(config) => {
                self.hold_pressed = None;
                drop(self.shortcuts.rebind(config.shortcut));
            }
            Err(error) => {
                tracing::warn!(error = %format!("{error:#}"), "cannot rebuild shortcuts after hold timeout")
            }
        }
        tracing::warn!("hold exceeded its maximum duration");
        self.dispatch(Event::HoldTimedOut, ReplyTarget::NotRequested);
    }

    /// Apply the `[shortcut]` section of the config again. The whole file must
    /// be valid, though only that section applies without a restart. The
    /// caller hears back once the shortcut actor shows the new state.
    fn rebind(&self, reply: oneshot::Sender<ipc::v1::Reply>) {
        match Config::load(&self.config_file) {
            Ok(config) => {
                let rebound = self.shortcuts.rebind(config.shortcut);
                tokio::spawn(async move {
                    let _ = reply.send(match rebound.await {
                        Ok(()) => ipc::v1::Reply::Ok,
                        Err(_) => ipc::v1::Reply::Error {
                            message: "the shortcut backend stopped".to_string(),
                        },
                    });
                });
            }
            Err(error) => {
                let _ = reply.send(ipc::v1::Reply::Error {
                    message: format!("{error:#}"),
                });
            }
        }
    }

    /// Message loop: handle one message at a time until a mailbox closes.
    async fn run(mut self, mut mailboxes: Mailboxes) -> Result<()> {
        loop {
            tokio::select! {
                cmd = mailboxes.commands.recv() => {
                    let Some(cmd) = cmd else { anyhow::bail!("ipc accept loop died") };
                    match cmd {
                        ipc::Command::Status(reply) => {
                            let _ = reply.send(self.dictation.label().to_string());
                        }
                        ipc::Command::Toggle(reply) => {
                            self.dispatch(Event::Toggle, ReplyTarget::Requested(reply));
                        }
                        ipc::Command::Hold(edge, reply) => {
                            // Answered at once: an edge never waits for the take to start.
                            let _ = reply.send(ipc::v1::Reply::Ok);
                            self.hold(edge);
                        }
                        ipc::Command::ShortcutRebind(reply) => self.rebind(reply),
                        ipc::Command::ShortcutStatus(reply) => {
                            let _ = reply.send(ipc::v1::Reply::ShortcutStatus(self.shortcuts.status()));
                        }
                    }
                }

                outcome = mailboxes.input_reports.recv() => {
                    match outcome {
                        Some(outcome) => {
                            self.dispatch(Event::InputFinished(outcome), ReplyTarget::NotRequested);
                        }
                        None => anyhow::bail!("text input worker stopped unexpectedly"),
                    }
                }

                event = mailboxes.session_reports.recv() => {
                    match event {
                        Some(event) => self.dispatch(event, ReplyTarget::NotRequested),
                        None => anyhow::bail!("session actor stopped unexpectedly"),
                    }
                }

                activation = mailboxes.activations.recv() => {
                    match activation {
                        Some(ActivationReceipt { activation: Activation::Toggle, .. }) => self.dispatch(Event::Toggle, ReplyTarget::NotRequested),
                        Some(ActivationReceipt { activation: Activation::Hold(edge), received_at }) => {
                            self.hold_at(edge, received_at);
                        }
                        None => anyhow::bail!("shortcut actor stopped unexpectedly"),
                    }
                }

                _ = async { self.finalize_deadline.as_mut().unwrap().await },
                        if self.finalize_deadline.is_some() => {
                    tracing::warn!("finalize timed out");
                    self.dispatch(Event::FinalizeTimedOut, ReplyTarget::NotRequested);
                }

                _ = async { self.hold_deadline.as_mut().unwrap().await },
                        if self.hold_deadline.is_some() => {
                    self.hold_timed_out();
                }
            }
        }
    }
}

/// How long the hold recording has lasted when the key is released at
/// `released`; zero when no hold recording is running.
fn effective_hold(started: Option<Instant>, released: Instant) -> Duration {
    started.map_or(Duration::ZERO, |started| {
        released.saturating_duration_since(started)
    })
}

pub async fn run(paths: Paths, cfg: Config) -> Result<()> {
    let listener = ipc::bind(&paths)?;
    let (cmd_tx, commands) = mpsc::channel::<ipc::Command>(8);
    ipc::spawn_accept_loop(listener, cmd_tx);

    // Text input worker: an actor that owns the input method and commits one
    // queued text at a time, so slow polishing or text input never stalls the
    // dictation loop.
    let (input_tx, mut input_rx) = mpsc::channel::<String>(16);
    let (input_report_tx, input_reports) = mpsc::unbounded_channel::<InputOutcome>();
    let backend = crate::input_method::default_backend(cfg.input_method.backend);
    let polisher = Polisher::from_config(&cfg.polish)?;
    tracing::info!(?backend, "input method backend");
    tokio::spawn(async move {
        let mut input_method = InputMethod::new(backend);
        while let Some(text) = input_rx.recv().await {
            let original = text;
            // Cloud work is optional; queued originals take priority.
            let corrected = polisher
                .polish_when_idle(original.clone(), input_rx.is_empty())
                .await;
            let text = input_method
                .compatible_text(&original, &corrected)
                .to_string();
            let outcome = match input_method.commit_text(&text).await {
                Ok(()) => InputOutcome::Succeeded,
                Err(error) => {
                    tracing::warn!(error = %format!("{error:#}"), "text input failed");
                    InputOutcome::Failed(format!("{error:#}"))
                }
            };
            if input_report_tx.send(outcome).is_err() {
                break;
            }
        }
    });

    // The session actor holds the only sender, so this mailbox closes exactly
    // when the actor stops.
    let (activation_tx, activations) = mpsc::unbounded_channel();
    let shortcuts = ShortcutHandle::spawn(cfg.shortcut, Bus::Session, activation_tx, notify);
    let (session_report_tx, session_reports) = mpsc::unbounded_channel::<Event>();
    let actor = DictationActor::new(
        SessionHandle::spawn(cfg, session_report_tx),
        input_tx,
        notify,
        paths.config_file.clone(),
        shortcuts,
    );

    notify("voice-input", "daemon started");
    tracing::info!(socket = %paths.socket.display(), "daemon listening");

    actor
        .run(Mailboxes {
            commands,
            input_reports,
            session_reports,
            activations,
        })
        .await
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use tokio::sync::{mpsc, oneshot};
    use tokio::time::Instant;

    use super::{DictationActor, Mailboxes, ReplyTarget, effective_hold};
    use crate::config::ShortcutConfig;
    use crate::dictation::{Event, InputOutcome, MAX_HOLD};
    use crate::ipc::{self, v1::Reply};
    use crate::session::{SessionCommand, SessionHandle};
    use crate::shortcut::portal::{Bus, ShortcutHandle};
    use crate::shortcut::status::{PerAction, ShortcutStatus};
    use crate::shortcut::{Activation, ActivationReceipt, Backend, HoldEdge};

    const TAP: Duration = Duration::from_millis(100);
    const KEPT: Duration = Duration::from_secs(1);

    /// A dictation actor wired to channels the test holds.
    struct Harness {
        actor: DictationActor,
        /// Everything the actor told the session actor.
        session: mpsc::UnboundedReceiver<SessionCommand>,
        _input: mpsc::Receiver<String>,
    }

    fn harness() -> Harness {
        let (mailbox, session) = mpsc::unbounded_channel();
        let (input_tx, input) = mpsc::channel(16);
        Harness {
            actor: DictationActor::new(
                SessionHandle::from_mailbox(mailbox),
                input_tx,
                |_, _| {},
                PathBuf::from("/nonexistent/config.toml"),
                ShortcutHandle::detached(),
            ),
            session,
            _input: input,
        }
    }

    /// The senders that keep a running actor's mailboxes open.
    struct Senders {
        commands: mpsc::Sender<ipc::Command>,
        _input_reports: mpsc::UnboundedSender<InputOutcome>,
        session_reports: mpsc::UnboundedSender<Event>,
        activations: mpsc::UnboundedSender<ActivationReceipt>,
    }

    fn mailboxes() -> (Mailboxes, Senders) {
        let (commands_tx, commands) = mpsc::channel(8);
        let (input_reports_tx, input_reports) = mpsc::unbounded_channel();
        let (session_reports_tx, session_reports) = mpsc::unbounded_channel();
        let (activations_tx, activations) = mpsc::unbounded_channel();
        (
            Mailboxes {
                commands,
                input_reports,
                session_reports,
                activations,
            },
            Senders {
                commands: commands_tx,
                _input_reports: input_reports_tx,
                session_reports: session_reports_tx,
                activations: activations_tx,
            },
        )
    }

    /// Run the actor's message loop in the background.
    fn spawn_loop(actor: DictationActor) -> Senders {
        let (mailboxes, senders) = mailboxes();
        tokio::spawn(actor.run(mailboxes));
        senders
    }

    /// Send a hold key edge over IPC to a running loop and return its answer.
    async fn ipc_hold(senders: &Senders, edge: HoldEdge) -> Reply {
        let (reply, answer) = oneshot::channel();
        senders
            .commands
            .send(ipc::Command::Hold(edge, reply))
            .await
            .unwrap();
        answer.await.unwrap()
    }

    fn sent(session: &mut mpsc::UnboundedReceiver<SessionCommand>) -> Vec<SessionCommand> {
        std::iter::from_fn(|| session.try_recv().ok()).collect()
    }

    /// Ask a running loop for its status: once answered, the loop has handled
    /// everything that was due before.
    async fn status(senders: &Senders) -> String {
        let (reply, answer) = oneshot::channel();
        senders
            .commands
            .send(ipc::Command::Status(reply))
            .await
            .unwrap();
        answer.await.unwrap()
    }

    /// Press the hold key and let its take start.
    fn start_hold(actor: &mut DictationActor) {
        actor.hold(HoldEdge::Press);
        actor.dispatch(Event::SessionStarted, ReplyTarget::NotRequested);
    }

    #[test]
    fn effective_hold_counts_from_the_start_of_the_recording() {
        let started = Instant::now();
        let released = started + Duration::from_millis(450);
        assert_eq!(
            [
                effective_hold(Some(started), released),
                effective_hold(None, released)
            ],
            [Duration::from_millis(450), Duration::ZERO]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn accidental_tap_tears_the_take_down_without_stopping_it() {
        let mut harness = harness();
        start_hold(&mut harness.actor);
        tokio::time::advance(TAP).await;
        harness.actor.hold(HoldEdge::Release);
        assert_eq!(
            sent(&mut harness.session),
            [SessionCommand::Start, SessionCommand::Teardown]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn kept_hold_stops_the_take() {
        let mut harness = harness();
        start_hold(&mut harness.actor);
        tokio::time::advance(KEPT).await;
        harness.actor.hold(HoldEdge::Release);
        assert_eq!(
            sent(&mut harness.session),
            [SessionCommand::Start, SessionCommand::Stop]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn press_right_after_a_tap_starts_after_the_teardown() {
        let mut harness = harness();
        start_hold(&mut harness.actor);
        tokio::time::advance(TAP).await;
        harness.actor.hold(HoldEdge::Release);
        harness.actor.hold(HoldEdge::Press);
        assert_eq!(
            sent(&mut harness.session),
            [
                SessionCommand::Start,
                SessionCommand::Teardown,
                SessionCommand::Start
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn release_outside_a_hold_recording_sends_nothing() {
        let mut harness = harness();
        harness.actor.hold(HoldEdge::Release);
        assert_eq!(sent(&mut harness.session), []);

        let (reply, _answer) = oneshot::channel();
        harness
            .actor
            .dispatch(Event::Toggle, ReplyTarget::Requested(reply));
        harness
            .actor
            .dispatch(Event::SessionStarted, ReplyTarget::NotRequested);
        tokio::time::advance(KEPT).await;
        harness.actor.hold(HoldEdge::Release);
        assert_eq!(sent(&mut harness.session), [SessionCommand::Start]);
    }

    #[tokio::test(start_paused = true)]
    async fn hold_deadline_stops_a_take_whose_release_is_lost() {
        let mut harness = harness();
        start_hold(&mut harness.actor);
        let _senders = spawn_loop(harness.actor);
        let started = Instant::now();
        assert_eq!(
            [harness.session.recv().await, harness.session.recv().await],
            [Some(SessionCommand::Start), Some(SessionCommand::Stop)]
        );
        assert_eq!(started.elapsed(), MAX_HOLD);
    }

    #[tokio::test(start_paused = true)]
    async fn disarmed_hold_deadline_never_fires() {
        let mut harness = harness();
        start_hold(&mut harness.actor);
        tokio::time::advance(KEPT).await;
        harness.actor.hold(HoldEdge::Release);
        let senders = spawn_loop(harness.actor);
        tokio::time::advance(MAX_HOLD * 2).await;
        assert_eq!(status(&senders).await, "finalizing");
        assert_eq!(
            sent(&mut harness.session),
            [SessionCommand::Start, SessionCommand::Stop]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn release_and_timeout_due_together_stop_the_take_once() {
        // The release is handled first: it disarms the deadline that is due.
        let mut released_first = harness();
        start_hold(&mut released_first.actor);
        tokio::time::advance(MAX_HOLD).await;
        released_first.actor.hold(HoldEdge::Release);
        let senders = spawn_loop(released_first.actor);
        assert_eq!(status(&senders).await, "finalizing");
        assert_eq!(
            sent(&mut released_first.session),
            [SessionCommand::Start, SessionCommand::Stop]
        );

        // The timeout is handled first: the release that follows is ignored.
        let mut timed_out_first = harness();
        start_hold(&mut timed_out_first.actor);
        tokio::time::advance(MAX_HOLD).await;
        timed_out_first
            .actor
            .dispatch(Event::HoldTimedOut, ReplyTarget::NotRequested);
        timed_out_first.actor.hold(HoldEdge::Release);
        assert_eq!(
            sent(&mut timed_out_first.session),
            [SessionCommand::Start, SessionCommand::Stop]
        );
    }

    #[tokio::test]
    async fn hold_requests_leave_a_waiting_toggle_caller_its_own_answer() {
        let reports = [
            (Event::SessionStarted, "recording"),
            (
                Event::SessionStartFailed("offline".to_string()),
                "error: offline",
            ),
        ];
        for (report, toggle_answer) in reports {
            let mut harness = harness();
            let senders = spawn_loop(harness.actor);
            let (reply, answer) = oneshot::channel();
            senders
                .commands
                .send(ipc::Command::Toggle(reply))
                .await
                .unwrap();
            assert_eq!(harness.session.recv().await, Some(SessionCommand::Start));
            assert_eq!(
                [
                    ipc_hold(&senders, HoldEdge::Press).await,
                    ipc_hold(&senders, HoldEdge::Release).await
                ],
                [Reply::Ok, Reply::Ok]
            );
            senders.session_reports.send(report).unwrap();
            assert_eq!(answer.await.unwrap(), toggle_answer);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn release_and_timeout_ready_in_one_iteration_stop_the_take_once() {
        // select! polls ready branches in random order: repeat to see both.
        for _ in 0..16 {
            let mut harness = harness();
            start_hold(&mut harness.actor);
            let (mailboxes, senders) = mailboxes();
            let (reply, answer) = oneshot::channel();
            senders
                .commands
                .try_send(ipc::Command::Hold(HoldEdge::Release, reply))
                .unwrap();
            tokio::time::advance(MAX_HOLD).await;
            tokio::spawn(harness.actor.run(mailboxes));
            assert_eq!(answer.await.unwrap(), Reply::Ok);
            assert_eq!(status(&senders).await, "finalizing");
            assert_eq!(
                sent(&mut harness.session),
                [SessionCommand::Start, SessionCommand::Stop]
            );
        }
    }

    #[tokio::test]
    async fn key_repeat_held_across_the_end_of_a_take_starts_nothing() {
        let mut harness = harness();
        let (start_reply, _start_answer) = oneshot::channel();
        harness
            .actor
            .dispatch(Event::Toggle, ReplyTarget::Requested(start_reply));
        harness
            .actor
            .dispatch(Event::SessionStarted, ReplyTarget::NotRequested);
        let (stop_reply, _stop_answer) = oneshot::channel();
        harness
            .actor
            .dispatch(Event::Toggle, ReplyTarget::Requested(stop_reply));
        // Pressed while the take finalizes, still held once it has finished.
        harness.actor.hold(HoldEdge::Press);
        harness
            .actor
            .dispatch(Event::ProviderCompleted, ReplyTarget::NotRequested);
        harness.actor.hold(HoldEdge::Press);
        harness.actor.hold(HoldEdge::Release);
        assert_eq!(
            sent(&mut harness.session),
            [
                SessionCommand::Start,
                SessionCommand::Stop,
                SessionCommand::Teardown
            ]
        );

        harness.actor.hold(HoldEdge::Press);
        assert_eq!(sent(&mut harness.session), [SessionCommand::Start]);
    }

    #[tokio::test]
    async fn a_shortcut_toggle_leaves_a_waiting_ipc_caller_its_own_answer() {
        let mut harness = harness();
        let (reply, answer) = oneshot::channel();
        harness
            .actor
            .dispatch(Event::Toggle, ReplyTarget::Requested(reply));
        // A shortcut toggle while the take starts is answered "starting", for nobody.
        harness
            .actor
            .dispatch(Event::Toggle, ReplyTarget::NotRequested);
        harness
            .actor
            .dispatch(Event::SessionStarted, ReplyTarget::NotRequested);
        assert_eq!(answer.await.unwrap(), "recording");
        assert_eq!(sent(&mut harness.session), [SessionCommand::Start]);
    }

    #[tokio::test(start_paused = true)]
    async fn shortcut_hold_edges_drive_a_hold_take() {
        let mut harness = harness();
        let senders = spawn_loop(harness.actor);
        senders
            .activations
            .send(ActivationReceipt {
                activation: Activation::Hold(HoldEdge::Press),
                received_at: Instant::now(),
            })
            .unwrap();
        assert_eq!(harness.session.recv().await, Some(SessionCommand::Start));
        senders.session_reports.send(Event::SessionStarted).unwrap();
        // The report and the status query travel through different mailboxes.
        while status(&senders).await != "recording" {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(KEPT).await;
        senders
            .activations
            .send(ActivationReceipt {
                activation: Activation::Hold(HoldEdge::Release),
                received_at: Instant::now(),
            })
            .unwrap();
        assert_eq!(harness.session.recv().await, Some(SessionCommand::Stop));
    }

    #[tokio::test]
    async fn shortcut_rebind_applies_only_a_valid_config() {
        let dir = std::env::temp_dir().join(format!("voice-input-rebind-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config_file = dir.join("config.toml");
        let base =
            "[provider]\nurl = \"http://127.0.0.1:8000/v1/audio/transcriptions\"\nmodel = \"m\"\n";
        let (activations, _activations) = mpsc::unbounded_channel();
        let shortcuts = ShortcutHandle::spawn(
            ShortcutConfig::default(),
            Bus::Session,
            activations,
            |_, _| {},
        );
        let before = shortcuts.status();
        let (mailbox, _session) = mpsc::unbounded_channel();
        let (input_tx, _input) = mpsc::channel(16);
        let actor = DictationActor::new(
            SessionHandle::from_mailbox(mailbox),
            input_tx,
            |_, _| {},
            config_file.clone(),
            shortcuts,
        );
        let senders = spawn_loop(actor);
        let rebind = || async {
            let (reply, answer) = oneshot::channel();
            senders
                .commands
                .send(ipc::Command::ShortcutRebind(reply))
                .await
                .unwrap();
            answer.await.unwrap()
        };
        let status = || async {
            let (reply, answer) = oneshot::channel();
            senders
                .commands
                .send(ipc::Command::ShortcutStatus(reply))
                .await
                .unwrap();
            answer.await.unwrap()
        };

        std::fs::write(&config_file, format!("{base}[audio]\nchunk_ms = 0\n")).unwrap();
        assert!(matches!(
            rebind().await,
            Reply::Error { message } if message.contains("audio.chunk_ms")
        ));
        assert_eq!(status().await, Reply::ShortcutStatus(before.clone()));

        std::fs::write(
            &config_file,
            format!("{base}[shortcut]\nbackend = \"gnome-keybinding\"\ntoggle = \"Ctrl+Alt+V\"\n"),
        )
        .unwrap();
        assert_eq!(rebind().await, Reply::Ok);
        assert_eq!(
            status().await,
            Reply::ShortcutStatus(ShortcutStatus {
                backend: Backend::GnomeKeybinding,
                preferred: PerAction {
                    toggle: "Ctrl+Alt+V".parse().unwrap(),
                    hold: before.preferred.hold,
                },
                ..before
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn release_queued_before_the_start_report_stays_a_tap() {
        let mut harness = harness();
        harness.actor.hold(HoldEdge::Press);
        tokio::time::advance(Duration::from_millis(100)).await;
        let (mailboxes, senders) = mailboxes();
        // The release is already queued when the start report is handled.
        senders
            .activations
            .send(ActivationReceipt {
                activation: Activation::Hold(HoldEdge::Release),
                received_at: Instant::now(),
            })
            .unwrap();
        harness
            .actor
            .dispatch(Event::SessionStarted, ReplyTarget::NotRequested);
        // Scheduling delays the queued release beyond MIN_HOLD.
        tokio::time::advance(Duration::from_millis(400)).await;
        tokio::spawn(harness.actor.run(mailboxes));
        assert_eq!(
            [harness.session.recv().await, harness.session.recv().await],
            [Some(SessionCommand::Start), Some(SessionCommand::Teardown)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_timeout_rebuilds_shortcuts_and_allows_a_fresh_press() {
        let mut harness = harness();
        let dir = std::env::temp_dir().join(format!("voice-input-timeout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_file = dir.join("config.toml");
        std::fs::write(
            &config_file,
            "[provider]\nurl = \"http://127.0.0.1:8000/v1/audio/transcriptions\"\nmodel = \"m\"\n",
        )
        .unwrap();
        harness.actor.config_file = config_file;
        start_hold(&mut harness.actor);
        harness.actor.hold_timed_out();
        assert_eq!(
            sent(&mut harness.session),
            [SessionCommand::Start, SessionCommand::Stop]
        );

        // Once the timed-out take has finished, a new press works without
        // waiting for another release or a daemon restart.
        harness
            .actor
            .dispatch(Event::ProviderCompleted, ReplyTarget::NotRequested);
        harness.actor.hold(HoldEdge::Press);
        assert_eq!(
            sent(&mut harness.session),
            [SessionCommand::Teardown, SessionCommand::Start]
        );
    }
}
