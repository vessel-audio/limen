use std::time::Duration;

use super::{FIRST_RETRY, Input, LAST_RETRY, Machine, Output, initial_status};
use crate::config::ShortcutConfig;
use crate::shortcut::portal::connection::{Call, Generation, Report};
use crate::shortcut::status::{PerAction, SessionState, ShortcutStatus, Trigger};
use crate::shortcut::{Activation, Backend, HoldEdge};

const G1: Generation = Generation(1);
const G2: Generation = Generation(2);

fn portal() -> ShortcutConfig {
    ShortcutConfig {
        backend: Backend::Portal,
        ..ShortcutConfig::default()
    }
}

fn preferred() -> PerAction<crate::shortcut::Shortcut> {
    PerAction {
        toggle: portal().toggle,
        hold: portal().hold,
    }
}

fn status(session: SessionState) -> ShortcutStatus {
    ShortcutStatus {
        backend: Backend::Portal,
        session,
        preferred: preferred(),
        actual: PerAction::default(),
        last_error: None,
    }
}

fn actual() -> PerAction<Option<Trigger>> {
    PerAction {
        toggle: Some(Trigger::new("Press <Super>z".to_string())),
        hold: Some(Trigger::new("Press <Super>x".to_string())),
    }
}

fn bound() -> ShortcutStatus {
    ShortcutStatus {
        actual: actual(),
        ..status(SessionState::Bound)
    }
}

/// Feed `inputs` in order; the outputs of each.
fn run(machine: &mut Machine, inputs: impl IntoIterator<Item = Input>) -> Vec<Vec<Output>> {
    inputs
        .into_iter()
        .map(|input| machine.handle(input))
        .collect()
}

/// A machine whose first generation has bound the shortcuts.
fn bound_machine() -> Machine {
    let mut machine = Machine::default();
    run(
        &mut machine,
        [
            Input::Rebind(portal()),
            Input::Report(G1, Report::Connected),
            Input::Report(G1, Report::Registered),
            Input::Report(G1, Report::SessionCreated),
            Input::Report(G1, Report::Bound(actual())),
        ],
    );
    machine
}

#[test]
fn each_call_waits_for_the_one_before() {
    let mut machine = Machine::default();
    assert_eq!(
        run(
            &mut machine,
            [
                Input::Rebind(portal()),
                Input::Report(G1, Report::Connected),
                Input::Report(G1, Report::Registered),
                Input::Report(G1, Report::SessionCreated),
                Input::Report(G1, Report::Bound(actual())),
            ]
        ),
        [
            vec![
                Output::Connect(G1),
                Output::Publish(status(SessionState::Binding)),
                Output::Rebound,
            ],
            vec![Output::Call(G1, Call::Register)],
            vec![Output::Call(G1, Call::CreateSession)],
            vec![Output::Call(G1, Call::Bind(preferred()))],
            vec![Output::Publish(bound())],
        ]
    );
}

#[test]
fn a_failed_registration_never_creates_a_session() {
    let mut machine = Machine::default();
    let failure = "the portal did not register the app".to_string();
    assert_eq!(
        run(
            &mut machine,
            [
                Input::Rebind(portal()),
                Input::Report(G1, Report::Connected),
                Input::Report(G1, Report::Failed(failure.clone())),
                Input::Report(G1, Report::Registered),
                Input::Ended(G1),
                Input::RetryDue,
            ]
        ),
        [
            vec![
                Output::Connect(G1),
                Output::Publish(status(SessionState::Binding)),
                Output::Rebound,
            ],
            vec![Output::Call(G1, Call::Register)],
            vec![
                Output::Stop(G1),
                Output::Publish(ShortcutStatus {
                    last_error: Some(failure.clone()),
                    ..status(SessionState::Failed)
                }),
                Output::Notify(failure.clone()),
            ],
            vec![],
            vec![Output::RetryAfter(FIRST_RETRY)],
            vec![
                Output::Connect(G2),
                Output::Publish(ShortcutStatus {
                    last_error: Some(failure),
                    ..status(SessionState::Binding)
                }),
            ],
        ]
    );
}

