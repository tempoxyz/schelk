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

// TODO: I think we should specify a directory, and then that directory should store the `state`
// file. This may come in handy in case we would like to recover from a failure during copying
// changed blocks out of the virgin to the scratch.
const DEFAULT_STATE_PATH: &str = "/var/lib/schelk/state.json";

/// Global override for state file path (set via CLI or env var)
static STATE_PATH_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

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
    fs::create_dir_all(&dir).wrap_err("Failed to create state directory")?;

    let lock_path = dir.join("schelk.lock");
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
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
