// Volume operations
// Handles block device validation and superblock hashing

use std::fs::File;
use std::os::unix::fs::FileTypeExt;
use std::path::Path;

use eyre::{Result, WrapErr, eyre};
use sha2::{Digest, Sha256};

use crate::io;

// Re-export BlockRange for backward compatibility
pub use crate::io::BlockRange;

/// Validate that a path is a valid block device we can access
pub fn validate_block_device(path: &Path) -> Result<()> {
    let metadata =
        std::fs::metadata(path).wrap_err_with(|| format!("Cannot access {}", path.display()))?;

    if !metadata.file_type().is_block_device() {
        return Err(eyre!("{} is not a block device", path.display()));
    }

    // Try to open for read to check permissions
    File::open(path).wrap_err_with(|| format!("Cannot read {}", path.display()))?;

    Ok(())
}

/// Get the size of a block device in bytes
pub fn get_size(path: &Path) -> Result<u64> {
    io::get_size(path)
}

/// Compute SHA-256 hash of the superblock
pub fn hash_superblock(path: &Path) -> Result<[u8; 32]> {
    let superblock = io::read_superblock(path)?;

    let mut hasher = Sha256::new();
    hasher.update(&superblock);
    let result = hasher.finalize();

    Ok(result.into())
}

/// Full block-level copy from source to destination
/// Shows progress and returns bytes copied
pub fn full_copy<F>(src: &Path, dst: &Path, progress: F) -> Result<u64>
where
    F: FnMut(u64, u64),
{
    io::full_copy(src, dst, progress)
}

/// Copy specific block ranges from source to destination using parallel I/O
/// Used for incremental recovery
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
    io::copy_blocks(src, dst, blocks, granularity, progress)
}
