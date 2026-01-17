// Environment validation
// Checks that required tools and system configuration are available

use eyre::{Result, eyre};
use nix::unistd::geteuid;

/// Check that we are running as root (UID 0)
///
/// Required for:
/// - dmsetup operations (creating/removing dm-era devices)
/// - mount/umount operations
/// - Block device access
pub fn require_root() -> Result<()> {
    if !geteuid().is_root() {
        return Err(eyre!(
            "This command requires root privileges.\n\
             Please run with sudo or as root."
        ));
    }
    Ok(())
}
