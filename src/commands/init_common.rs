// Common validation and verification for init-new and init-from commands.

use std::path::Path;

use eyre::{Result, eyre};

use crate::volume;

/// Validate granularity is sane for dm-era.
pub fn validate_granularity(granularity: u64) -> Result<()> {
    if granularity == 0 {
        return Err(eyre!("Granularity must be greater than 0."));
    }
    if !granularity.is_multiple_of(512) {
        return Err(eyre!(
            "Granularity must be a multiple of 512 bytes (got {} bytes).",
            granularity
        ));
    }
    Ok(())
}

/// Reject configurations where virgin and scratch point to the same device.
pub fn reject_same_device(virgin: &Path, scratch: &Path) -> Result<()> {
    // Canonicalize to resolve symlinks and relative paths
    let v = std::fs::canonicalize(virgin).unwrap_or_else(|_| virgin.to_path_buf());
    let s = std::fs::canonicalize(scratch).unwrap_or_else(|_| scratch.to_path_buf());

    if v == s {
        return Err(eyre!(
            "Virgin and scratch must be different devices.\n  \
             Virgin:  {}\n  \
             Scratch: {}",
            virgin.display(),
            scratch.display()
        ));
    }
    Ok(())
}

/// Verify that scratch matches virgin after a copy by comparing superblock hashes.
pub fn verify_copy(virgin: &Path, scratch: &Path) -> Result<()> {
    let h_virgin = volume::hash_superblock(virgin)?;
    let h_scratch = volume::hash_superblock(scratch)?;

    if h_virgin != h_scratch {
        return Err(eyre!(
            "Post-copy verification failed: superblock hashes do not match.\n  \
             Virgin:  {}\n  \
             Scratch: {}",
            h_virgin
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
            h_scratch
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn granularity_zero_rejected() {
        assert!(validate_granularity(0).is_err());
    }

    #[test]
    fn granularity_not_512_multiple_rejected() {
        assert!(validate_granularity(4097).is_err());
        assert!(validate_granularity(1000).is_err());
        assert!(validate_granularity(513).is_err());
    }

    #[test]
    fn granularity_valid_accepted() {
        assert!(validate_granularity(512).is_ok());
        assert!(validate_granularity(4096).is_ok());
        assert!(validate_granularity(8192).is_ok());
    }

    #[test]
    fn same_device_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vol.img");
        File::create(&path).unwrap();

        assert!(reject_same_device(&path, &path).is_err());
    }

    #[test]
    fn different_devices_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.img");
        let b = dir.path().join("b.img");
        File::create(&a).unwrap();
        File::create(&b).unwrap();

        assert!(reject_same_device(&a, &b).is_ok());
    }

    #[test]
    fn verify_copy_detects_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.img");
        let b = dir.path().join("b.img");

        let size: u64 = 32 * 1024 * 1024;
        let f = File::create(&a).unwrap();
        f.set_len(size).unwrap();
        let f = File::create(&b).unwrap();
        f.set_len(size).unwrap();

        // Format only one
        crate::volume::mkfs_ext4(&a).unwrap();

        // Superblocks should differ
        assert!(verify_copy(&a, &b).is_err());
    }

    #[test]
    fn verify_copy_passes_when_identical() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.img");
        let b = dir.path().join("b.img");

        let size: u64 = 32 * 1024 * 1024;
        let f = File::create(&a).unwrap();
        f.set_len(size).unwrap();
        let f = File::create(&b).unwrap();
        f.set_len(size).unwrap();

        crate::volume::mkfs_ext4(&a).unwrap();
        crate::volume::full_copy(&a, &b, |_, _| {}).unwrap();

        assert!(verify_copy(&a, &b).is_ok());
    }
}
