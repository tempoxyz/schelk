// Low-level I/O operations for block devices
// Handles reading, writing, copying, and zeroing of block devices

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use eyre::{Result, WrapErr, eyre};

/// Size of superblock to read for hashing (4KB)
const SUPERBLOCK_SIZE: usize = 4096;

/// Buffer size for copying/zeroing (1MB)
const BUFFER_SIZE: usize = 1024 * 1024;

/// Number of parallel I/O threads for block copying
// TODO: make configurable
const COPY_THREADS: usize = 25;

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

    // Seek to end to get size
    let size = file
        .metadata()
        .ok()
        .and_then(|m| if m.len() > 0 { Some(m.len()) } else { None })
        .or_else(|| {
            // For block devices, metadata().len() returns 0
            // Use seek to end instead
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

/// Full block-level copy from source to destination
/// Shows progress via callback and returns bytes copied
pub fn full_copy<F>(src: &Path, dst: &Path, mut progress: F) -> Result<u64>
where
    F: FnMut(u64, u64),
{
    let size = get_size(src)?;

    let mut src_file =
        File::open(src).wrap_err_with(|| format!("Cannot open source {}", src.display()))?;

    let mut dst_file = OpenOptions::new()
        .write(true)
        .open(dst)
        .wrap_err_with(|| format!("Cannot open destination {}", dst.display()))?;

    let mut buf = vec![0u8; BUFFER_SIZE];
    let mut copied: u64 = 0;

    loop {
        let bytes_read = src_file
            .read(&mut buf)
            .wrap_err("Failed to read from source")?;

        if bytes_read == 0 {
            break;
        }

        dst_file
            .write_all(&buf[..bytes_read])
            .wrap_err("Failed to write to destination")?;

        copied += bytes_read as u64;
        progress(copied, size);
    }

    // Ensure data is flushed to disk
    dst_file.sync_all().wrap_err("Failed to sync destination")?;

    Ok(copied)
}

/// Copy specific block ranges from source to destination using parallel I/O
/// Used for incremental recovery
pub fn copy_blocks<F>(
    src: &Path,
    dst: &Path,
    blocks: &[BlockRange],
    granularity: u64,
    mut progress: F,
) -> Result<u64>
where
    F: FnMut(u64, u64),
{
    let total_blocks: u64 = blocks.iter().map(|r| r.len).sum();
    let total_bytes = total_blocks * granularity;

    if blocks.is_empty() {
        return Ok(0);
    }

    // Flatten block ranges into individual block offsets for work distribution
    let mut work_items: Vec<u64> = Vec::with_capacity(total_blocks as usize);
    for range in blocks {
        for i in 0..range.len {
            work_items.push((range.start + i) * granularity);
        }
    }

    let src_path = src.to_path_buf();
    let dst_path = dst.to_path_buf();
    let copied = Arc::new(AtomicU64::new(0));
    let work_items = Arc::new(work_items);
    let error: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));

    // Spawn worker threads
    let handles: Vec<_> = (0..COPY_THREADS)
        .map(|thread_id| {
            let src_path = src_path.clone();
            let dst_path = dst_path.clone();
            let work_items = Arc::clone(&work_items);
            let copied = Arc::clone(&copied);
            let error = Arc::clone(&error);

            thread::spawn(move || {
                copy_blocks_worker(
                    thread_id,
                    &src_path,
                    &dst_path,
                    &work_items,
                    granularity,
                    &copied,
                    &error,
                )
            })
        })
        .collect();

    // Poll progress while threads are running
    loop {
        let current = copied.load(Ordering::Relaxed);
        progress(current, total_bytes);

        if current >= total_bytes {
            break;
        }

        // Check if any error occurred
        if error.lock().unwrap().is_some() {
            break;
        }

        thread::sleep(std::time::Duration::from_millis(50));
    }

    // Wait for all threads to complete
    for handle in handles {
        let _ = handle.join();
    }

    // Check for errors
    if let Some(err_msg) = error.lock().unwrap().take() {
        return Err(eyre!("{}", err_msg));
    }

    // Sync destination
    let dst_file = OpenOptions::new()
        .write(true)
        .open(dst)
        .wrap_err_with(|| format!("Cannot open destination {}", dst.display()))?;
    dst_file.sync_all().wrap_err("Failed to sync destination")?;

    Ok(total_bytes)
}

/// Worker thread for parallel block copying
fn copy_blocks_worker(
    thread_id: usize,
    src_path: &PathBuf,
    dst_path: &PathBuf,
    work_items: &[u64],
    granularity: u64,
    copied: &AtomicU64,
    error: &std::sync::Mutex<Option<String>>,
) {
    // Each thread opens its own file handles
    let src_file = match File::open(src_path) {
        Ok(f) => f,
        Err(e) => {
            let mut err = error.lock().unwrap();
            if err.is_none() {
                *err = Some(format!("Thread {}: cannot open source: {}", thread_id, e));
            }
            return;
        }
    };

    let dst_file = match OpenOptions::new().write(true).open(dst_path) {
        Ok(f) => f,
        Err(e) => {
            let mut err = error.lock().unwrap();
            if err.is_none() {
                *err = Some(format!(
                    "Thread {}: cannot open destination: {}",
                    thread_id, e
                ));
            }
            return;
        }
    };

    let mut buf = vec![0u8; granularity as usize];

    // Process work items round-robin style
    for (i, &offset) in work_items.iter().enumerate() {
        if i % COPY_THREADS != thread_id {
            continue;
        }

        // Check if another thread encountered an error
        if error.lock().unwrap().is_some() {
            return;
        }

        // Use pread/pwrite for concurrent access without seeking
        if let Err(e) = src_file.read_exact_at(&mut buf, offset) {
            let mut err = error.lock().unwrap();
            if err.is_none() {
                *err = Some(format!(
                    "Thread {}: read error at offset {}: {}",
                    thread_id, offset, e
                ));
            }
            return;
        }

        if let Err(e) = dst_file.write_all_at(&buf, offset) {
            let mut err = error.lock().unwrap();
            if err.is_none() {
                *err = Some(format!(
                    "Thread {}: write error at offset {}: {}",
                    thread_id, offset, e
                ));
            }
            return;
        }

        copied.fetch_add(granularity, Ordering::Relaxed);
    }
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
