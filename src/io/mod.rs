// Low-level I/O operations for block devices
// Handles reading, writing, copying, and zeroing of block devices

mod uring;

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use eyre::{Result, WrapErr, eyre};

/// Size of superblock to read for hashing (4KB)
const SUPERBLOCK_SIZE: usize = 4096;

/// Buffer size for zeroing (1MB)
const BUFFER_SIZE: usize = 1024 * 1024;

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
