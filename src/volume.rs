// Volume operations
// Handles block device validation, superblock hashing, and filesystem creation

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

/// Create a fresh ext4 filesystem on a block device or file.
///
/// Uses the `ext4-mkfs` crate (pure Rust, no system tools required) with:
/// - 4096-byte block size
/// - Journaling enabled
/// - Label "schelk"
/// - Zeroed UUID for determinism
pub fn mkfs_ext4(path: &Path) -> Result<()> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .wrap_err_with(|| format!("Cannot open {} for formatting", path.display()))?;

    let total_size = if file.metadata()?.file_type().is_block_device() {
        get_size(path)?
    } else {
        file.metadata()?.len()
    };

    let config = ext4_mkfs::MkfsConfig {
        fs_type: ext4_mkfs::FsType::Ext4,
        block_size: 4096,
        label: Some("schelk".to_string()),
        uuid: Some([0u8; 16]),
        journal: true,
        inode_size: 256,
    };

    let device = ext4_mkfs::IoBlockDevice::new(file, 4096, total_size);

    ext4_mkfs::mkfs(device, config)
        .map_err(|e| eyre!("{}", e))
        .wrap_err_with(|| format!("Failed to create ext4 filesystem on {}", path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, SeekFrom};

    #[test]
    fn mkfs_ext4_on_file() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("test.img");

        // 32 MB — minimum viable size for ext4 with journal
        let size: u64 = 32 * 1024 * 1024;
        {
            let f = File::create(&img).unwrap();
            f.set_len(size).unwrap();
        }

        mkfs_ext4(&img).expect("mkfs should succeed");

        // Verify the superblock looks like ext4 (magic number at offset 0x438)
        let mut f = File::open(&img).unwrap();
        let mut magic = [0u8; 2];
        f.seek(SeekFrom::Start(0x438)).unwrap();
        f.read_exact(&mut magic).unwrap();
        // ext4 superblock magic is 0xEF53 (little-endian)
        assert_eq!(magic, [0x53, 0xEF], "ext4 superblock magic not found");
    }
}
