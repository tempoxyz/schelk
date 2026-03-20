// App state management
// Persists configuration and runtime state to /var/lib/schelk
// All writes must be atomic (write to temp, fsync, rename) for crash safety

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::PathBuf;

use eyre::{Result, WrapErr, eyre};
use nix::fcntl::{Flock, FlockArg};
use serde::{Deserialize, Serialize};

use std::sync::OnceLock;

use crate::dmera;

// TODO: I think we should specify a directory, and then that directory should store the `state`
// file. This may come in handy in case we would like to recover from a failure during copying
// changed blocks out of the virgin to the scratch.
const DEFAULT_STATE_PATH: &str = "/var/lib/schelk/state.json";

/// Global override for state file path (set via CLI or env var)
static STATE_PATH_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

fn default_dm_era_name() -> String {
    dmera::DEFAULT_DM_ERA_NAME.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppState {
    /// Path to virgin volume (read-only golden image)
    pub virgin: PathBuf,
    /// Path to scratch volume (writable working copy)
    pub scratch: PathBuf,
    /// Path to RAM disk for dm-era metadata
    pub ramdisk: PathBuf,
    /// Mount point for the scratch volume
    pub mount_point: PathBuf,
    // TODO: Maybe auto-detect. I think mount may be doing that by default.
    /// Filesystem type (e.g., "ext4", "xfs")
    pub fstype: String,
    /// Mount options (e.g., "noatime,nodiratime")
    pub mount_options: Option<String>,
    /// Block granularity in bytes (default 4096)
    pub granularity: u64,
    /// SHA-256 hash of virgin superblock for integrity checks
    /// Scratch superblock should always match this (before mount and after recover)
    pub virgin_superblock_hash: [u8; 32],
    /// Device-mapper name for the dm-era target (e.g., "bench_era").
    /// Defaults to "bench_era" when absent (backwards-compatible with older state files).
    #[serde(default = "default_dm_era_name")]
    pub dm_era_name: String,
    /// Whether dm-era is active and volume is mounted
    pub is_mounted: bool,
    /// Current dm-era epoch for tracking changes
    pub current_era: Option<u64>,
}

/// Set the state file path override (call once at startup)
pub fn set_path_override(path: PathBuf) {
    STATE_PATH_OVERRIDE.set(path).unwrap();
}

/// Returns the path to the schelk state file
/// Uses override if set, otherwise /var/lib/schelk/state.json
pub fn state_path() -> Result<PathBuf> {
    if let Some(path) = STATE_PATH_OVERRIDE.get() {
        Ok(path.clone())
    } else {
        Ok(PathBuf::from(DEFAULT_STATE_PATH))
    }
}

/// Returns the state directory for schelk
fn state_dir() -> Result<PathBuf> {
    let path = state_path()?;
    Ok(path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/")))
}

/// Load app state from disk
/// Returns None if state file doesn't exist
pub fn load() -> Result<Option<AppState>> {
    let path = state_path()?;

    if !path.exists() {
        return Ok(None);
    }

    let mut file = File::open(&path).wrap_err("Failed to open state file")?;

    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .wrap_err("Failed to read state file")?;

    let state: AppState = serde_json::from_str(&contents).wrap_err("State file is corrupted")?;

    Ok(Some(state))
}

/// Acquire an exclusive flock on the schelk lock file.
///
/// Returns an owned `Flock<File>` that holds the lock until dropped. This prevents concurrent
/// schelk operations (e.g. two `recover` or `mount` calls) from racing on the same volumes.
///
/// The lock file lives next to the state file (e.g. `/var/lib/schelk/schelk.lock`).
pub fn lock() -> Result<Flock<File>> {
    let dir = state_dir()?;
    lock_path(&dir.join("schelk.lock"))
}

/// Acquire an exclusive flock on a specific lock file path.
///
/// Same as [`lock`] but targets an arbitrary path instead of the default state directory.
/// Useful for testing.
pub fn lock_path(lock_path: &std::path::Path) -> Result<Flock<File>> {
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)
            .wrap_err_with(|| format!("Failed to create directory: {}", parent.display()))?;
    }

    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .wrap_err_with(|| format!("Failed to open lock file: {}", lock_path.display()))?;

    Flock::lock(file, FlockArg::LockExclusiveNonblock).map_err(|(_, errno)| {
        eyre!(
            "Another schelk process is already running (flock on {}): {}",
            lock_path.display(),
            errno
        )
    })
}

