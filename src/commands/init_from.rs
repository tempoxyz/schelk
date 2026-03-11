// Command: init-from
// Adopts an existing, pre-populated virgin volume.
//
// Use this when you have already prepared the virgin volume with data (e.g.,
// loaded a database snapshot, run schema migrations) and want schelk to take
// control of it.
//
// This is DESTRUCTIVE: the scratch volume will be overwritten with a full copy
// of the virgin. With --no-copy, the scratch volume is left untouched (the user
// asserts both volumes are already identical).
//
// If app state already exists, offers to reinitialize.

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Instant;

use eyre::{Result, eyre};

use crate::confirm;
use crate::env;
use crate::ramdisk;
use crate::state::{self, AppState};
use crate::volume;

/// Run the init-from command
#[expect(clippy::too_many_arguments)]
pub async fn run(
    virgin: PathBuf,
    scratch: PathBuf,
    ramdisk: PathBuf,
    mount_point: PathBuf,
    fstype: String,
    mount_options: Option<String>,
    granularity: u64,
    no_copy: bool,
    yes: bool,
    reinit: bool,
) -> Result<()> {
    env::require_root()?;

    super::init_common::validate_granularity(granularity)?;
    super::init_common::reject_same_device(&virgin, &scratch)?;

    // Check if already initialized.
    if let Some(existing) = state::load()? {
        confirm::require(
            &format!(
                "schelk is already initialized:\n  \
                 Path: {}\n\
                 Virgin:  {}\n  \
                 Scratch: {}\n\
                 Reinitialize?",
                state::state_path()?.display(),
                existing.virgin.display(),
                existing.scratch.display()
            ),
            reinit,
        )?;
    }

    // Validate volumes are valid block devices
    volume::validate_block_device(&virgin)?;
    volume::validate_block_device(&scratch)?;

    // Check that virgin and scratch volumes are the same size
    let virgin_size = volume::get_size(&virgin)?;
    let scratch_size = volume::get_size(&scratch)?;
    if virgin_size != scratch_size {
        return Err(eyre!(
            "Virgin and scratch volumes must be the same size.\n  \
             Virgin:  {} ({} bytes)\n  \
             Scratch: {} ({} bytes)",
            virgin.display(),
            virgin_size,
            scratch.display(),
            scratch_size
        ));
    }

    // Validate RAM disk is accessible and sufficiently sized.
    ramdisk::validate_size(&ramdisk, scratch_size, granularity)?;

    if no_copy {
        println!("Adopting existing volumes (--no-copy mode).");
        println!("You assert that both volumes are already identical.");
        println!();
        println!("  Virgin:  {} ({} bytes)", virgin.display(), virgin_size);
        println!("  Scratch: {} ({} bytes)", scratch.display(), scratch_size);
        println!();
        println!("WARNING: If the volumes are NOT identical, subsequent recovers");
        println!("will produce corrupt results.");
        println!();

        confirm::require("Proceed?", yes)?;
    } else {
        println!("Adopting existing virgin volume.");
        println!("The scratch volume will be overwritten with a full copy of the virgin.");
        println!();
        println!("  Virgin:  {} ({} bytes)", virgin.display(), virgin_size);
        println!("  Scratch: {} ({} bytes)", scratch.display(), scratch_size);
        println!();

        confirm::require("Proceed?", yes)?;

        // Copy virgin to scratch
        println!();
        println!("Copying virgin to scratch...");
        let start = Instant::now();
        let mut last_print = Instant::now();

        let copied = volume::full_copy(&virgin, &scratch, |copied, total| {
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
    }

    // Compute virgin superblock hash for integrity checks
    let virgin_superblock_hash = volume::hash_superblock(&virgin)?;

    let app_state = AppState {
        virgin,
        scratch,
        ramdisk,
        mount_point,
        fstype,
        mount_options,
        granularity,
        virgin_superblock_hash,
        is_mounted: false,
        current_era: None,
    };

    state::save(&app_state)?;

    println!();
    println!("schelk initialized successfully.");
    println!();
    println!("WARNING: schelk now expects to control both volumes:");
    println!("  Virgin:  {}", app_state.virgin.display());
    println!("  Scratch: {}", app_state.scratch.display());
    println!();
    println!("Do NOT mount these volumes outside of schelk, as this may corrupt your data.");

    Ok(())
}
