//! voice-input: system-wide voice input for Wayland.
//!
//! Microphone audio is buffered for one take, transcribed after the stop
//! shortcut, and committed as final text through the configured input method.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod shortcut;

use voice_input_core::{Config, HoldEdge, Paths};

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
    /// Show, set up, or change the global shortcuts
    Shortcut {
        #[command(subcommand)]
        command: shortcut::ShortcutCommand,
    },
    /// Press or release the hold-to-talk key (for compositor key-release bindings)
    Hold {
        #[arg(value_enum)]
        edge: HoldEdge,
    },
    /// Dev tool: transcribe a wav file and print the final event flow
    Probe {
        /// Input wav, must be 16kHz mono s16le
        wav: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<ExitCode> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    let cli = Cli::parse();
    let paths = Paths::discover()?;

    let code = match cli.command {
        Command::Daemon => {
            let cfg = Config::load_or_create(&paths)?;
            voice_input_core::run_daemon(paths, cfg).await?;
            ExitCode::SUCCESS
        }
        Command::Toggle => {
            let reply = voice_input_core::send(&paths, "toggle").await?;
            println!("{reply}");
            ExitCode::SUCCESS
        }
        Command::Status => {
            let reply = voice_input_core::send(&paths, "status").await?;
            println!("{reply}");
            ExitCode::SUCCESS
        }
        Command::Shortcut { command } => shortcut::run(&paths, command).await?,
        Command::Hold { edge } => {
            shortcut::hold(&paths, edge).await?;
            ExitCode::SUCCESS
        }
        Command::Probe { wav } => {
            let cfg = Config::load_or_create(&paths)?;
            voice_input_core::probe(&cfg, &wav).await?;
            ExitCode::SUCCESS
        }
    };
    Ok(code)
}