/// Save app state to disk atomically
/// Uses write-to-temp, fsync, rename pattern for crash safety
pub fn save(state: &AppState) -> Result<()> {
    let path = state_path()?;
    let dir = state_dir()?;

    // Ensure directory exists
    fs::create_dir_all(&dir).wrap_err("Failed to create state directory")?;

    // Serialize state
    let contents = serde_json::to_string_pretty(state).wrap_err("Failed to serialize state")?;

    // Write to temporary file in same directory (for atomic rename)
    let temp_path = dir.join(".state.json.tmp");
    let mut file = File::create(&temp_path).wrap_err("Failed to create temp state file")?;

    file.write_all(contents.as_bytes())
        .wrap_err("Failed to write temp state file")?;

    // Fsync the file to ensure data is on disk
    file.sync_all().wrap_err("Failed to fsync state file")?;

    // Atomic rename
    fs::rename(&temp_path, &path).wrap_err("Failed to rename state file")?;

    // Fsync the directory to ensure rename is persisted
    let dir_file = File::open(&dir).wrap_err("Failed to open state directory")?;
    dir_file
        .sync_all()
        .wrap_err("Failed to fsync state directory")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lock_fails_while_first_held() {
        let dir = tempfile::tempdir().unwrap();
        let lf = dir.path().join("schelk.lock");

        let _first = lock_path(&lf).expect("first lock should succeed");
        let err = lock_path(&lf).expect_err("second lock should fail");
        assert!(
            format!("{err}").contains("Another schelk process"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn lock_released_after_drop() {
        let dir = tempfile::tempdir().unwrap();
        let lf = dir.path().join("schelk.lock");

        let first = lock_path(&lf).expect("first lock should succeed");
        drop(first);
        let _second = lock_path(&lf).expect("lock should succeed after drop");
    }

    #[test]
    fn old_state_without_dm_era_name_uses_default() {
        // Simulate a state file from before dm_era_name was added
        let json = r#"{
            "virgin": "/dev/nvme1n1",
            "scratch": "/dev/nvme2n1",
            "ramdisk": "/dev/ram0",
            "mount_point": "/schelk",
            "fstype": "ext4",
            "mount_options": null,
            "granularity": 4096,
            "virgin_superblock_hash": [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
            "is_mounted": false,
            "current_era": null
        }"#;
        let state: AppState = serde_json::from_str(json).unwrap();
        assert_eq!(state.dm_era_name, dmera::DEFAULT_DM_ERA_NAME);
    }

    #[test]
    fn custom_dm_era_name_roundtrips() {
        let state = AppState {
            virgin: PathBuf::from("/dev/a"),
            scratch: PathBuf::from("/dev/b"),
            ramdisk: PathBuf::from("/dev/ram0"),
            mount_point: PathBuf::from("/mnt"),
            fstype: "ext4".to_string(),
            mount_options: None,
            granularity: 4096,
            virgin_superblock_hash: [0u8; 32],
            dm_era_name: "my_custom_era".to_string(),
            is_mounted: false,
            current_era: None,
        };
        let json = serde_json::to_string(&state).unwrap();
        let loaded: AppState = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.dm_era_name, "my_custom_era");
    }

    #[test]
    fn lock_contention_across_processes() {
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let lf = dir.path().join("schelk.lock");

        // Hold the lock in this process
        let _held = lock_path(&lf).expect("lock should succeed");

        // Spawn a child that tries to flock the same file (non-blocking)
        let status = Command::new("flock")
            .args(["--nonblock", "--exclusive"])
            .arg(&lf)
            .arg("true")
            .status()
            .expect("failed to run flock(1)");

        assert!(
            !status.success(),
            "child flock should fail while parent holds lock"
        );
    }
}
