// Command: init-new
// Creates fresh ext4 filesystems on both volumes from scratch.
//
// This command is DESTRUCTIVE: it creates a fresh ext4 filesystem on the virgin
// volume, then copies virgin to scratch so both are byte-identical. All existing
// data on both volumes will be lost.
//
// If app state already exists, offers to reinitialize.

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Instant;

use eyre::{Result, eyre};

use crate::confirm;
use crate::dmera;
use crate::env;
use crate::ramdisk;
use crate::state::{self, AppState};
use crate::volume;

/// Run the init-new command
#[expect(clippy::too_many_arguments)]
pub async fn run(
    virgin: PathBuf,
    scratch: PathBuf,
    ramdisk: PathBuf,
    mount_point: PathBuf,
    mount_options: Option<String>,
    granularity: u64,
    dm_era_name: String,
    yes: bool,
) -> Result<()> {
    env::require_root()?;

    super::init_common::validate_granularity(granularity)?;
    super::init_common::reject_same_device(&virgin, &scratch)?;
    dmera::validate_name(&dm_era_name)?;

    // Check that mkfs.ext4 is available before doing anything else
    volume::check_mkfs_ext4().await?;

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
            yes,
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

    // Warn that this is destructive
    println!("This will create a fresh ext4 filesystem on the virgin volume");
    println!("and copy it to the scratch volume.");
    println!("All existing data on BOTH volumes will be destroyed.");
    println!();
    println!("  Virgin:  {} ({} bytes)", virgin.display(), virgin_size);
    println!("  Scratch: {} ({} bytes)", scratch.display(), scratch_size);
    println!("  Block size: 4096 bytes (ext4)");
    println!("  Journaling: enabled");
    println!();

    confirm::require("Proceed with initialization?", yes)?;

    // Create ext4 filesystem on virgin volume
    println!();
    println!("Creating ext4 filesystem on virgin volume...");
    volume::mkfs_ext4(&virgin)?;
    println!("  done.");

    // Copy virgin to scratch so both volumes are identical
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

    // Compute virgin superblock hash for integrity checks
    let virgin_superblock_hash = volume::hash_superblock(&virgin)?;

    let app_state = AppState {
        virgin,
        scratch,
        ramdisk,
        mount_point,
        fstype: "ext4".to_string(),
        mount_options,
        granularity,
        virgin_superblock_hash,
        dm_era_name,
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
