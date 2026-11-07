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

//! Linux-only: the harness boots Xvfb with private D-Bus and AT-SPI buses
//! and drives the app with XTest. On other platforms it compiles to a stub
//! that says so.

#[cfg(target_os = "linux")]
mod cases;
#[cfg(target_os = "linux")]
mod daemon;
#[cfg(target_os = "linux")]
mod desktop;
#[cfg(target_os = "linux")]
mod session;

#[cfg(target_os = "linux")]
use std::panic::AssertUnwindSafe;
#[cfg(target_os = "linux")]
use std::process::ExitCode;
#[cfg(target_os = "linux")]
use std::time::Instant;

/// One named end-to-end case.
#[cfg(target_os = "linux")]
pub struct Case {
    pub name: &'static str,
    pub run: fn() -> anyhow::Result<()>,
}
#[cfg(target_os = "linux")]
fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match args.first().map(String::as_str) {
        Some(daemon::SUBCOMMAND) => daemon::serve(&args[1..]),
        _ => run(&args),
    }
}

/// Run every case whose name contains one of the filters, or all of them.
#[cfg(target_os = "linux")]
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

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("limen gui-e2e is only supported on Linux");
    std::process::ExitCode::from(2)
}
