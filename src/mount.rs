// Filesystem mount/unmount operations
// Handles mounting dm-era device and unmounting for recovery

use std::fs;
use std::path::Path;

use eyre::{Result, WrapErr};

use crate::cmd;

/// Mount a device at the specified mountpoint
///
/// Creates the mountpoint directory if it doesn't exist.
pub async fn mount(
    device: &Path,
    mountpoint: &Path,
    fstype: &str,
    options: Option<&str>,
) -> Result<()> {
    // Create mountpoint if it doesn't exist
    if !mountpoint.exists() {
        fs::create_dir_all(mountpoint)
            .wrap_err_with(|| format!("Failed to create mountpoint {}", mountpoint.display()))?;
    }

    let mut args = vec![
        "-t".to_string(),
        fstype.to_string(),
        device.to_string_lossy().into_owned(),
    ];

    if let Some(opts) = options {
        args.push("-o".to_string());
        args.push(opts.to_string());
    }

    args.push(mountpoint.to_string_lossy().into_owned());

    cmd::run("mount", &args).await.wrap_err_with(|| {
        format!(
            "Failed to mount {} at {}",
            device.display(),
            mountpoint.display()
        )
    })?;

    Ok(())
}

/// Unmount a filesystem
///
/// This flushes all pending writes and prevents further modifications.
/// Important to call before taking dm-era snapshot.
pub async fn unmount(mountpoint: &Path) -> Result<()> {
    cmd::run("umount", [&mountpoint.to_string_lossy().into_owned()])
        .await
        .wrap_err_with(|| format!("Failed to unmount {}", mountpoint.display()))?;

    Ok(())
}

/// Check if a path is currently a mountpoint
///
/// Parses /proc/mounts to determine if the path is mounted.
pub fn is_mounted(mountpoint: &Path) -> Result<bool> {
    let mounts = fs::read_to_string("/proc/mounts").wrap_err("Failed to read /proc/mounts")?;

    let mountpoint_str = mountpoint
        .canonicalize()
        .unwrap_or_else(|_| mountpoint.to_path_buf())
        .to_string_lossy()
        .into_owned();

    for line in mounts.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            // Second field is the mountpoint
            if parts[1] == mountpoint_str {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

/// Check if a device is mounted anywhere
///
/// Parses /proc/mounts to determine if the device is mounted at any mountpoint.
/// This is useful to prevent writes to a device that is actively mounted,
/// which could cause filesystem corruption.
pub fn is_device_mounted(device: &Path) -> Result<Option<String>> {
    let mounts = fs::read_to_string("/proc/mounts").wrap_err("Failed to read /proc/mounts")?;

    // Try to canonicalize the device path to resolve symlinks
    // If canonicalization fails, we'll use the original path
    let device_canonical = device.canonicalize().ok();
    let device_str = device.to_string_lossy();

    for line in mounts.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            // First field is the device, second is the mountpoint
            let mounted_device = parts[0];
            let mountpoint = parts[1];

            // Try to canonicalize the mounted device to resolve symlinks
            let mounted_canonical = Path::new(mounted_device).canonicalize().ok();

            // Check if the devices match
            if devices_match(&device_str, mounted_device, &device_canonical, &mounted_canonical) {
                return Ok(Some(mountpoint.to_string()));
            }
        }
    }

    Ok(None)
}

/// Helper function to check if two device paths refer to the same device
///
/// Compares both canonical and non-canonical paths to handle cases where
/// canonicalization might fail on one or both sides.
fn devices_match(
    device_str: &str,
    mounted_device: &str,
    device_canonical: &Option<std::path::PathBuf>,
    mounted_canonical: &Option<std::path::PathBuf>,
) -> bool {
    // If both canonicalized successfully, compare canonical paths
    if let (Some(dev_canon), Some(mount_canon)) = (device_canonical, mounted_canonical) {
        return dev_canon == mount_canon;
    }

    // Fall back to string comparison if canonicalization fails on either side
    mounted_device == device_str
        || device_canonical.as_ref().map_or(false, |dc| dc.to_string_lossy() == mounted_device)
        || mounted_canonical.as_ref().map_or(false, |mc| mc.to_string_lossy() == device_str)
}
