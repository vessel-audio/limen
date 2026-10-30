//! Desktop end-to-end tests for the GPUI control panel.
//!
//! Each case launches the shipped `limen gui` binary inside its own
//! Xvfb display with private session and accessibility buses, finds controls
//! through AT-SPI, drives them with XTest pointer and keyboard input, and
//! observes the effects the operating system owns: the clipboard, window
//! manager requests, rendered pixels, spawned programs, and the daemon
//! protocol. Cases run serially because they share pointer, keyboard, and
//! focus state.
//!
//! Run them with `cargo gui-e2e` inside `nix develop`; every case leaves a
//! final screenshot and its logs under `target/tmp/gui-e2e/<module>/<case>/`.

mod cases;
mod daemon;
mod desktop;
mod session;

use std::panic::AssertUnwindSafe;
use std::process::ExitCode;
use std::time::Instant;

/// One named end-to-end case.
pub struct Case {
    pub name: &'static str,
    pub run: fn() -> anyhow::Result<()>,
}

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match args.first().map(String::as_str) {
        Some(daemon::SUBCOMMAND) => daemon::serve(&args[1..]),
        _ => run(&args),
    }
}

/// Run every case whose name contains one of the filters, or all of them.
fn run(args: &[String]) -> ExitCode {
    let filters = args
        .iter()
        .filter(|arg| !arg.starts_with('-'))
        .collect::<Vec<_>>();
    let selected = cases::ALL
        .iter()
        .filter(|case| {
            filters.is_empty()
                || filters
                    .iter()
                    .any(|filter| case.name.contains(filter.as_str()))
        })
        .collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "--list") {
        selected
            .iter()
            .for_each(|case| println!("{}: test", case.name));
        return ExitCode::SUCCESS;
    }
    println!("\nrunning {} desktop e2e cases", selected.len());
    let started = Instant::now();
    let failures = selected
        .iter()
        .filter_map(|case| {
            session::begin_case(case.name);
            let outcome = std::panic::catch_unwind(AssertUnwindSafe(case.run));
            let failure = match outcome {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(format!("{error:?}")),
                Err(panic) => Some(
                    panic
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| panic.downcast_ref::<&str>().map(|text| text.to_string()))
                        .unwrap_or_else(|| "panicked".to_string()),
                ),
            };
            println!(
                "test {} ... {}",
                case.name,
                match failure {
                    None => "ok",
                    Some(_) => "FAILED",
                }
            );
            failure.map(|failure| (case.name, failure))
        })
        .collect::<Vec<_>>();
    for (name, failure) in &failures {
        println!("\n---- {name} ----\n{failure}");
        println!("artifacts: {}", session::artifacts_for(name).display());
    }
    println!(
        "\ntest result: {}. {} passed; {} failed; finished in {:.1}s",
        match failures.is_empty() {
            true => "ok",
            false => "FAILED",
        },
        selected.len() - failures.len(),
        failures.len(),
        started.elapsed().as_secs_f64()
    );
    match failures.is_empty() {
        true => ExitCode::SUCCESS,
        false => ExitCode::FAILURE,
    }
}
