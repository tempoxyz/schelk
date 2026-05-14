// Low-level I/O operations for block devices
// Handles reading, writing, copying, and resetting block devices

mod uring;

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;
use std::process::{Command, Stdio};

use eyre::{Result, WrapErr, eyre};
use nix::sys::stat::{major, minor};

/// Size of superblock to read for hashing (4KB)
const SUPERBLOCK_SIZE: usize = 4096;

/// Buffer size for zeroing (1MB)
const BUFFER_SIZE: usize = 1024 * 1024;

/// Linux block major for brd-backed `/dev/ramN` devices.
const RAMDISK_MAJOR: u64 = 1;

/// A range of blocks to copy
#[derive(Debug, Clone)]
pub struct BlockRange {
    /// Starting block number
    pub start: u64,
    /// Number of consecutive blocks
    pub len: u64,
}

/// Get the size of a block device in bytes
pub fn get_size(path: &Path) -> Result<u64> {
    let file = File::open(path).wrap_err_with(|| format!("Cannot open {}", path.display()))?;

    let size = file
        .metadata()
        .ok()
        .and_then(|m| if m.len() > 0 { Some(m.len()) } else { None })
        .or_else(|| {
            // For block devices, metadata().len() returns 0; seek to end instead
            let mut f = file;
            f.seek(SeekFrom::End(0)).ok()
        })
        .ok_or_else(|| eyre!("Cannot determine size of {}", path.display()))?;

    Ok(size)
}

/// Read the superblock (first 4KB) of a device
pub fn read_superblock(path: &Path) -> Result<Vec<u8>> {
    let mut file = File::open(path).wrap_err_with(|| format!("Cannot open {}", path.display()))?;

    let mut buf = vec![0u8; SUPERBLOCK_SIZE];
    file.read_exact(&mut buf)
        .wrap_err_with(|| format!("Cannot read superblock from {}", path.display()))?;

    Ok(buf)
}

/// Method used to reset a device back to zeroes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetMethod {
    /// Device was discarded with `blkdiscard`.
    Blkdiscard,
    /// Device was overwritten with zeroes.
    ZeroWrite,
}

impl ResetMethod {
    pub fn description(self) -> &'static str {
        match self {
            Self::Blkdiscard => "blkdiscard",
            Self::ZeroWrite => "sequential zero write",
        }
    }
}

/// Reset the dm-era metadata device to all zeroes.
///
/// For Linux brd devices (`/dev/ramN`), discard frees the backing pages and reads from missing
/// pages return zeroes. That makes resetting large ramdisks much cheaper than writing zeroes
/// through the entire device. Other devices keep the conservative zero-write behavior because
/// discard does not universally guarantee zero-filled reads.
pub fn reset_to_zero(path: &Path) -> Result<ResetMethod> {
    if can_reset_brd_with_blkdiscard(path)? {
        blkdiscard(path)?;
        return Ok(ResetMethod::Blkdiscard);
    }

    zero(path)?;
    Ok(ResetMethod::ZeroWrite)
}

/// Zero the entire block device.
pub fn zero(path: &Path) -> Result<()> {
    let size = get_size(path)?;

    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .wrap_err_with(|| format!("Cannot open {} for writing", path.display()))?;

    let zeros = vec![0u8; BUFFER_SIZE];
    let mut written: u64 = 0;

    while written < size {
        let to_write = std::cmp::min(BUFFER_SIZE as u64, size - written) as usize;
        file.write_all(&zeros[..to_write])
            .wrap_err("Failed to zero device")?;
        written += to_write as u64;
    }

    file.sync_all().wrap_err("Failed to sync device")?;

    Ok(())
}

fn can_reset_brd_with_blkdiscard(path: &Path) -> Result<bool> {
    let Some(sysfs_path) = brd_sysfs_path(path)? else {
        return Ok(false);
    };

    if !blkdiscard_available() {
        return Ok(false);
    }

    let discard_max_bytes = read_sysfs_u64(&sysfs_path.join("queue/discard_max_bytes"))?;
    if discard_max_bytes == 0 {
        return Ok(false);
    }

    let discard_granularity = read_sysfs_u64(&sysfs_path.join("queue/discard_granularity"))?;
    let size = get_size(path)?;
    if discard_granularity == 0 || size % discard_granularity != 0 {
        return Ok(false);
    }

    Ok(true)
}

