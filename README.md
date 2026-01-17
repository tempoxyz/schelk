# schelk

schelk is a tool for benchmarking databases quickly.

Assumptions that are made building it:

1. Benchmarking must be as faithful as possible.
2. Benchmarking is destructive.
3. The iteration cycle must be as short as possible.
4. You can foot the bill in terms of hardware.

This tool assumes you have a virgin volume and a scratch volume. Initially, the virgin volume is 
initialized with the data for tests and typically contains the initial snapshot. The scratch volume
is then written so that it is a perfect copy of the virgin volume. Then you run the benchmark
which messes with the scratch volume. Once you are done schelk allows you to rollback the state of 
the scratch volume back to the virgin volume quickly. It does this by tracking the exact updates
made to the scratch volume and then surgically patching them.

# Pre-requisites

- sufficiently new rust version. 
- `era_invalidate` from thin-provisioning-tools. While it can be installed via 
  `apt install thin-provisioning-tools` it is not recommended as it may be outdated. Anything 
  pre-1.0 is going to be slow. The newer version is available at [thin-provisioning-tools](https://github.com/device-mapper-utils/thin-provisioning-tools) repo.
- `dmsetup`. Should come with your distro most of the time.

# Usage

No binary releases at the moment. Clone repo and run `cargo install`.

```
cargo install --path .
```

This tool requires two disks of the equal size[^1] and a ramdisk. It's hard to give a precise 
formula, but for 1.7 TiB drive at 4 KiB granularity, 4 GiB ramdisk is sufficient.

[^1]: 🦄 Future Feature is to lift this restriction.

```
# Load with 4 GiB size (rd_size is in KB, so 4 GiB = 4*1024*1024 = 4194304 KB)
sudo modprobe brd rd_size=4194304
```

The disks should have the same content at initialization time, eg. via dd

```
sudo dd if=/dev/nvme1n1 of=/dev/nvme2n1 bs=256M status=progress conv=fsync
```

Once that's done you can run the initialization command.

```
# Note: This tool requires superuser privileges.
sudo schelk init \
    --virgin /dev/nvme1n1 \
    --scratch /dev/nvme2n1 \
    --ramdisk /dev/ram0 \
    --mount-point /schelk \
    --fstype ext4
```

and then run the experiments.

```
# mounts /schelk
sudo schelk mount
# messes with it
./bench.sh
# recovers it to the original state
sudo schelk recover
```

Note that both disks become untouchable. You must not mount them or 
otherwise you will have perform the full copy.

# FAQ and rationale
 
 - Why not use LVM snapshots or ZFS/btrfs? This tool is for benchmarks and we want to get faithful 
   results from it. Those introduce a measurable overhead.
 - Why not LVM thin of the read-only base for the golden and LV for writable overlay? The same reasons. In this case, all the reads are going to hit one device but writes are going to hit the other.
