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

use cli::{Cli, Command};

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
            fstype,
            mount_options,
            granularity,
        }) => {
            commands::init::run(
                virgin,
                scratch,
                ramdisk,
                mount_point,
                fstype,
                mount_options,
                granularity,
                cli.yes,
            )
            .await
        }
        Some(Command::FullRecover) => commands::full_recover::run(cli.yes).await,
        Some(Command::Mount) => commands::mount::run().await,
        Some(Command::Recover) => commands::recover::run().await,
        Some(Command::Promote) => commands::promote::run(cli.yes).await,
        Some(Command::Status) | None => commands::status::run().await,
    }
}
