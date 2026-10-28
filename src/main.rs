//! voice-input: system-wide voice input for Wayland.
//!
//! Microphone audio is buffered for one take, transcribed after the stop
//! shortcut, and committed as final text through the configured input method.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod audio;
mod config;
mod daemon;
mod dictation;
mod input_method;
mod ipc;
mod notify;
mod polish;
mod provider;
mod session;

#[derive(Parser)]
#[command(
    name = "voice-input",
    version,
    about = "Wayland voice input: mic -> ASR -> type into focused window"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Resident daemon: captures audio, drives the ASR session, commits text
    Daemon,
    /// Send the start/stop signal to the daemon (bind to a GNOME shortcut)
    Toggle,
    /// Query daemon state
    Status,
    /// Dev tool: transcribe a wav file and print the final event flow
    Probe {
        /// Input wav, must be 16kHz mono s16le
        wav: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    let cli = Cli::parse();
    let paths = config::Paths::discover()?;

    match cli.command {
        Command::Daemon => {
            let cfg = config::Config::load_or_create(&paths)?;
            daemon::run(paths, cfg).await
        }
        Command::Toggle => {
            let reply = ipc::send(&paths, "toggle").await?;
            println!("{reply}");
            Ok(())
        }
        Command::Status => {
            let reply = ipc::send(&paths, "status").await?;
            println!("{reply}");
            Ok(())
        }
        Command::Probe { wav } => {
            let cfg = config::Config::load_or_create(&paths)?;
            provider::probe(&cfg, &wav).await
        }
    }
}
