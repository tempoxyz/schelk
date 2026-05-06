// Command: restore
// Recovers scratch volume from virgin, then mounts it again for the next run.
//
// This is a convenience command for the common benchmark loop:
//   schelk recover && schelk mount

use eyre::{Result, WrapErr};

use crate::commands::{mount, recover};
use crate::{env, state};

/// Run the restore command.
///
/// If `kill` is true, processes blocking the recovery unmount are sent
/// SIGKILL and the unmount is retried.
pub async fn run(kill: bool) -> Result<()> {
    env::require_root()?;

    let _lock = state::lock()?;

    println!("Restoring scratch volume from virgin, then remounting it.");
    println!();

    recover::run_locked(kill)
        .await
        .wrap_err("Restore failed during recovery")?;

    println!();
    println!("Recovery succeeded. Mounting restored scratch volume...");
    println!();

    mount::run_locked()
        .await
        .wrap_err("Restore recovered the scratch volume but failed to mount it")?;

    Ok(())
}
