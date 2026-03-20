// Command: promote
// Promotes scratch volume to become the new virgin by copying only changed blocks
//
// This is the reverse of `recover`: instead of restoring scratch from virgin,
// it updates virgin from scratch. Useful when a benchmark run has produced a
// new desired baseline state.
//
// Pre-checks:
//   1. App state must exist
//   2. Volume must be currently mounted (via previous mount command)
//   3. dmsetup and era_invalidate must be available in PATH
//
// Operation:
//   1. Unmount the filesystem (flushes writes, prevents further modifications)
//   2. Take dm-era metadata snapshot
//   3. Collect changed blocks via era_invalidate
//   4. Drop metadata snapshot
//   5. Remove dm-era device
//   6. Copy affected blocks from scratch to virgin (reversed from recover)
//   7. Update app state (clear mounted flag, recompute virgin superblock hash)
//
// This is a destructive operation - requires confirmation unless -y flag is set.

use std::io::{Write, stdout};
use std::time::Instant;

use eyre::{Result, WrapErr, eyre};

use crate::confirm;
use crate::error::not_initialized;
use crate::{dmera, env, mount, state, volume};

/// Run the promote command.
///
/// If `kill` is true, processes blocking the unmount are sent SIGKILL
/// and the unmount is retried.
pub async fn run(yes: bool, kill: bool) -> Result<()> {
    env::require_root()?;

    let _lock = state::lock()?;

    let app_state = state::load()?.ok_or_else(not_initialized)?;

    if !app_state.is_mounted {
        return Err(not_mounted());
    }

    let base_era = app_state.current_era.unwrap_or(0);

    // Check that dmsetup and era_invalidate are available
    dmera::check_dmsetup().await?;
    dmera::check_era_invalidate().await?;

    // Verify dm-era device actually exists
    if !dmera::exists(&app_state.dm_era_name).await? {
        return Err(eyre!(
            "dm-era device '{}' does not exist.\n\
             State says mounted but device is missing. This may indicate a system crash.\n\
             Run 'schelk mount' to remount, or manually reset state.",
            app_state.dm_era_name
        ));
    }

    println!("Promoting scratch to virgin (destructive)");
    println!("  Virgin:  {}", app_state.virgin.display());
    println!("  Scratch: {}", app_state.scratch.display());
    println!();
    println!("WARNING: This will permanently overwrite the virgin volume with scratch.");
    println!("         The old virgin data will be LOST and cannot be recovered.");

    confirm::require("Permanently overwrite virgin volume?", yes)?;

    println!();

    // Step 1: Unmount the filesystem (if actually mounted)
    if mount::is_mounted(&app_state.mount_point)? {
        println!("Unmounting {}...", app_state.mount_point.display());
        mount::unmount(&app_state.mount_point, kill)
            .await
            .wrap_err("Failed to unmount filesystem")?;
    } else {
        println!(
            "Mountpoint {} not actually mounted, skipping unmount...",
            app_state.mount_point.display()
        );
    }

    // Step 2: Take dm-era metadata snapshot
    println!("Taking dm-era metadata snapshot...");
    dmera::take_metadata_snapshot(&app_state.dm_era_name)
        .await
        .wrap_err("Failed to take metadata snapshot")?;

    // Step 3: Collect changed blocks with era_invalidate
    println!("Collecting changed blocks since era {}...", base_era);
    let changed_blocks = match dmera::get_changed_blocks(&app_state.ramdisk, base_era as u32) {
        Ok(blocks) => blocks,
        Err(e) => {
            // Try to drop the metadata snapshot before returning error
            let _ = dmera::drop_metadata_snapshot(&app_state.dm_era_name).await;
            return Err(e.wrap_err("Failed to collect changed blocks"));
        }
    };

    // Step 4: Drop metadata snapshot
    println!("Dropping metadata snapshot for '{}'...", app_state.dm_era_name);
    dmera::drop_metadata_snapshot(&app_state.dm_era_name)
        .await
        .wrap_err("Failed to drop metadata snapshot")?;

    // Step 5: Remove dm-era device
    println!("Removing dm-era device...");
    dmera::remove(&app_state.dm_era_name)
        .await
        .wrap_err("Failed to remove dm-era device")?;

    // Step 6: Copy affected blocks from scratch to virgin
    let total_blocks: u64 = changed_blocks.iter().map(|r| r.len).sum();
    let total_bytes = total_blocks * app_state.granularity;

    let copy_duration = if changed_blocks.is_empty() {
        println!("No blocks were modified. Nothing to promote.");
        std::time::Duration::ZERO
    } else {
        println!(
            "Copying {} changed blocks ({}) from scratch to virgin...",
            total_blocks,
            format_bytes(total_bytes)
        );

        let copy_start = Instant::now();
        let mut last_percent: u64 = 0;
        volume::copy_blocks(
            &app_state.scratch,
            &app_state.virgin,
            &changed_blocks,
            app_state.granularity,
            |copied, total| {
                let percent = if total > 0 {
                    (copied * 100) / total
                } else {
                    100
                };
                if percent != last_percent {
                    last_percent = percent;
                    print!("\r  Progress: {}%", percent);
                    let _ = stdout().flush();
                }
            },
        )
        .wrap_err("Failed to copy blocks")?;
        let elapsed = copy_start.elapsed();

        println!("\r  Progress: 100% - Done");
        elapsed
    };

    // Step 7: Update app state
    let mut app_state = app_state;
    app_state.is_mounted = false;
    app_state.current_era = None;
    // Recompute virgin superblock hash since virgin content has changed
    app_state.virgin_superblock_hash = volume::hash_superblock(&app_state.virgin)?;
    state::save(&app_state)?;

    println!();
    println!("Promote complete.");
    println!("  Blocks promoted: {}", total_blocks);
    println!("  Bytes copied:    {}", format_bytes(total_bytes));
    if total_bytes > 0 && copy_duration.as_secs_f64() > 0.0 {
        let duration_secs = copy_duration.as_secs_f64();
        let speed = total_bytes as f64 / duration_secs;
        println!("  Time elapsed:    {}", format_duration(copy_duration));
        println!("  Average speed:   {}/s", format_bytes(speed as u64));
    }

    Ok(())
}

fn not_mounted() -> eyre::Report {
    eyre!("Volume is not mounted.")
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
