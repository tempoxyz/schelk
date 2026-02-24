// RAM disk validation
// dm-era metadata sizing calculations

use std::path::Path;

use eyre::{Result, eyre};

use crate::io;

/// Fixed overhead for superblock, space map, and transaction buffers (bytes)
const FIXED_OVERHEAD: u64 = 1024 * 1024;

// =============================================================================
// dm-era Metadata Sizing
// =============================================================================
//
// dm-era stores block modification tracking data on a metadata device. The
// metadata consists of:
//
// 1. SUPERBLOCK (1 block = 4KB)
//    Fixed header with magic, version, UUIDs, and pointers to other structures.
//
// 2. ERA ARRAY
//    One u32 per tracked block storing the era number when that block was last
//    written. Packed into 4KB metadata blocks with a 24-byte header, giving
//    1018 entries per block: (4096 - 24) / 4 = 1018.
//
//    For a volume with N blocks: ceil(N / 1018) metadata blocks.
//
// 3. B-TREE INDEX
//    The era array blocks are indexed by a B-tree. Internal nodes add ~4%
//    overhead based on empirical testing.
//
// 4. SPACE MAP
//    Tracks which metadata blocks are in use. Small fixed overhead.
//
// 5. WRITESET (in RAM only, not on metadata device)
//    Two bitsets of N bits each for tracking current era writes. These consume
//    kernel memory, not metadata device space.
//
// EMPIRICAL FORMULA (derived from testing with various volume sizes):
//
//   min_metadata_blocks = ceil(nr_blocks / 1018) * 1.04 + 10
//   min_metadata_bytes  = min_metadata_blocks * 4096
//
// Where:
//   - nr_blocks = volume_size / granularity
//   - 1018 = u32 entries per 4K metadata block
//   - 1.04 = B-tree overhead factor (~4%)
//   - 10 = fixed blocks for superblock, space map, transaction buffers
//
// The ideal size adds 15% margin plus 1MB fixed overhead for safety.
//
// Reference: Linux kernel drivers/md/dm-era-target.c
//            thin-provisioning-tools (era_invalidate, era_dump)
// =============================================================================

/// Calculate minimum and ideal RAM disk sizes for dm-era metadata.
///
/// Returns (min_size, ideal_size) in bytes.
/// - min_size: absolute minimum (will fail below this + 10% buffer)
/// - ideal_size: recommended size with comfortable margin
fn calculate_required_sizes(volume_size: u64, granularity: u64) -> (u64, u64) {
    let nr_blocks = volume_size / granularity;

    // Era array blocks: ceil(nr_blocks / 1018) where 1018 = entries per 4K block
    // Each 4K metadata block holds (4096 - 24 header) / 4 bytes = 1018 u32 values
    let era_array_blocks = (nr_blocks + 1017) / 1018;

    // B-tree overhead: ~4% of era array blocks for internal nodes
    let btree_overhead_blocks = (era_array_blocks * 4) / 100;

    // Fixed overhead: ~10 blocks for superblock, space map root, transaction buffers
    let fixed_blocks = 10;

    // Minimum: era array + btree + fixed, in bytes
    let min_blocks = era_array_blocks + btree_overhead_blocks + fixed_blocks;
    let min_size = min_blocks * 4096;

    // Ideal: minimum + 15% margin + 1MB fixed overhead
    let ideal_size = (min_size * 115) / 100 + FIXED_OVERHEAD;

    (min_size, ideal_size)
}

/// Validate that RAM disk is large enough for the given volume and granularity.
///
/// Prints a warning if size is adequate but below ideal.
/// Returns error if size is below minimum.
pub fn validate_size(path: &Path, volume_size: u64, granularity: u64) -> Result<()> {
    let actual_size = io::get_size(path)?;
    let (min_size, ideal_size) = calculate_required_sizes(volume_size, granularity);

    // Minimum with 10% safety buffer
    let min_with_buffer = (min_size * 110) / 100;

    if actual_size < min_with_buffer {
        return Err({
            let required = min_with_buffer;
            let actual = actual_size;
            eyre!(
                "RAM disk too small: {} bytes required, {} bytes available.",
                required,
                actual
            )
        });
    }

    if actual_size < ideal_size {
        eprintln!(
            "Warning: RAM disk size ({} bytes) is less than ideal ({} bytes). \
             This may work but leaves little headroom.",
            actual_size, ideal_size
        );
    }

    Ok(())
}
