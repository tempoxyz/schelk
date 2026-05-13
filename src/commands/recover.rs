// Command: recover
// Restores scratch volume to virgin state by copying only changed blocks
//
// Pre-checks:
//   1. App state must exist
//   2. Volume must be currently mounted (via previous mount command)
//   3. dmsetup and era_invalidate must be available in PATH
//
// Operation:
//   1. Unmount the filesystem (flushes writes, prevents further modifications)
//   2. Take dm-era metadata snapshot: dmsetup message <dm_era_name> 0 take_metadata_snap
//   3. Collect changed blocks: era_invalidate --metadata-snapshot --written-since <era> /dev/ram0 > changed.xml
//   4. Drop metadata snapshot: dmsetup message <dm_era_name> 0 drop_metadata_snap
//   5. Remove dm-era device: dmsetup remove <dm_era_name>
//   6. Parse changed.xml and copy affected blocks from virgin to scratch
//   7. Update app state (clear mounted flag, remove changed.xml)
//
// Future: Save changed.xml to app state for crash recovery

use std::io::{Write, stdout};

use eyre::{Result, WrapErr, eyre};

use crate::error::not_initialized;
use crate::timing::{self, StepTimer};
use crate::{dmera, env, mount, state, volume};

/// Run the recover command.
///
/// If `kill` is true, processes blocking the unmount are sent SIGKILL
/// and the unmount is retried.
pub async fn run(kill: bool) -> Result<()> {
    env::require_root()?;

    let _lock = state::lock()?;

    run_locked(kill).await
}

