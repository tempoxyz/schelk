// Command: full-recover
// Performs a complete copy from virgin volume to scratch volume
//
// This is a costly operation that should ideally be performed only once
// to initialize the scratch volume as an exact copy of the virgin.
//
// Pre-checks:
//   1. App state must exist (init must have been run)
//   2. Scratch volume must not be mounted
//
// Operation:
//   - Full block-level copy from virgin to scratch
//   - Updates superblock hash in app state after completion
//
// This is a destructive operation - requires confirmation unless -y flag is set.

use std::io::{self, Write};
use std::time::Instant;

use eyre::Result;

use crate::confirm;
use crate::error::{already_mounted, not_initialized};
use crate::{dmera, env, mount, state, volume};

/// Run the full-recover command
pub async fn run(yes: bool) -> Result<()> {
    env::require_root()?;

    let mut app_state = state::load()?.ok_or_else(not_initialized)?;

    if app_state.is_mounted {
        // After a reboot or power loss the state file still says "mounted" but
        // the dm-era device and filesystem mount are gone.  Detect this stale
        // state and allow full-recover to proceed — there is nothing to unmount.
        let dm_era_exists = dmera::exists(&app_state.dm_era_name).await?;
        let fs_mounted = mount::is_mounted(&app_state.mount_point)?;

        if dm_era_exists || fs_mounted {
            return Err(already_mounted());
        }

        println!(
            "State says mounted, but dm-era device and filesystem are gone \
             (likely a reboot or power loss)."
        );
        println!("Clearing stale mounted state and proceeding with full recovery.");
        println!();

        app_state.is_mounted = false;
        app_state.current_era = None;
        state::save(&app_state)?;
    }

    // Validate volumes are accessible
    // TODO: here we need to read accessible virgin and write accessible scratch. Be more precise
    // here.
    volume::validate_block_device(&app_state.virgin)?;
    volume::validate_block_device(&app_state.scratch)?;

    // Check sizes match
    let virgin_size = volume::get_size(&app_state.virgin)?;
    let scratch_size = volume::get_size(&app_state.scratch)?;

    if virgin_size != scratch_size {
        return Err(eyre::eyre!(
            "Volume size mismatch: virgin is {} bytes, scratch is {} bytes",
            virgin_size,
            scratch_size
        ));
    }

    println!("Full recovery from virgin to scratch volume");
    println!();
    println!(
        "  Virgin:  {} ({} bytes)",
        app_state.virgin.display(),
        virgin_size
    );
    println!(
        "  Scratch: {} ({} bytes)",
        app_state.scratch.display(),
        scratch_size
    );
    println!();
    println!("WARNING: This will overwrite ALL data on the scratch volume.");

    confirm::require("Proceed with full recovery?", yes)?;

    println!();
    println!("Copying...");

    let start = Instant::now();
    let mut last_print = Instant::now();

    let copied = volume::full_copy(&app_state.virgin, &app_state.scratch, |copied, total| {
        // TODO: Use a more beautiful progress bar.
        // Update progress at most every 100ms
        if last_print.elapsed().as_millis() >= 100 {
            let percent = (copied as f64 / total as f64) * 100.0;
            let mb_copied = copied / (1024 * 1024);
            let mb_total = total / (1024 * 1024);
            print!("\r  {} / {} MB ({:.1}%)    ", mb_copied, mb_total, percent);
            let _ = io::stdout().flush();
            last_print = Instant::now();
        }
    })?;

    let elapsed = start.elapsed();
    let mb_copied = copied / (1024 * 1024);
    let speed = mb_copied as f64 / elapsed.as_secs_f64();

    println!(
        "\r  {} MB copied in {:.1}s ({:.1} MB/s)    ",
        mb_copied,
        elapsed.as_secs_f64(),
        speed
    );
    println!();

    // Update virgin superblock hash in state (scratch now matches virgin)
    println!("Updating state...");
    app_state.virgin_superblock_hash = volume::hash_superblock(&app_state.virgin)?;
    state::save(&app_state)?;

    println!("Full recovery complete.");

    Ok(())
}
