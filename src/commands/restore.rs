// Command: restore
// Recovers scratch volume from virgin, then mounts it again for the next run.
//
// This is a convenience command for the common benchmark loop:
//   schelk recover && schelk mount

use eyre::{Result, WrapErr};

use crate::commands::{mount, recover};
use crate::timing::{self, StepTimer};
use crate::{env, state};

/// Run the restore command.
///
/// If `kill` is true, processes blocking the recovery unmount are sent
/// SIGKILL and the unmount is retried.
pub async fn run(kill: bool) -> Result<()> {
    env::require_root()?;

    let _lock = state::lock()?;

    let command_timer = StepTimer::start("restore", "total");

    println!("Restoring scratch volume from virgin, then remounting it.");
    println!();

    let recover_timer = StepTimer::start("restore", "recover_phase");
    recover::run_locked(kill)
        .await
        .wrap_err("Restore failed during recovery")?;
    recover_timer.finish();

    println!();
    println!("Recovery succeeded. Mounting restored scratch volume...");
    println!();

    let mount_timer = StepTimer::start("restore", "mount_phase");
    mount::run_locked()
        .await
        .wrap_err("Restore recovered the scratch volume but failed to mount it")?;
    mount_timer.finish();

    command_timer.finish_with(|operation, step, elapsed| {
        tracing::info!(
            operation = operation,
            step = step,
            elapsed_ms = timing::elapsed_ms(elapsed),
            elapsed = %timing::format_duration(elapsed),
            "completed"
        );
    });

    Ok(())
}