#[test]
fn shortcut_signals_become_dictation_input() {
    let mut machine = bound_machine();
    let changed = PerAction {
        toggle: Some(Trigger::new("Press <Super>F9".to_string())),
        hold: None,
    };
    assert_eq!(
        run(
            &mut machine,
            [
                Input::Report(G1, Report::Activated("toggle".to_string())),
                Input::Report(G1, Report::Deactivated("toggle".to_string())),
                Input::Report(G1, Report::Activated("hold".to_string())),
                Input::Report(G1, Report::Deactivated("hold".to_string())),
                Input::Report(G1, Report::Activated("unknown".to_string())),
                Input::Report(G1, Report::Changed(changed.clone())),
            ]
        ),
        [
            vec![Output::Activate(Activation::Toggle)],
            vec![],
            vec![Output::Activate(Activation::Hold(HoldEdge::Press))],
            vec![Output::Activate(Activation::Hold(HoldEdge::Release))],
            vec![],
            vec![Output::Publish(ShortcutStatus {
                actual: changed,
                ..status(SessionState::Bound)
            })],
        ]
    );
}

#[test]
fn a_rebind_starts_anew_only_after_the_old_generation_ended() {
    let mut machine = bound_machine();
    assert_eq!(
        run(
            &mut machine,
            [
                Input::Rebind(portal()),
                // Rebinding again while the old one closes starts nothing more.
                Input::Rebind(portal()),
                Input::Report(G1, Report::Activated("toggle".to_string())),
                Input::Ended(G1),
                Input::Report(G1, Report::Activated("toggle".to_string())),
                Input::Report(G1, Report::Bound(actual())),
            ]
        ),
        [
            vec![Output::Stop(G1)],
            vec![],
            vec![],
            vec![
                Output::Connect(G2),
                Output::Publish(status(SessionState::Binding)),
                Output::Rebound,
            ],
            vec![],
            vec![],
        ]
    );
}

#[test]
fn switching_to_another_backend_disables_the_session() {
    let mut machine = bound_machine();
    let none = ShortcutConfig {
        backend: Backend::GnomeKeybinding,
        ..portal()
    };
    assert_eq!(
        run(&mut machine, [Input::Rebind(none), Input::Ended(G1)]),
        [
            vec![Output::Stop(G1)],
            vec![
                Output::Publish(ShortcutStatus {
                    backend: Backend::GnomeKeybinding,
                    ..status(SessionState::Disabled)
                }),
                Output::Rebound,
            ],
        ]
    );
}

#[test]
fn a_portal_restart_sets_up_again_at_once() {
    let mut machine = bound_machine();
    assert_eq!(
        run(
            &mut machine,
            [Input::Report(G1, Report::PortalRestarted), Input::Ended(G1)]
        ),
        [
            vec![Output::Stop(G1)],
            vec![
                Output::Connect(G2),
                Output::Publish(status(SessionState::Binding))
            ],
        ]
    );
}

#[test]
fn a_cancelled_dialog_is_not_retried() {
    let mut machine = Machine::default();
    run(
        &mut machine,
        [
            Input::Rebind(portal()),
            Input::Report(G1, Report::Connected),
            Input::Report(G1, Report::Registered),
            Input::Report(G1, Report::SessionCreated),
        ],
    );
    assert_eq!(
        run(
            &mut machine,
            [
                Input::Report(G1, Report::Cancelled),
                Input::Ended(G1),
                Input::RetryDue
            ]
        ),
        [
            vec![
                Output::Stop(G1),
                Output::Publish(status(SessionState::Cancelled))
            ],
            vec![],
            vec![],
        ]
    );
}

/// Fail the running generation `generation` with `error` and let it end.
fn fail(machine: &mut Machine, generation: Generation, error: &str) -> Vec<Output> {
    run(
        machine,
        [
            Input::Report(generation, Report::Connected),
            Input::Report(generation, Report::Failed(error.to_string())),
            Input::Ended(generation),
            Input::RetryDue,
        ],
    )
    .concat()
}

fn waits(outputs: &[Output]) -> Vec<Duration> {
    outputs
        .iter()
        .filter_map(|output| match output {
            Output::RetryAfter(wait) => Some(*wait),
            _ => None,
        })
        .collect()
}

fn notified(outputs: &[Output]) -> usize {
    outputs
        .iter()
        .filter(|output| matches!(output, Output::Notify(_)))
        .count()
}

