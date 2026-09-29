// Common validation for init-new and init-from commands.

use std::path::Path;

use eyre::{Result, eyre};

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

/// Reject configurations where any managed devices alias each other.
pub fn reject_device_aliases(virgin: &Path, scratch: &Path, ramdisk: &Path) -> Result<()> {
    let v = std::fs::canonicalize(virgin).unwrap_or_else(|_| virgin.to_path_buf());
    let s = std::fs::canonicalize(scratch).unwrap_or_else(|_| scratch.to_path_buf());
    let r = std::fs::canonicalize(ramdisk).unwrap_or_else(|_| ramdisk.to_path_buf());

    if v == s {
        return Err(eyre!(
            "Virgin and scratch must be different devices.\n  \
             Virgin:  {}\n  \
             Scratch: {}",
            virgin.display(),
            scratch.display()
        ));
    }
    if v == r {
        return Err(eyre!(
            "Virgin and RAM disk must be different devices.\n  \
             Virgin:  {}\n  \
             RAM disk: {}",
            virgin.display(),
            ramdisk.display()
        ));
    }
    if s == r {
        return Err(eyre!(
            "Scratch and RAM disk must be different devices.\n  \
             Scratch: {}\n  \
             RAM disk: {}",
            scratch.display(),
            ramdisk.display()
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
    }

    #[test]
    fn granularity_valid_accepted() {
        assert!(validate_granularity(512).is_ok());
        assert!(validate_granularity(4096).is_ok());
    }

    #[test]
    fn virgin_scratch_alias_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let virgin = dir.path().join("virgin.img");
        let ramdisk = dir.path().join("ramdisk.img");
        File::create(&virgin).unwrap();
        File::create(&ramdisk).unwrap();

        assert!(reject_device_aliases(&virgin, &virgin, &ramdisk).is_err());
    }

    #[test]
    fn ramdisk_alias_of_virgin_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let virgin = dir.path().join("virgin.img");
        let scratch = dir.path().join("scratch.img");
        File::create(&virgin).unwrap();
        File::create(&scratch).unwrap();

        assert!(reject_device_aliases(&virgin, &scratch, &virgin).is_err());
    }

    #[test]
    fn ramdisk_alias_of_scratch_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let virgin = dir.path().join("virgin.img");
        let scratch = dir.path().join("scratch.img");
        File::create(&virgin).unwrap();
        File::create(&scratch).unwrap();

        assert!(reject_device_aliases(&virgin, &scratch, &scratch).is_err());
    }

    #[test]
    fn ramdisk_symlink_alias_rejected() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let virgin = dir.path().join("virgin.img");
        let scratch = dir.path().join("scratch.img");
        let ramdisk_alias = dir.path().join("ramdisk.img");
        File::create(&virgin).unwrap();
        File::create(&scratch).unwrap();
        symlink(&scratch, &ramdisk_alias).unwrap();

        assert!(reject_device_aliases(&virgin, &scratch, &ramdisk_alias).is_err());
    }

    #[test]
    fn distinct_devices_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let virgin = dir.path().join("virgin.img");
        let scratch = dir.path().join("scratch.img");
        let ramdisk = dir.path().join("ramdisk.img");
        File::create(&virgin).unwrap();
        File::create(&scratch).unwrap();
        File::create(&ramdisk).unwrap();

        assert!(reject_device_aliases(&virgin, &scratch, &ramdisk).is_ok());
    }
}