/// Run the recover command while the caller holds the schelk state lock.
pub(crate) async fn run_locked(kill: bool) -> Result<()> {
    let command_timer = StepTimer::start("recover", "total");
    let app_state = state::load()?.ok_or_else(not_initialized)?;

    if !app_state.is_mounted {
        println!("Volume is not mounted. Nothing to recover.");
        command_timer.finish_with(|operation, step, elapsed| {
            tracing::info!(
                operation = operation,
                step = step,
                skipped = true,
                reason = "not_mounted",
                elapsed_ms = timing::elapsed_ms(elapsed),
                elapsed = %timing::format_duration(elapsed),
                "completed"
            );
        });
        return Ok(());
    }

    let base_era = app_state.current_era.unwrap_or(0);

    // Check that dmsetup and era_invalidate are available
    let prerequisite_timer = StepTimer::start("recover", "validate_prerequisites");
    dmera::check_dmsetup().await?;
    dmera::check_era_invalidate().await?;

    // Verify dm-era device actually exists
    if !dmera::exists(&app_state.dm_era_name).await? {
        return Err(eyre!(
            "dm-era device '{}' does not exist.\n\
             State says mounted but device is missing — likely a host reboot or power loss.\n\
             Incremental recovery is not possible. Run 'schelk full-recover' to restore scratch from virgin.",
            app_state.dm_era_name
        ));
    }
    prerequisite_timer.finish();

    println!("Recovering scratch volume from virgin");
    println!("  Virgin:  {}", app_state.virgin.display());
    println!("  Scratch: {}", app_state.scratch.display());
    println!();

    // Step 1: Unmount the filesystem (if actually mounted).
    // When --kill is set, any processes blocking the mount are killed first.
    let unmount_timer = StepTimer::start("recover", "unmount_filesystem");
    if mount::is_mounted(&app_state.mount_point)? {
        println!("Unmounting {}...", app_state.mount_point.display());
        mount::unmount(&app_state.mount_point, kill)
            .await
            .wrap_err("Failed to unmount filesystem")?;
        unmount_timer.finish();
    } else {
        println!(
            "Mountpoint {} not actually mounted, skipping unmount...",
            app_state.mount_point.display()
        );
        unmount_timer.finish_with(|operation, step, elapsed| {
            tracing::info!(
                operation = operation,
                step = step,
                skipped = true,
                elapsed_ms = timing::elapsed_ms(elapsed),
                elapsed = %timing::format_duration(elapsed),
                "completed"
            );
        });
    }

    // Step 2: Take dm-era metadata snapshot
    let snapshot_timer = StepTimer::start("recover", "take_metadata_snapshot");
    println!("Taking dm-era metadata snapshot...");
    dmera::take_metadata_snapshot(&app_state.dm_era_name)
        .await
        .wrap_err("Failed to take metadata snapshot")?;
    snapshot_timer.finish();

    // Step 3: Collect changed blocks with era_invalidate (via thinp library)
    let collect_timer = StepTimer::start("recover", "collect_changed_blocks");
    println!("Collecting changed blocks since era {}...", base_era);
    let changed_blocks = match dmera::get_changed_blocks(&app_state.ramdisk, base_era as u32) {
        Ok(blocks) => blocks,
        Err(e) => {
            // Try to drop the metadata snapshot before returning error
            let _ = dmera::drop_metadata_snapshot(&app_state.dm_era_name).await;
            return Err(e.wrap_err("Failed to collect changed blocks"));
        }
    };
    let total_blocks: u64 = changed_blocks.iter().map(|r| r.len).sum();
    collect_timer.finish_with(|operation, step, elapsed| {
        tracing::info!(
            operation = operation,
            step = step,
            ranges = changed_blocks.len(),
            blocks = total_blocks,
            elapsed_ms = timing::elapsed_ms(elapsed),
            elapsed = %timing::format_duration(elapsed),
            "completed"
        );
    });

    // Step 4: Drop metadata snapshot
    let drop_snapshot_timer = StepTimer::start("recover", "drop_metadata_snapshot");
    println!(
        "Dropping metadata snapshot for '{}'...",
        app_state.dm_era_name
    );
    dmera::drop_metadata_snapshot(&app_state.dm_era_name)
        .await
        .wrap_err("Failed to drop metadata snapshot")?;
    drop_snapshot_timer.finish();

    // Step 5: Remove dm-era device
    let remove_timer = StepTimer::start("recover", "remove_dm_era_device");
    println!("Removing dm-era device...");
    dmera::remove(&app_state.dm_era_name)
        .await
        .wrap_err("Failed to remove dm-era device")?;
    remove_timer.finish();

    // Step 6: Copy affected blocks from virgin to scratch
    let total_bytes = total_blocks * app_state.granularity;

    let copy_timer = StepTimer::start("recover", "copy_changed_blocks");
    let copy_duration = if changed_blocks.is_empty() {
        println!("No blocks were modified. Nothing to recover.");
        copy_timer.finish_with(|operation, step, elapsed| {
            tracing::info!(
                operation = operation,
                step = step,
                skipped = true,
                blocks = 0,
                bytes = 0,
                elapsed_ms = timing::elapsed_ms(elapsed),
                elapsed = %timing::format_duration(elapsed),
                "completed"
            );
        })
    } else {
        println!(
            "Copying {} changed blocks ({})...",
            total_blocks,
            format_bytes(total_bytes)
        );

        let mut last_percent: u64 = 0;
        volume::copy_blocks(
            &app_state.virgin,
            &app_state.scratch,
            &changed_blocks,
            app_state.granularity,
            |copied, total| {
                let percent = copied.saturating_mul(100).checked_div(total).unwrap_or(100);
                if percent != last_percent {
                    last_percent = percent;
                    print!("\r  Progress: {}%", percent);
                    let _ = stdout().flush();
                }
            },
        )
        .wrap_err("Failed to copy blocks")?;
        let elapsed = copy_timer.finish_with(|operation, step, elapsed| {
            tracing::info!(
                operation = operation,
                step = step,
                blocks = total_blocks,
                bytes = total_bytes,
                elapsed_ms = timing::elapsed_ms(elapsed),
                elapsed = %timing::format_duration(elapsed),
                "completed"
            );
        });

        println!("\r  Progress: 100% - Done");
        elapsed
    };

    // Step 7: Update app state
    let state_timer = StepTimer::start("recover", "save_state");
    let mut app_state = app_state;
    app_state.is_mounted = false;
    app_state.current_era = None;
    state::save(&app_state)?;
    state_timer.finish();

    println!();
    println!("Recovery complete.");
    println!("  Blocks restored: {}", total_blocks);
    println!("  Bytes copied:    {}", format_bytes(total_bytes));
    if total_bytes > 0 && copy_duration.as_secs_f64() > 0.0 {
        let duration_secs = copy_duration.as_secs_f64();
        let speed = total_bytes as f64 / duration_secs;
        println!("  Time elapsed:    {}", format_duration(copy_duration));
        println!("  Average speed:   {}/s", format_bytes(speed as u64));
    }

    command_timer.finish_with(|operation, step, elapsed| {
        tracing::info!(
            operation = operation,
            step = step,
            blocks = total_blocks,
            bytes = total_bytes,
            elapsed_ms = timing::elapsed_ms(elapsed),
            elapsed = %timing::format_duration(elapsed),
            "completed"
        );
    });

    Ok(())
}

/// Format bytes in human-readable form
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} bytes", bytes)
    }
}

/// Format duration in human-readable form
fn format_duration(duration: std::time::Duration) -> String {
    let secs = duration.as_secs_f64();
    if secs >= 60.0 {
        let mins = (secs / 60.0).floor();
        let remaining_secs = secs - (mins * 60.0);
        format!("{:.0}m {:.2}s", mins, remaining_secs)
    } else if secs >= 1.0 {
        format!("{:.2}s", secs)
    } else {
        format!("{:.0}ms", secs * 1000.0)
    }
}
