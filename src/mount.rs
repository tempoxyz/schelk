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
///
/// If unmount fails because the volume is busy, the error message lists the
/// processes that are still using the mountpoint (PID + command line).
pub async fn unmount(mountpoint: &Path) -> Result<()> {
    let output = cmd::run_unchecked("umount", [&mountpoint.to_string_lossy().into_owned()])
        .await
        .wrap_err_with(|| format!("Failed to unmount {}", mountpoint.display()))?;

    if output.success {
        return Ok(());
    }

    let stderr = output.stderr.trim();
    if stderr.contains("busy") {
        let procs = find_processes_using(mountpoint);
        let mut msg = format!(
            "Failed to unmount {}: target is busy\n",
            mountpoint.display()
        );
        if procs.is_empty() {
            msg.push_str("  (could not identify blocking processes)");
        } else {
            msg.push_str("Processes still using the mount:\n");
            for (pid, cmdline) in &procs {
                msg.push_str(&format!("  PID={pid} {cmdline:?}\n"));
            }
        }
        return Err(eyre::eyre!(msg));
    }

    // Non-busy failure — preserve original error
    let msg = if stderr.is_empty() {
        format!(
            "Failed to unmount {}: exit code {:?}",
            mountpoint.display(),
            output.code
        )
    } else {
        format!("Failed to unmount {}: {}", mountpoint.display(), stderr)
    };
    Err(eyre::eyre!(msg))
}

/// Scan /proc to find processes with open files or cwd under `mountpoint`.
///
/// Returns a list of (pid, cmdline) pairs. Best-effort: silently skips
/// entries that are unreadable (e.g. kernel threads, races with exiting
/// processes).
fn find_processes_using(mountpoint: &Path) -> Vec<(u32, String)> {
    let mountpoint = mountpoint
        .canonicalize()
        .unwrap_or_else(|_| mountpoint.to_path_buf());

    let mut results: Vec<(u32, String)> = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return results;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };

        let proc_dir = entry.path();

        if is_under(&proc_dir.join("cwd"), &mountpoint)
            || is_under(&proc_dir.join("root"), &mountpoint)
            || has_fd_under(&proc_dir.join("fd"), &mountpoint)
        {
            let cmdline = fs::read_to_string(proc_dir.join("cmdline"))
                .unwrap_or_default()
                .replace('\0', " ")
                .trim()
                .to_string();
            results.push((pid, cmdline));
        }
    }

    results
}

/// Check whether a symlink (e.g. /proc/<pid>/cwd) resolves to a path under `mountpoint`.
fn is_under(link: &Path, mountpoint: &Path) -> bool {
    fs::read_link(link)
        .map(|target| target.starts_with(mountpoint))
        .unwrap_or(false)
}

/// Check whether any fd in /proc/<pid>/fd/ points under `mountpoint`.
fn has_fd_under(fd_dir: &Path, mountpoint: &Path) -> bool {
    let Ok(entries) = fs::read_dir(fd_dir) else {
        return false;
    };
    for entry in entries.flatten() {
        if is_under(&entry.path(), mountpoint) {
            return true;
        }
    }
    false
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
