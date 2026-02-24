// Command: init
// Initializes schelk configuration with volume paths and settings
//
// If app state already exists, offers to reinitialize.
//
// Parameters:
//   - virgin: path to the virgin volume (device or partition)
//   - scratch: path to the scratch volume (device or partition)
//   - ramdisk: path to the RAM disk
//   - mount_point: where to mount the scratch volume
//   - mount_options: optional mount options
//   - granularity: block size for tracking, defaults to 4096
//
// Checks performed:
//   1. Validate virgin volume is a valid block device
//   2. Validate scratch volume is a valid block device
//   3. Validate RAM disk is sufficiently sized for volume size / granularity
//   4. Check dmsetup availability and version
//   5. Check Linux version compatibility
//
// On success:
//   - Computes and stores superblock hashes for both volumes
//   - Saves app state to XDG directory
//   - Warns user that schelk now controls both volumes

use std::path::PathBuf;

use eyre::{Result, eyre};

use crate::confirm;
use crate::env;
use crate::ramdisk;
use crate::state::{self, AppState};
use crate::volume;

/// Run the init command
#[allow(clippy::too_many_arguments)]
pub async fn run(
    virgin: PathBuf,
    scratch: PathBuf,
    ramdisk: PathBuf,
    mount_point: PathBuf,
    fstype: String,
    mount_options: Option<String>,
    granularity: u64,
    yes: bool,
) -> Result<()> {
    env::require_root()?;

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
    // Note that it may actually be not a RAM disk but I guess it's fine. We can add it later.
    ramdisk::validate_size(&ramdisk, scratch_size, granularity)?;

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

    println!("schelk initialized successfully.");
    println!();
    println!("WARNING: schelk now expects to control both volumes:");
    println!("  Virgin:  {}", app_state.virgin.display());
    println!("  Scratch: {}", app_state.scratch.display());
    println!();
    println!("Do NOT mount these volumes outside of schelk, as this may corrupt your data.");

    Ok(())
}