#[test]
fn failures_back_off_up_to_a_cap_and_reset_after_success() {
    let mut machine = Machine::default();
    machine.handle(Input::Rebind(portal()));
    let mut all = Vec::new();
    for generation in 1..=12 {
        all.extend(fail(&mut machine, Generation(generation), "no portal"));
    }
    let backoff = waits(&all);
    assert!(
        backoff.windows(2).all(|pair| pair[0] <= pair[1]),
        "{backoff:?}"
    );
    assert_eq!(
        (backoff.first(), backoff.last()),
        (Some(&FIRST_RETRY), Some(&LAST_RETRY))
    );
    assert!(backoff.iter().all(|wait| *wait <= LAST_RETRY));

    // Bind generation 13, then fail it: the wait starts over.
    let g13 = Generation(13);
    run(
        &mut machine,
        [
            Input::Report(g13, Report::Connected),
            Input::Report(g13, Report::Registered),
            Input::Report(g13, Report::SessionCreated),
            Input::Report(g13, Report::Bound(actual())),
            Input::Report(g13, Report::Failed("no portal".to_string())),
        ],
    );
    assert_eq!(
        machine.handle(Input::Ended(g13)),
        [Output::RetryAfter(FIRST_RETRY)]
    );
}

#[test]
fn a_repeated_failure_is_notified_once() {
    let mut machine = Machine::default();
    machine.handle(Input::Rebind(portal()));
    let counts = [
        (G1, "no portal"),
        (G2, "no portal"),
        (Generation(3), "no portal"),
        (Generation(4), "Register failed"),
    ]
    .map(|(generation, error)| notified(&fail(&mut machine, generation, error)));
    assert_eq!(counts, [1, 0, 0, 1]);
}

#[test]
fn a_task_that_dies_counts_as_a_failure() {
    let mut machine = bound_machine();
    let error = "the portal connection ended".to_string();
    assert_eq!(
        machine.handle(Input::Ended(G1)),
        [
            Output::Publish(ShortcutStatus {
                last_error: Some(error.clone()),
                ..ShortcutStatus {
                    session: SessionState::Failed,
                    ..bound()
                }
            }),
            Output::Notify(error),
            Output::RetryAfter(FIRST_RETRY),
        ]
    );
}

#[test]
fn a_hold_left_down_is_released_when_its_generation_stops() {
    let press = Input::Report(G1, Report::Activated("hold".to_string()));
    let release = Output::Activate(Activation::Hold(HoldEdge::Release));

    let mut restarted = bound_machine();
    restarted.handle(press.clone());
    assert_eq!(
        restarted.handle(Input::Report(G1, Report::PortalRestarted)),
        [release.clone(), Output::Stop(G1)]
    );

    let mut rebound = bound_machine();
    rebound.handle(press.clone());
    assert_eq!(
        rebound.handle(Input::Rebind(portal())),
        [release.clone(), Output::Stop(G1)]
    );

    let mut died = bound_machine();
    died.handle(press.clone());

    // The whole output of a generation that died holding the key.
    let error = "the portal connection ended".to_string();
    assert_eq!(
        died.handle(Input::Ended(G1)),
        [
            release,
            Output::Publish(ShortcutStatus {
                last_error: Some(error.clone()),
                ..ShortcutStatus {
                    session: SessionState::Failed,
                    ..bound()
                }
            }),
            Output::Notify(error),
            Output::RetryAfter(FIRST_RETRY),
        ]
    );

    // Released before: nothing left to release.
    let mut let_go = bound_machine();
    let_go.handle(press);
    let_go.handle(Input::Report(G1, Report::Deactivated("hold".to_string())));
    assert_eq!(let_go.handle(Input::Rebind(portal())), [Output::Stop(G1)]);
}

#[test]
fn startup_status_shows_the_pending_portal_binding() {
    assert_eq!(initial_status(portal()), status(SessionState::Binding));
    let gnome = ShortcutConfig {
        backend: Backend::GnomeKeybinding,
        ..portal()
    };
    assert_eq!(
        initial_status(gnome),
        ShortcutStatus {
            backend: Backend::GnomeKeybinding,
            ..status(SessionState::Disabled)
        }
    );
}
