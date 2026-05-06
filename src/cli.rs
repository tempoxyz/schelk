// CLI definition using clap
// Defines subcommands: init-new, init-from, full-recover, mount, recover, restore, promote, status
// Global flags: -y (skip confirmation)
//
// Default behavior: If no subcommand is provided, runs 'status'

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::dmera;

const HELP_FOOTER: &str = "AI-agent setup and usage guide:\n  https://github.com/tempoxyz/schelk/blob/master/docs/SKILL.md";

/// Version string including the git SHA when available (e.g. "0.1.0 (abc1234)").
fn version_string() -> &'static str {
    if let Some(sha) = option_env!("SCHELK_GIT_SHA") {
        // Leak is fine — called once, lives for the program's lifetime.
        Box::leak(format!("{} ({})", env!("CARGO_PKG_VERSION"), sha).into_boxed_str())
    } else {
        env!("CARGO_PKG_VERSION")
    }
}

#[derive(Parser)]
#[command(name = "schelk")]
#[command(about = "Fast database benchmarking with surgical volume recovery")]
#[command(version = version_string())]
#[command(after_long_help = HELP_FOOTER)]
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
    /// Create a fresh ext4 filesystem on the virgin volume, then clone it to scratch (destructive)
    #[command(name = "init-new")]
    InitNew {
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

        /// Mount options (e.g., "noatime,nodiratime")
        #[arg(long)]
        mount_options: Option<String>,

        /// Block granularity in bytes
        #[arg(long, default_value = "4096")]
        granularity: u64,

        /// Device-mapper name for the dm-era target.
        #[arg(long, default_value = dmera::DEFAULT_DM_ERA_NAME)]
        dm_era_name: String,
    },

    /// Adopt an existing pre-populated virgin volume (destructive to scratch)
    #[command(name = "init-from")]
    InitFrom {
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

        /// Device-mapper name for the dm-era target.
        #[arg(long, default_value = dmera::DEFAULT_DM_ERA_NAME)]
        dm_era_name: String,

        /// Skip copying virgin to scratch
        #[arg(long)]
        no_copy: bool,
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

    /// Recover scratch volume from virgin, then mount it again
    Restore {
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
