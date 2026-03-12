use std::alloc::{self, Layout};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::io::{AsRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use eyre::{Result, WrapErr, eyre};
use io_uring::types::Fixed;
use nix::fcntl::OFlag;
use nix::sys::stat::Mode;

const RING_COUNT: usize = 4;

const FULL_COPY_CHUNK_SIZE: usize = 2 * 1024 * 1024;
const FULL_COPY_SQ_DEPTH: u32 = 256;
const FULL_COPY_SLOTS_PER_RING: usize = 64;

const BLOCKS_COPY_TARGET_CHUNK: u64 = 256 * 1024;
const BLOCKS_COPY_MAX_CHUNK: u64 = 1024 * 1024;
const BLOCKS_COPY_SQ_DEPTH: u32 = 512;
const BLOCKS_COPY_SLOTS_PER_RING: usize = 128;

const SUBMIT_BATCH_SIZE: usize = 32;
const SUPERBLOCK_SIZE: usize = 4096;
const BUFFER_SIZE: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct BlockRange {
    pub start: u64,
    pub len: u64,
}

struct CopyChunk {
    offset: u64,
    len: u64,
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Free,
    Reading,
    Writing,
}

struct Slot {
    offset: u64,
    len: u64,
    phase: Phase,
}

fn encode_user_data(slot_id: usize, phase: Phase) -> u64 {
    slot_id as u64 | ((phase as u64) << 32)
}

fn decode_user_data(ud: u64) -> (usize, Phase) {
    let slot_id = (ud & 0xFFFF_FFFF) as usize;
    let phase = match ud >> 32 {
        0 => Phase::Free,
        1 => Phase::Reading,
        2 => Phase::Writing,
        _ => Phase::Free,
    };
    (slot_id, phase)
}

struct AlignedBufferPool {
    base: std::ptr::NonNull<u8>,
    slot_size: usize,
    slot_count: usize,
    layout: Layout,
}

impl AlignedBufferPool {
    fn new(slot_size: usize, slot_count: usize) -> Result<Self> {
        let total = slot_size
            .checked_mul(slot_count)
            .ok_or_else(|| eyre!("buffer pool size overflow"))?;
        let layout = Layout::from_size_align(total, 4096)
            .map_err(|e| eyre!("invalid buffer layout: {}", e))?;
        let base = std::ptr::NonNull::new(unsafe { alloc::alloc(layout) })
            .ok_or_else(|| eyre!("failed to allocate aligned buffer pool"))?;
        unsafe {
            std::ptr::write_bytes(base.as_ptr(), 0, total);
        }
        Ok(Self {
            base,
            slot_size,
            slot_count,
            layout,
        })
    }

    fn slot_mut_ptr(&self, idx: usize) -> *mut u8 {
        assert!(idx < self.slot_count);
        unsafe { self.base.as_ptr().add(idx * self.slot_size) }
    }

    fn slot_ptr(&self, idx: usize) -> *const u8 {
        self.slot_mut_ptr(idx) as *const u8
    }
}

unsafe impl Send for AlignedBufferPool {}

impl Drop for AlignedBufferPool {
    fn drop(&mut self) {
        unsafe {
            alloc::dealloc(self.base.as_ptr(), self.layout);
        }
    }
}

pub fn get_size(path: &Path) -> Result<u64> {
    let file = File::open(path).wrap_err_with(|| format!("Cannot open {}", path.display()))?;

    let size = file
        .metadata()
        .ok()
        .and_then(|m| if m.len() > 0 { Some(m.len()) } else { None })
        .or_else(|| {
            let mut f = file;
            f.seek(SeekFrom::End(0)).ok()
        })
        .ok_or_else(|| eyre!("Cannot determine size of {}", path.display()))?;

    Ok(size)
}

pub fn read_superblock(path: &Path) -> Result<Vec<u8>> {
    let mut file = File::open(path).wrap_err_with(|| format!("Cannot open {}", path.display()))?;

    let mut buf = vec![0u8; SUPERBLOCK_SIZE];
    file.read_exact(&mut buf)
        .wrap_err_with(|| format!("Cannot read superblock from {}", path.display()))?;

    Ok(buf)
}

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

fn sequential_chunks(size: u64, chunk_size: u64) -> Vec<CopyChunk> {
    let aligned_size = (size + 4095) & !4095;
    let mut chunks = Vec::new();
    let mut offset = 0u64;
    while offset < aligned_size {
        let len = std::cmp::min(chunk_size, aligned_size - offset);
        chunks.push(CopyChunk { offset, len });
        offset += len;
    }
    chunks
}

fn prepare_chunks(blocks: &[BlockRange], granularity: u64) -> Vec<CopyChunk> {
    if blocks.is_empty() {
        return Vec::new();
    }

    let mut sorted: Vec<BlockRange> = blocks.to_vec();
    sorted.sort_by_key(|r| r.start);

    let mut merged: Vec<BlockRange> = Vec::new();
    for r in &sorted {
        if let Some(last) = merged.last_mut()
            && last.start + last.len == r.start
        {
            last.len += r.len;
            continue;
        }
        merged.push(r.clone());
    }

    let mut chunks = Vec::new();
    for range in &merged {
        let byte_offset = range.start * granularity;
        let byte_len = range.len * granularity;
        let mut off = 0u64;
        while off < byte_len {
            let remaining = byte_len - off;
            let target = BLOCKS_COPY_TARGET_CHUNK;
            let raw_len = std::cmp::min(remaining, std::cmp::max(target, granularity));
            let capped = std::cmp::min(raw_len, BLOCKS_COPY_MAX_CHUNK);
            let aligned = (capped / granularity) * granularity;
            let chunk_len = if aligned == 0 { granularity } else { aligned };
            let chunk_len = std::cmp::min(chunk_len, remaining);
            chunks.push(CopyChunk {
                offset: byte_offset + off,
                len: chunk_len,
            });
            off += chunk_len;
        }
    }

    chunks
}

#[allow(clippy::too_many_arguments)]
fn run_ring(
    src_fd: RawFd,
    dst_fd: RawFd,
    chunks: &[CopyChunk],
    sq_depth: u32,
    slots_per_ring: usize,
    slot_size: usize,
    copied: &AtomicU64,
) -> Result<()> {
    if chunks.is_empty() {
        return Ok(());
    }

    let mut ring = io_uring::IoUring::new(sq_depth).wrap_err("failed to create io_uring")?;

    ring.submitter()
        .register_files(&[src_fd, dst_fd])
        .wrap_err("failed to register files")?;

    let pool = AlignedBufferPool::new(slot_size, slots_per_ring)?;

    let mut slots: Vec<Slot> = (0..slots_per_ring)
        .map(|_| Slot {
            offset: 0,
            len: 0,
            phase: Phase::Free,
        })
        .collect();

    let mut chunk_idx = 0usize;
    let mut in_flight = 0usize;
    let mut pending_submits = 0usize;
    let mut first_error: Option<eyre::Report> = None;

    loop {
        if first_error.is_none() {
            let mut submitted_this_iter = false;

            #[allow(clippy::needless_range_loop)]
            for slot_id in 0..slots_per_ring {
                if chunk_idx >= chunks.len() {
                    break;
                }
                if slots[slot_id].phase != Phase::Free {
                    continue;
                }

                let chunk = &chunks[chunk_idx];
                chunk_idx += 1;

                let buf = pool.slot_mut_ptr(slot_id);
                let read_len = std::cmp::min(chunk.len as usize, slot_size);

                let sqe = io_uring::opcode::Read::new(Fixed(0), buf, read_len as u32)
                    .offset(chunk.offset)
                    .build()
                    .user_data(encode_user_data(slot_id, Phase::Reading));

                slots[slot_id].phase = Phase::Reading;
                slots[slot_id].offset = chunk.offset;
                slots[slot_id].len = read_len as u64;

                unsafe {
                    ring.submission()
                        .push(&sqe)
                        .map_err(|_| eyre!("submission queue full"))?;
                }

                in_flight += 1;
                pending_submits += 1;
                submitted_this_iter = true;

                if pending_submits >= SUBMIT_BATCH_SIZE {
                    ring.submitter().submit().wrap_err("submit failed")?;
                    pending_submits = 0;
                }
            }

            if pending_submits > 0 && submitted_this_iter {
                ring.submitter().submit().wrap_err("submit failed")?;
                pending_submits = 0;
            }
        }

        if in_flight == 0 {
            break;
        }

        if pending_submits > 0 {
            ring.submitter().submit().wrap_err("submit failed")?;
            pending_submits = 0;
        }

        ring.submitter()
            .submit_and_wait(1)
            .wrap_err("submit_and_wait failed")?;

        let cqes: Vec<_> = ring.completion().collect();
        for cqe in &cqes {
            let (slot_id, phase) = decode_user_data(cqe.user_data());
            let result = cqe.result();

            if result < 0 {
                if first_error.is_none() {
                    let err = std::io::Error::from_raw_os_error(-result);
                    first_error = Some(eyre::Report::new(err).wrap_err(format!(
                        "io_uring operation failed at offset {}",
                        slots[slot_id].offset
                    )));
                }
                slots[slot_id].phase = Phase::Free;
                in_flight -= 1;
                continue;
            }

            match phase {
                Phase::Reading => {
                    let expected = slots[slot_id].len as i32;
                    if result != expected {
                        if first_error.is_none() {
                            first_error = Some(eyre!(
                                "short read at offset {}: expected {} bytes, got {}",
                                slots[slot_id].offset,
                                expected,
                                result
                            ));
                        }
                        slots[slot_id].phase = Phase::Free;
                        in_flight -= 1;
                        continue;
                    }

                    let buf = pool.slot_ptr(slot_id);
                    let write_len = slots[slot_id].len as u32;
                    let offset = slots[slot_id].offset;

                    let sqe = io_uring::opcode::Write::new(Fixed(1), buf, write_len)
                        .offset(offset)
                        .build()
                        .user_data(encode_user_data(slot_id, Phase::Writing));

                    slots[slot_id].phase = Phase::Writing;

                    unsafe {
                        ring.submission()
                            .push(&sqe)
                            .map_err(|_| eyre!("submission queue full"))?;
                    }

                    pending_submits += 1;
                    if pending_submits >= SUBMIT_BATCH_SIZE {
                        ring.submitter().submit().wrap_err("submit failed")?;
                        pending_submits = 0;
                    }
                }
                Phase::Writing => {
                    let expected = slots[slot_id].len as i32;
                    if result != expected {
                        if first_error.is_none() {
                            first_error = Some(eyre!(
                                "short write at offset {}: expected {} bytes, got {}",
                                slots[slot_id].offset,
                                expected,
                                result
                            ));
                        }
                        slots[slot_id].phase = Phase::Free;
                        in_flight -= 1;
                        continue;
                    }

                    copied.fetch_add(slots[slot_id].len, Ordering::Relaxed);
                    slots[slot_id].phase = Phase::Free;
                    in_flight -= 1;
                }
                Phase::Free => {
                    in_flight -= 1;
                }
            }
        }
    }

    if let Some(err) = first_error {
        return Err(err);
    }

    Ok(())
}

fn open_direct(path: &Path, flags: OFlag) -> Result<OwnedFd> {
    nix::fcntl::open(path, flags | OFlag::O_DIRECT, Mode::empty())
        .map_err(eyre::Report::new)
        .wrap_err_with(|| format!("Cannot open {}", path.display()))
}

#[allow(clippy::too_many_arguments)]
fn run_copy(
    src: &Path,
    dst: &Path,
    chunks: Vec<CopyChunk>,
    sq_depth: u32,
    slots_per_ring: usize,
    slot_size: usize,
    progress: &mut dyn FnMut(u64, u64),
    total_bytes: u64,
) -> Result<()> {
    let src_owned = open_direct(src, OFlag::O_RDONLY)?;
    let dst_owned = open_direct(dst, OFlag::O_WRONLY)?;

    let src_fd = src_owned.as_raw_fd();
    let dst_fd = dst_owned.as_raw_fd();

    const DIRECT_IO_ALIGN: u64 = 4096;
    for chunk in &chunks {
        if chunk.offset % DIRECT_IO_ALIGN != 0 || chunk.len % DIRECT_IO_ALIGN != 0 {
            return Err(eyre!(
                "O_DIRECT requires {}-byte aligned offsets and lengths, \
                 got offset={} len={}",
                DIRECT_IO_ALIGN,
                chunk.offset,
                chunk.len
            ));
        }
    }

    let ring_count = chunks.len().clamp(1, RING_COUNT);

    let mut partitions: Vec<Vec<CopyChunk>> = (0..ring_count).map(|_| Vec::new()).collect();
    let chunks_per_ring = chunks.len().div_ceil(ring_count);
    for (i, chunk) in chunks.into_iter().enumerate() {
        let ring_idx = i / chunks_per_ring;
        let ring_idx = std::cmp::min(ring_idx, ring_count - 1);
        partitions[ring_idx].push(chunk);
    }

    let copied = Arc::new(AtomicU64::new(0));

    let handles: Vec<_> = partitions
        .into_iter()
        .map(|partition| {
            let copied = Arc::clone(&copied);
            thread::spawn(move || {
                run_ring(
                    src_fd,
                    dst_fd,
                    &partition,
                    sq_depth,
                    slots_per_ring,
                    slot_size,
                    &copied,
                )
            })
        })
        .collect();

    loop {
        let current = copied.load(Ordering::Relaxed);
        progress(current, total_bytes);

        if current >= total_bytes {
            break;
        }

        let all_done = handles.iter().all(|h| h.is_finished());
        if all_done {
            let current = copied.load(Ordering::Relaxed);
            progress(current, total_bytes);
            break;
        }

        thread::sleep(std::time::Duration::from_millis(50));
    }

    let mut first_error: Option<eyre::Report> = None;
    for handle in handles {
        match handle.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
            Err(_) => {
                if first_error.is_none() {
                    first_error = Some(eyre!("ring thread panicked"));
                }
            }
        }
    }

    if let Some(err) = first_error {
        return Err(err);
    }

    nix::unistd::fsync(&dst_owned).wrap_err("failed to fsync destination")?;

    Ok(())
}

