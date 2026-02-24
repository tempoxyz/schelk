// Filesystem mount/unmount operations
// Handles mounting dm-era device and unmounting for recovery

use std::fs;
use std::path::Path;

use eyre::{Result, WrapErr, eyre};
use nix::errno::Errno;
use nix::mount::MntFlags;

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
pub fn unmount(mountpoint: &Path) -> Result<()> {
    match nix::mount::umount2(mountpoint, MntFlags::empty()) {
        Ok(()) => Ok(()),
        Err(Errno::EBUSY) => {
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
            Err(eyre!(msg))
        }
        Err(e) => Err(eyre!(e)).wrap_err(format!("Failed to unmount {}", mountpoint.display())),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    /// Mount a tmpfs at `mountpoint` using nix. Requires CAP_SYS_ADMIN.
    fn mount_tmpfs(mountpoint: &Path) {
        nix::mount::mount(
            Some("tmpfs"),
            mountpoint,
            Some("tmpfs"),
            nix::mount::MsFlags::empty(),
            None::<&str>,
        )
        .expect("mount tmpfs failed (need root + CAP_SYS_ADMIN)");
    }

    /// These tests require root with CAP_SYS_ADMIN (mount/unmount privileges).
    /// Run with: cargo test -- --ignored
    #[test]
    #[ignore = "requires root + CAP_SYS_ADMIN"]
    fn unmount_succeeds_when_not_busy() {
        let dir = TempDir::new().unwrap();
        mount_tmpfs(dir.path());

        // Nothing holds the mount open — unmount should succeed.
        unmount(dir.path()).expect("unmount should succeed when not busy");

        assert!(!is_mounted(dir.path()).unwrap());
    }

    #[test]
    #[ignore = "requires root + CAP_SYS_ADMIN"]
    fn unmount_reports_blocking_process_when_busy() {
        let dir = TempDir::new().unwrap();
        mount_tmpfs(dir.path());

        // Spawn a process whose cwd is inside the mount to keep it busy.
        let mut blocker = Command::new("sleep")
            .arg("60")
            .current_dir(dir.path())
            .spawn()
            .expect("failed to spawn blocker");

        let err = unmount(dir.path()).expect_err("unmount should fail with EBUSY");
        let msg = format!("{err}");
        assert!(msg.contains("target is busy"), "unexpected error: {msg}");
        assert!(
            msg.contains(&format!("PID={}", blocker.id())),
            "error should mention the blocker PID: {msg}"
        );
        assert!(
            msg.contains("sleep"),
            "error should mention the command: {msg}"
        );

        blocker.kill().ok();
        blocker.wait().ok();
        // Clean up: unmount now that blocker is gone.
        unmount(dir.path()).ok();
    }
}
