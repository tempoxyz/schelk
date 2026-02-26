// CLI definition using clap
// Defines subcommands: init, full-recover, mount, recover, status
// Global flags: -y (skip confirmation)
//
// Default behavior: If no subcommand is provided, runs 'status'

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "schelk")]
#[command(about = "Fast database benchmarking with surgical volume recovery")]
#[command(version)]
pub struct Cli {
    /// Skip interactive confirmations
    #[arg(short = 'y', long = "yes", global = true)]
    pub yes: bool,

    /// Path to state file (default: /var/lib/schelk/state.json)
    #[arg(long = "state-path", global = true, env = "SCHELK_STATE")]
    pub state_path: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Initialize schelk with volume configuration
    Init {
        /// Path to virgin volume (read-only golden image)
        #[arg(long)]
        virgin: PathBuf,

        /// Path to scratch volume (writable working copy)
        #[arg(long)]
        scratch: PathBuf,

        /// Path to RAM disk for dm-era metadata
        #[arg(long)]
        ramdisk: PathBuf,

        /// Mount point for the scratch volume
        #[arg(long)]
        mount_point: PathBuf,

        /// Filesystem type (e.g., "ext4", "xfs")
        #[arg(long)]
        fstype: String,

        /// Mount options (e.g., "noatime,nodiratime")
        #[arg(long)]
        mount_options: Option<String>,

        /// Block granularity in bytes
        #[arg(long, default_value = "4096")]
        granularity: u64,
    },

    /// Copy virgin volume to scratch volume (full restore)
    #[command(name = "full-recover")]
    FullRecover,

    /// Mount scratch volume with dm-era tracking
    Mount,

    /// Recover scratch volume from virgin (surgical restore)
    Recover {
        /// Kill processes blocking unmount instead of failing
        #[arg(short = 'k', long = "kill")]
        kill: bool,
    },

    /// Overwrite virgin volume with current scratch state (destructive).
    ///
    /// Permanently replaces the virgin (golden image) with the current scratch
    /// contents. The old virgin data is lost and cannot be recovered.
    /// Only use this when the scratch state should become the new baseline.
    Promote {
        /// Kill processes blocking unmount instead of failing
        #[arg(short = 'k', long = "kill")]
        kill: bool,
    },

    /// Show current status (default command)
    Status,
}
