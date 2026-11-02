//! Limen: system-wide voice input for Wayland.
//!
//! Microphone audio is buffered for one take, transcribed after the stop
//! shortcut, and committed as final text through the configured input method.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};

mod shortcut;

use limen_core::{Config, HoldEdge, Paths};

#[derive(Parser)]
#[command(
    name = "limen",
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
    /// Open the graphical control panel
    #[cfg(feature = "gui")]
    Gui,
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

fn main() -> anyhow::Result<ExitCode> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    let cli = Cli::parse();
    let paths = Paths::discover()?;

    match cli.command {
        #[cfg(feature = "gui")]
        Command::Gui => {
            limen_gui::run(paths, Arc::new(limen_rig::Rig::new()?)).map(|()| ExitCode::SUCCESS)
        }
        command => run_async(command, paths),
    }
}

fn run_async(command: Command, paths: Paths) -> anyhow::Result<ExitCode> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async move {
            let code = match command {
                #[cfg(feature = "gui")]
                Command::Gui => unreachable!("GUI commands do not use the async runtime"),
                Command::Daemon => {
                    let cfg = Config::load_or_create(&paths)?;
                    limen_core::run_daemon(paths, cfg, Arc::new(limen_rig::Rig::new()?)).await?;
                    ExitCode::SUCCESS
                }
                Command::Toggle => {
                    let reply = limen_core::send(&paths, "toggle").await?;
                    println!("{reply}");
                    ExitCode::SUCCESS
                }
                Command::Status => {
                    let reply = limen_core::send(&paths, "status").await?;
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
                    limen_core::probe(&cfg, &limen_rig::Rig::new()?, &wav).await?;
                    ExitCode::SUCCESS
                }
            };
            Ok(code)
        })
}
