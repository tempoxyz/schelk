//! A bunch of common error messages.

pub use eyre::eyre;

// Helper functions to create common errors with consistent messages

pub fn not_initialized() -> eyre::Report {
    eyre!("schelk has not been initialized. Run 'schelk init-new' or 'schelk init-from' first.")
}

pub fn volume_mismatch() -> eyre::Report {
    eyre!(
        "Volume superblock does not match expected state. The volume may have been modified externally."
    )
}

pub fn already_mounted() -> eyre::Report {
    eyre!("Volume is already mounted.")
}
