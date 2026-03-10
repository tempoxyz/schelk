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

/// Create a fresh ext4 filesystem on a block device or file.
///
/// Uses the `ext4-mkfs` crate (pure Rust, no system tools required) with:
/// - 4096-byte block size
/// - Journaling enabled
/// - Label "schelk"
/// - Zeroed UUID (note: output is not fully deterministic due to lwext4 internals)
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

    // Ensure the formatted data is durable on disk before continuing
    let file = File::open(path)?;
    file.sync_all()
        .wrap_err_with(|| format!("Failed to sync {} after formatting", path.display()))?;

    Ok(())
}

/// Validate that a path exists and is accessible for reading and writing.
/// Unlike `validate_block_device`, this does not require a block device — it
/// also accepts regular files. Used for testing with file-backed images.
#[cfg(test)]
fn validate_volume(path: &Path) -> Result<()> {
    let metadata =
        std::fs::metadata(path).wrap_err_with(|| format!("Cannot access {}", path.display()))?;

    let ft = metadata.file_type();
    if !ft.is_block_device() && !ft.is_file() {
        return Err(eyre!(
            "{} is neither a block device nor a regular file",
            path.display()
        ));
    }

    File::open(path).wrap_err_with(|| format!("Cannot read {}", path.display()))?;

    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, SeekFrom};

    fn create_image(path: &std::path::Path, size: u64) {
        let f = File::create(path).unwrap();
        f.set_len(size).unwrap();
    }

    #[test]
    fn mkfs_ext4_creates_valid_superblock() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("test.img");

        // 32 MB — minimum viable size for ext4 with journal
        create_image(&img, 32 * 1024 * 1024);
        mkfs_ext4(&img).expect("mkfs should succeed");

        // Verify ext4 superblock magic at offset 0x438
        let mut f = File::open(&img).unwrap();
        let mut magic = [0u8; 2];
        f.seek(SeekFrom::Start(0x438)).unwrap();
        f.read_exact(&mut magic).unwrap();
        assert_eq!(magic, [0x53, 0xEF], "ext4 superblock magic not found");
    }

    #[test]
    fn full_copy_produces_identical_volumes() {
        let dir = tempfile::tempdir().unwrap();
        let virgin = dir.path().join("virgin.img");
        let scratch = dir.path().join("scratch.img");

        let size: u64 = 32 * 1024 * 1024;
        create_image(&virgin, size);
        create_image(&scratch, size);

        // Format virgin
        mkfs_ext4(&virgin).unwrap();

        // Copy virgin to scratch
        full_copy(&virgin, &scratch, |_, _| {}).unwrap();

        // Read both fully and compare
        let mut v = Vec::new();
        let mut s = Vec::new();
        File::open(&virgin).unwrap().read_to_end(&mut v).unwrap();
        File::open(&scratch).unwrap().read_to_end(&mut s).unwrap();

        assert_eq!(v.len(), s.len());
        assert_eq!(v, s, "virgin and scratch must be byte-identical after copy");
    }

    #[test]
    fn superblock_hash_matches_after_copy() {
        let dir = tempfile::tempdir().unwrap();
        let virgin = dir.path().join("virgin.img");
        let scratch = dir.path().join("scratch.img");

        let size: u64 = 32 * 1024 * 1024;
        create_image(&virgin, size);
        create_image(&scratch, size);

        mkfs_ext4(&virgin).unwrap();
        full_copy(&virgin, &scratch, |_, _| {}).unwrap();

        let h1 = hash_superblock(&virgin).unwrap();
        let h2 = hash_superblock(&scratch).unwrap();
        assert_eq!(h1, h2, "superblock hashes must match after copy");
    }

    #[test]
    fn validate_volume_accepts_files() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("test.img");
        create_image(&img, 1024);

        assert!(validate_volume(&img).is_ok());
    }

    #[test]
    fn validate_volume_rejects_nonexistent() {
        let result = validate_volume(std::path::Path::new("/nonexistent/path"));
        assert!(result.is_err());
    }

    #[test]
    fn init_new_workflow_on_files() {
        // Simulates the full init-new workflow using file images
        let dir = tempfile::tempdir().unwrap();
        let virgin = dir.path().join("virgin.img");
        let scratch = dir.path().join("scratch.img");

        let size: u64 = 32 * 1024 * 1024;
        create_image(&virgin, size);
        create_image(&scratch, size);

        // Validate volumes
        validate_volume(&virgin).unwrap();
        validate_volume(&scratch).unwrap();

        // Sizes must match
        assert_eq!(get_size(&virgin).unwrap(), get_size(&scratch).unwrap());

        // Create ext4 on virgin
        mkfs_ext4(&virgin).unwrap();

        // Copy to scratch
        let copied = full_copy(&virgin, &scratch, |_, _| {}).unwrap();
        assert_eq!(copied, size);

        // Superblock hashes match
        let h1 = hash_superblock(&virgin).unwrap();
        let h2 = hash_superblock(&scratch).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn init_from_workflow_on_files() {
        // Simulates the init-from workflow: adopt existing virgin, copy to scratch
        let dir = tempfile::tempdir().unwrap();
        let virgin = dir.path().join("virgin.img");
        let scratch = dir.path().join("scratch.img");

        let size: u64 = 32 * 1024 * 1024;
        create_image(&virgin, size);
        create_image(&scratch, size);

        // Pre-format the virgin (simulating user-prepared volume)
        mkfs_ext4(&virgin).unwrap();

        // Validate
        validate_volume(&virgin).unwrap();
        validate_volume(&scratch).unwrap();
        assert_eq!(get_size(&virgin).unwrap(), get_size(&scratch).unwrap());

        // Copy
        let copied = full_copy(&virgin, &scratch, |_, _| {}).unwrap();
        assert_eq!(copied, size);

        // Verify identity
        let h1 = hash_superblock(&virgin).unwrap();
        let h2 = hash_superblock(&scratch).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn init_from_no_copy_workflow_on_files() {
        // Simulates init-from --no-copy: user pre-prepared identical volumes
        let dir = tempfile::tempdir().unwrap();
        let virgin = dir.path().join("virgin.img");
        let scratch = dir.path().join("scratch.img");

        let size: u64 = 32 * 1024 * 1024;
        create_image(&virgin, size);
        create_image(&scratch, size);

        // Pre-format virgin
        mkfs_ext4(&virgin).unwrap();

        // Manually copy to scratch (simulating user doing this themselves)
        full_copy(&virgin, &scratch, |_, _| {}).unwrap();

        // Now the --no-copy path: just validate and hash, no copy
        validate_volume(&virgin).unwrap();
        validate_volume(&scratch).unwrap();
        assert_eq!(get_size(&virgin).unwrap(), get_size(&scratch).unwrap());

        // Hash virgin (this is all init-from --no-copy does)
        let h = hash_superblock(&virgin).unwrap();
        assert_ne!(h, [0u8; 32], "hash should be non-zero for formatted volume");
    }
}