pub fn full_copy<F>(src: &Path, dst: &Path, mut progress: F) -> Result<u64>
where
    F: FnMut(u64, u64),
{
    let size = get_size(src)?;
    let chunks = sequential_chunks(size, FULL_COPY_CHUNK_SIZE as u64);

    run_copy(
        src,
        dst,
        chunks,
        FULL_COPY_SQ_DEPTH,
        FULL_COPY_SLOTS_PER_RING,
        FULL_COPY_CHUNK_SIZE,
        &mut progress,
        size,
    )?;

    Ok(size)
}

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
    if blocks.is_empty() {
        return Ok(0);
    }

    let total_blocks: u64 = blocks.iter().map(|r| r.len).sum();
    let total_bytes = total_blocks * granularity;

    let chunks = prepare_chunks(blocks, granularity);

    let slot_size = std::cmp::max(BLOCKS_COPY_TARGET_CHUNK as usize, granularity as usize);
    let slot_size = slot_size.div_ceil(4096) * 4096;

    run_copy(
        src,
        dst,
        chunks,
        BLOCKS_COPY_SQ_DEPTH,
        BLOCKS_COPY_SLOTS_PER_RING,
        slot_size,
        &mut progress,
        total_bytes,
    )?;

    Ok(total_bytes)
}
