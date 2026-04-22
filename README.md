# schelk

schelk restores a block device to a known baseline quickly. It is designed for benchmarking
systems with large on-disk state - databases, or blockchain execution clients like
[reth](https://github.com/paradigmxyz/reth). For such systems, rebuilding the baseline between
runs is slow, and snapshot layers distort the measurements we want to take.

> [!TIP]
> If you want Codex, Claude, or another coding agent to set up or operate schelk for you, start
> it with [`docs/SKILL.md`](docs/SKILL.md) or point it directly at
> <https://github.com/tempoxyz/schelk/blob/master/docs/SKILL.md>. That file tells the agent how to
> install schelk, validate prerequisites, initialize volumes, and run the workflow safely.

## Why

A good benchmarking loop has two requirements:

1. **Fast rollback.** Each run mutates the state on disk, so the baseline must be restored
   before the next run. If the rollback takes hours, the iteration loop is dead.
2. **Faithful measurement.** The numbers must reflect the workload, not the rollback
   machinery. Overhead matters, but variance matters more. If a benchmark varies by 10%
   between runs, an improvement of 5% is not visible.

The two requirements are in tension. Any mechanism that makes rollback fast has to remember
something about the pre-run state, and every such mechanism leaves a trace in the read or
write path. The common approaches trade one requirement against the other:

- **Full copy of the volume.** The benchmark runs against a plain filesystem, with no
  tracking in the hot path - faithful. But on a multi-TB dataset one copy takes hours, so the
  iteration loop is impractical.
- **Copy-on-write filesystems** (ZFS, btrfs). Rollback is fast. But every write passes
  through the CoW layer, and successive runs fragment the dataset differently. Both the
  overhead and the layout drift enter the numbers.
- **LVM thin with the overlay on a separate disk.** Rollback is fast. But reads come from
  one disk and writes go to another. This is not the IO topology of production, so we are
  benchmarking a different system.

schelk tries to satisfy both. The observation is simple: a typical benchmark writes only a
small fraction of the volume. If we know *exactly which blocks* were written, we can restore
the scratch volume by copying only those blocks from a pristine **virgin** volume. Rollback
takes seconds in most cases, rather than hours. During the benchmark itself, the workload
runs against a plain ext4 filesystem on a real NVMe device, with no overlay and no write
redirection.

## How it works

schelk operates on two equal-size block devices: a **virgin** volume that holds the pristine
baseline, and a **scratch** volume that is mounted and used by the benchmark. At `init` time,
schelk makes scratch byte-identical to virgin. This is done either by creating a fresh ext4 on
both volumes, or by copying an existing virgin over. After initialization, both volumes belong
to schelk and should not be touched directly.

When `mount` is run, schelk places a `dm-era` device-mapper target on top of scratch. dm-era
records every written block into metadata that lives on the ramdisk. The benchmark runs
against the mounted filesystem as normal. dm-era does not redirect reads or writes; it only
records which blocks were written.

When `recover` is run, schelk unmounts the filesystem, asks dm-era for the list of blocks
that were written since the last baseline, and copies exactly those blocks from virgin back to
scratch. Recovery time is proportional to the number of written blocks, not to the size of the
volume, so it does not matter how long the benchmark ran.

A separate `promote` operation does the reverse: it copies the written blocks from scratch
onto virgin, so that the current state becomes the new baseline. This is useful after a schema
migration, or after a snapshot load that should persist across future runs.

## Pre-requisites

### Hardware

- Two block devices of equal size, one for **virgin** and one for **scratch**. Each must be
  large enough to hold the dataset.[^1]
- A ramdisk for dm-era metadata. The exact size depends on the internals of dm-era rather
  than on the workload, so a precise formula is hard to give. As a rule of thumb, 4 GiB is
  sufficient for a 1.7 TiB drive at 4 KiB granularity.

[^1]: 🦄 Future Feature is to lift the equal-size restriction.

### Software

- A reasonably modern Linux kernel with device-mapper and the `dm-era` target.
- A reasonably recent Rust toolchain.
- `mkfs.ext4` from e2fsprogs (required for `init-new`). Usually pre-installed; otherwise
  `apt install e2fsprogs`.
- `era_invalidate` from
  [thin-provisioning-tools](https://github.com/device-mapper-utils/thin-provisioning-tools).
  The distribution package works, but versions older than 1.0 are very slow. For serious use,
  build from source.[^2]
- `dmsetup` (shipped with most distributions).

[^2]: The following command tends to work:
  ```git clone https://github.com/jthornber/thin-provisioning-tools /tmp/tpt && cargo build --release --manifest-path /tmp/tpt/Cargo.toml && sudo cp /tmp/tpt/target/release/pdata_tools /usr/local/bin/ && sudo ln -sf /usr/local/bin/pdata_tools /usr/local/bin/era_invalidate```

## Usage

> [!WARNING]
> schelk requires sudo and will overwrite the volumes given to it.

### Install

There are no binary releases yet. Clone the repository and install from source:

```
cargo install --path .
```

### Set up a ramdisk

```
# 4 GiB ramdisk (rd_size is in KB, so 4 GiB = 4*1024*1024 = 4194304 KB)
sudo modprobe brd rd_size=4194304
```

### Initialize

There are two initialization paths:

**`init-new`** - create fresh ext4 filesystems on both volumes from scratch. All existing data
on both volumes is lost.

```
sudo schelk init-new \
    --virgin /dev/nvme1n1 \
    --scratch /dev/nvme2n1 \
    --ramdisk /dev/ram0 \
    --mount-point /schelk
```

**`init-from`** - adopt an existing, pre-populated virgin volume, for example one that already
has a database snapshot loaded. The scratch volume is overwritten with a copy of virgin.

```
sudo schelk init-from \
    --virgin /dev/nvme1n1 \
    --scratch /dev/nvme2n1 \
    --ramdisk /dev/ram0 \
    --mount-point /schelk \
    --fstype ext4
```

If both volumes are already prepared identically, `--no-copy` skips the full copy:

```
sudo schelk init-from ... --no-copy
```

### Run a benchmark

```
sudo schelk mount       # mount scratch with dm-era tracking
./bench.sh              # run the benchmark
sudo schelk recover     # restore scratch to virgin
```

### Promote scratch to a new baseline

Use this after a one-time state change that should be kept across future runs, such as a
schema migration or a snapshot load:

```
sudo schelk promote
```

### Other commands

- `schelk full-recover` - copy the entire virgin volume to scratch. Used when the incremental
  recovery path is no longer valid, for example after a host reboot.
- `schelk status` - report the current state (initialized, mounted, and so on).

Note that both volumes must not be used outside of schelk. Mounting them directly will
invalidate the incremental recovery path and force a full copy.

## When not to use schelk

schelk is not a silver bullet. It is brittle and has rough edges, and its hardware cost is
not trivial: two block devices large enough to hold the dataset, plus enough DRAM to back a
ramdisk. For many workloads, a CoW filesystem like ZFS or btrfs is a better fit — the
overhead is real, but easier to accept than the cost and operational effort of schelk.

Prefer a different approach when:

- Measurements can tolerate some overhead or distortion introduced by the rollback mechanism.
- The workload writes most of the dataset, so incremental recovery is not faster than a full
  copy.
- The hardware budget does not cover two dedicated volumes and enough DRAM for the ramdisk.

## Limitations

- **NVMe internal state is not restored.** Overwriting logical blocks does not reset FTL
  mappings, wear levelling, on-controller caches, or garbage collection state. Some
  run-to-run variance will always remain. Standard mitigations - drive pre-conditioning,
  long warmups, steady-state measurement windows - still apply; schelk does not replace them.
- **Ramdisk metadata does not survive a reboot.** If the host reboots or loses power while a
  dm-era device is active, incremental recovery is no longer possible. In that case, run
  `full-recover`.
- **Volumes are dedicated.** For the duration of a schelk session, both volumes must not be
  used by anything else. Mounting them or writing to them outside schelk invalidates the
  incremental recovery path.
- **Volumes must be of equal size.** This restriction may be lifted in the future.

## FAQ

- **Why not LVM snapshots, ZFS, or btrfs?** They add variance in the hot path of the
  benchmark. See [Why](#why).
- **Why not LVM thin with a read-only base and a writable overlay?** The same reason, and
  additionally the split read/write IO topology does not reflect production.
- **Why not a userspace filesystem via libfuse?** libfuse is single-threaded, which is a
  bottleneck for parallel benchmark workloads. The io_uring support in libfuse may eventually
  lift this, but at the time of writing it was still immature. A libfuse-based solution would
  also sit on top of a real filesystem, so restoring the baseline would mean writing back
  through that filesystem. The state of the underlying filesystem would drift between runs -
  the same problem as with CoW filesystems.
- **Why `dm-era` specifically?** It is the lightest tracking layer in mainline Linux: it does
  not move data, cache anything, or redirect IO. Its only job is to mark blocks with an
  "era" number when they are written. A bitmap based system would be much more efficient.
- **Why a ramdisk for metadata?** Two reasons. First, keeping metadata writes off the drive
  under test avoids contention with the benchmark. Second, the metadata is cheap to recreate,
  and a reboot invalidates the incremental recovery path regardless.
- **What is a typical recovery time?** Recovery time is proportional to the number of bytes
  written during the run, not to the volume size. A benchmark that writes a few GiB on a
  multi-TB volume typically recovers in seconds.
