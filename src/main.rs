// Entry point for schelk CLI
// Parses command-line arguments and dispatches to appropriate command handlers

use clap::Parser;

mod cli;
mod cmd;
mod commands;
mod confirm;
mod dmera;
mod env;
mod error;
mod io;
mod mount;
mod ramdisk;
mod state;
mod volume;

use std::process;

use tokio::signal::unix::{SignalKind, signal};

use cli::{Cli, Command};

/// Install signal handlers that ignore SIGINT and SIGTERM, printing a warning instead.
///
/// Commands like `recover`, `mount`, `promote`, and `full-recover` manipulate dm-era devices
/// and block volumes in multi-step sequences. Interrupting them mid-way can leave volumes in
/// an inconsistent state that requires a costly full recovery. We catch these signals and
/// refuse to act on them, letting the operation complete naturally.
fn ignore_termination_signals() {
    // Register signal handlers synchronously so they take effect immediately,
    // before the spawned task is polled. This avoids a race where a signal
    // arrives before the async block starts executing.
    let mut sigint = signal(SignalKind::interrupt()).unwrap();
    let mut sigterm = signal(SignalKind::terminate()).unwrap();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = sigint.recv() => {
                    eprintln!(
                        "\nRefusing to handle signal. Interrupting this operation can corrupt your volumes.\n\
                         If you really want to stop it, use: kill -9 {}",
                        process::id()
                    );
                }
                _ = sigterm.recv() => {
                    eprintln!(
                        "\nRefusing to handle signal. Interrupting this operation can corrupt your volumes.\n\
                         If you really want to stop it, use: kill -9 {}",
                        process::id()
                    );
                }
            }
        }
    });
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    color_eyre::install()?;

    let cli = Cli::parse();

    // Set state path override if provided
    if let Some(state_path) = cli.state_path {
        state::set_path_override(state_path);
    }

    match cli.command {
        Some(Command::Init {
            virgin,
            scratch,
            ramdisk,
            mount_point,
            mount_options,
            granularity,
        }) => {
            commands::init::run(
                virgin,
                scratch,
                ramdisk,
                mount_point,
                mount_options,
                granularity,
                cli.yes,
            )
            .await
        }
        Some(Command::FullRecover) => {
            ignore_termination_signals();
            commands::full_recover::run(cli.yes).await
        }
        Some(Command::Mount) => {
            ignore_termination_signals();
            commands::mount::run().await
        }
        Some(Command::Recover { kill }) => {
            ignore_termination_signals();
            commands::recover::run(kill).await
        }
        Some(Command::Promote { kill }) => {
            ignore_termination_signals();
            commands::promote::run(cli.yes, kill).await
        }
        Some(Command::Status) | None => commands::status::run().await,
    }
}
