// Command: mount
// Sets up dm-era tracking and mounts the scratch volume
//
// Pre-checks:
//   1. App state must exist
//   2. RAM disk must be adequately sized
//   3. Spot-check volume superblocks match expected state
//   4. Must not already be mounted (fool-proof check)
//
// Operation:
//   1. Zero the RAM disk
//   2. Create dm-era device: dmsetup create bench_era ... era /dev/ram0 /dev/scratch <granularity>
//   3. Mount the dm-era device at the configured mount point
//   4. Update app state to mark as mounted
//
// After this, all writes to the mounted filesystem are tracked by dm-era.
// Mount point and options come from the app state (configured during init).

use eyre::Result;

use crate::error::{not_initialized, volume_mismatch};
use crate::{dmera, env, io, mount, ramdisk, state, volume};
use eyre::eyre;

/// Run the mount command
pub async fn run() -> Result<()> {
    env::require_root()?;

    let _lock = state::lock()?;

    let app_state = state::load()?.ok_or_else(not_initialized)?;

    if app_state.is_mounted {
        return Err(crate::error::already_mounted());
    }

    // Check if mountpoint is already in use (ground-truth check for crash recovery)
    if mount::is_mounted(&app_state.mount_point)? {
        return Err(eyre!(
            "Mountpoint {} is already in use.\n\
             State says not mounted, but filesystem is mounted. This may indicate a crash.\n\
             Manually unmount with: sudo umount {}",
            app_state.mount_point.display(),
            app_state.mount_point.display()
        ));
    }

    // Check that dmsetup is available
    dmera::check_dmsetup().await?;

    // Check if dm-era device already exists (stale state from crash?)
    if dmera::exists(dmera::DM_ERA_NAME).await? {
        return Err(eyre::eyre!(
            "dm-era device '{}' already exists.\n\
             This may indicate a previous crash. Run 'schelk recover' or manually remove with:\n  \
             sudo dmsetup remove {}",
            dmera::DM_ERA_NAME,
            dmera::DM_ERA_NAME
        ));
    }

    // Step 1: Validate RAM disk is adequately sized
    println!("Validating RAM disk size...");
    let scratch_size = volume::get_size(&app_state.scratch)?;
    ramdisk::validate_size(&app_state.ramdisk, scratch_size, app_state.granularity)?;

    // Step 2: Spot-check volume superblocks match expected state
    println!("Verifying volume integrity...");
    let virgin_hash = volume::hash_superblock(&app_state.virgin)?;
    if virgin_hash != app_state.virgin_superblock_hash {
        return Err(volume_mismatch().wrap_err("Virgin volume superblock has changed"));
    }

    // Scratch should match virgin (either from init or after full-recover/recover)
    let scratch_hash = volume::hash_superblock(&app_state.scratch)?;
    if scratch_hash != app_state.virgin_superblock_hash {
        return Err(volume_mismatch().wrap_err(
            "Scratch volume superblock does not match virgin. Run 'schelk full-recover' first.",
        ));
    }

    // Step 3: Zero the RAM disk.
    //
    // I am not entirely sure if this is strictly necessary but ChatGPT was suggesting that and
    // we'll err on the safe side here.
    println!("Zeroing RAM disk...");
    io::zero(&app_state.ramdisk)?;

    // Step 4: Create dm-era device
    println!("Creating dm-era device...");
    if let Err(e) = dmera::create(
        dmera::DM_ERA_NAME,
        &app_state.ramdisk,
        &app_state.scratch,
        scratch_size,
        app_state.granularity,
    )
    .await
    {
        return Err(e.wrap_err("Failed to create dm-era device"));
    }

    // Step 5: Checkpoint to start era tracking
    println!("Initializing era tracking...");
    if let Err(e) = dmera::checkpoint(dmera::DM_ERA_NAME).await {
        // Rollback: remove dm-era device on failure
        let _ = dmera::remove(dmera::DM_ERA_NAME).await;
        return Err(e.wrap_err("Failed to checkpoint dm-era device"));
    }

    // Step 6: Mount the dm-era device
    println!(
        "Mounting {} at {}...",
        dmera::device_path().display(),
        app_state.mount_point.display()
    );
    let dm_device = dmera::device_path();
    if let Err(e) = mount::mount(
        &dm_device,
        &app_state.mount_point,
        &app_state.fstype,
        app_state.mount_options.as_deref(),
    )
    .await
    {
        // Rollback: remove dm-era device on mount failure
        let _ = dmera::remove(dmera::DM_ERA_NAME).await;
        return Err(e.wrap_err("Failed to mount dm-era device"));
    }

    // Step 7: Update app state to mark as mounted
    let mut app_state = app_state;
    app_state.is_mounted = true;
    app_state.current_era = Some(1);
    state::save(&app_state)?;

    println!();
    println!(
        "Successfully mounted at {}",
        app_state.mount_point.display()
    );
    if let Some(ref opts) = app_state.mount_options {
        println!("Mount options: {}", opts);
    }
    println!("All writes are now being tracked by dm-era.");

    Ok(())
}