fn blkdiscard_available() -> bool {
    Command::new("blkdiscard")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

fn brd_sysfs_path(path: &Path) -> Result<Option<std::path::PathBuf>> {
    let metadata =
        fs::metadata(path).wrap_err_with(|| format!("Cannot stat {}", path.display()))?;
    if !metadata.file_type().is_block_device() {
        return Ok(None);
    }

    let major = major(metadata.rdev());
    if major != RAMDISK_MAJOR {
        return Ok(None);
    }

    let sysfs_path = fs::canonicalize(format!(
        "/sys/dev/block/{}:{}",
        major,
        minor(metadata.rdev())
    ))
    .wrap_err_with(|| format!("Cannot inspect sysfs metadata for {}", path.display()))?;

    let Some(name) = sysfs_path.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };

    Ok(is_whole_brd_device_name(name).then_some(sysfs_path))
}

fn is_whole_brd_device_name(name: &str) -> bool {
    name.strip_prefix("ram")
        .is_some_and(|suffix| !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()))
}

fn read_sysfs_u64(path: &Path) -> Result<u64> {
    fs::read_to_string(path)
        .wrap_err_with(|| format!("Cannot read {}", path.display()))?
        .trim()
        .parse::<u64>()
        .wrap_err_with(|| format!("Cannot parse {}", path.display()))
}

fn blkdiscard(path: &Path) -> Result<()> {
    let output = Command::new("blkdiscard")
        .arg(path)
        .output()
        .wrap_err("Failed to execute blkdiscard")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = stderr.trim();
        if msg.is_empty() {
            return Err(eyre!(
                "blkdiscard failed with exit code {:?}",
                output.status.code()
            ));
        }
        return Err(eyre!("blkdiscard failed: {}", msg));
    }

    Ok(())
}

/// Full block-level copy from source to destination.
/// Shows progress via callback and returns bytes copied.
pub fn full_copy<F>(src: &Path, dst: &Path, progress: F) -> Result<u64>
where
    F: FnMut(u64, u64),
{
    uring::full_copy(src, dst, progress)
}

/// Copy specific block ranges from source to destination using io_uring.
/// Used for incremental recovery.
pub fn copy_blocks<F>(
    src: &Path,
    dst: &Path,
    blocks: &[BlockRange],
    granularity: u64,
    progress: F,
) -> Result<u64>
where
    F: FnMut(u64, u64),
{
    uring::copy_blocks(src, dst, blocks, granularity, progress)
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::fd::AsRawFd;

    use super::{ResetMethod, is_whole_brd_device_name, reset_to_zero};

    #[test]
    fn brd_name_detection_accepts_whole_ramdisks() {
        assert!(is_whole_brd_device_name("ram0"));
        assert!(is_whole_brd_device_name("ram12"));
    }

    #[test]
    fn brd_name_detection_rejects_other_devices_and_partitions() {
        assert!(!is_whole_brd_device_name("ram"));
        assert!(!is_whole_brd_device_name("ram0p1"));
        assert!(!is_whole_brd_device_name("loop0"));
        assert!(!is_whole_brd_device_name("nvme0n1"));
    }

    #[test]
    fn reset_to_zero_falls_back_to_zero_write_for_regular_files() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&[0xff; 8192]).unwrap();
        file.flush().unwrap();

        let path = format!("/proc/self/fd/{}", file.as_raw_fd());
        let method = reset_to_zero(std::path::Path::new(&path)).unwrap();
        assert_eq!(method, ResetMethod::ZeroWrite);

        file.seek(SeekFrom::Start(0)).unwrap();
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).unwrap();
        assert!(contents.iter().all(|byte| *byte == 0));
    }
}
