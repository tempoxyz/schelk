# schelk

Read the README.md and other documents. This is linux only.

## Mindset
 
We should be really friendly to the user and try to minimize the opportunity to misuse this tool.
The risk is that the user will get the wrong data from benchmarking or mess up their volumes and
that can lead to a very costly full recovery.

There are many errors modes in this tool. We need to defend against that. The user experience should be slick and foolproof.

1. probably we always want to tear down the whole dm-era thing after we have recovered.
2. during set up we should check the environment and if it makes sense. We should check if the dmsetup installed, if the version is correct. If linux version is correct.
3. the app should be stateful. you should configure it once. Specify what disks to use. Etc.
4. the app should be able to checksum, at least partially, the state of the virgin volume.  In case it identifies mismatch (for example, because the original volume was modified) then it should notify the user giving options to rectify the issue.
5. the app should prepare for crash of the system or the app itself.
6. mount should check whether there is already mount.
7. By default the commands are interactive and destructive operations must be confirmed either via
   the CLI flag -y or a press.

## Commands

1. init-new
2. init-from
3. full-recover
4. mount
5. recover
6. promote
7. status

### `init-new`

Creates fresh ext4 filesystems on both volumes from scratch. This is a **destructive** operation —
all existing data on both volumes will be lost.

If the app state already exists, it offers if it should reinitialize.

Parameters:

- the virgin volume path (device or partition)
- the scratch volume path (device or partition)
- the ram disk path.
- mount point and the mount options.
- granularity. Defaults to 4k.

Steps:

1. Checks (see below).
2. Creates a fresh ext4 filesystem on the virgin volume (4K block size, journaling enabled, 
   label "schelk", zeroed UUID for determinism).
3. Copies the virgin volume to the scratch volume (full block-level copy) so both are 
   byte-identical.
4. Computes the superblock hash and saves app state.

Checks:

1. Simple volume check (see below).
2. The RAM disk is sufficiently sized for the given drive and granularity.
3. The size of the scratch volume and the virgin volume matches.

Must be confirmed either by `-y` or an interactive prompt.

### `init-from`

Adopts an existing, pre-populated virgin volume. Use this when you have already prepared the virgin
volume with data (e.g., loaded a database snapshot, run schema migrations, etc.) and want schelk to
take control of it. This is a **destructive** operation — the scratch volume will be overwritten
with a full copy of the virgin.

If the app state already exists, it offers if it should reinitialize.

Parameters:

- the virgin volume path (device or partition)
- the scratch volume path (device or partition)
- the ram disk path.
- mount point and the mount options.
- granularity. Defaults to 4k.

Steps:

1. Checks (see below).
2. Copies the virgin volume to the scratch volume (full block-level copy).
3. Computes the superblock hash and saves app state.

Checks:

1. Simple volume check (see below).
2. The RAM disk is sufficiently sized for the given drive and granularity.
3. The size of the scratch volume and the virgin volume matches.

Must be confirmed either by `-y` or an interactive prompt.

> 🦄 Future Feature: interactive wizard. Help the user setting up in an interactive way.

Both commands should let the user know that schelk now expects to control both volumes. Mounting 
them outside of schelk may ruin them.

### `full-recover`

Performs copy from the virgin to the scratch. This is very costly and should be ideally performed 
only once. Must be confirmed either by -y.

### `mount`

Checks:

1. Checks that the RAM disk is adequately sized for the job.
1. Spot checks state of volumes and verifies that it is the same as the expected per the state of the 
app.

Zeroes the ramdisk and initializes dm-era for the scratch volume.

```
dmsetup create bench_era ... era /dev/ram0 /dev/nvme_ <granularity>
dmsetup message bench_era 0 checkpoint
```

Mounts the scratch volume at the specified location. 
It should be fool proof, in that it should not allow mount after amount.

### `recover`
 
Pre-checks:
 
- make sure that the mount previously happened. This should be performed by reading the app state
  first.
- `dmsetup`, `era_invalidate` is available in the PATH.

Unmount the filesystem first. This prevents further modifications and also would give a safeguard
from removing a filesystem while there is still some activity. That also flushes the filesystem.

```
umount /schelk
```
 
Take dm-era snapshot, invalidate. 

```
# Create a clone of metadata for userspace reading.
dmsetup message bench_era 0 take_metadata_snap
# Collect all the changed blocks into a file.
era_invalidate --metadata-snapshot --written-since "$BASE" /dev/ram0 > changed.xml
# Drop the metadata snapshot.
dmsetup message bench_era 0 drop_metadata_snap
```

At this point we obtained `changed.xml` containing all the blocks to restore.

> 🦄 In the future, we could save `changed.xml` into the appstate so that if recovery fails for
  some reason (cancelled by the user, app crashes, system crashes) the user does not have to perform
  full migration.

We should tear down the device mapper.

```
dmsetup remove bench_era
```

And then perform copy of the blocks from the virgin to scratch according to `changed.xml`. The 
progress of copying must be displayed. After finish, it should print a report.

Once it succeeded we should update the app state and remove `changed.xml`.

### `promote`

Promotes the scratch volume to become the new virgin. This is the reverse of `recover`: instead of
restoring scratch from virgin, it updates virgin from scratch. Useful when the benchmark run has
produced a new desired baseline state (e.g., after a schema migration or data load that should 
become the new starting point for future runs).

Pre-checks:

- App state must exist (initialized).
- The volume must be mounted (`is_mounted = true`), so that dm-era has tracked the changes.
- `dmsetup`, `era_invalidate` must be available in PATH.
- dm-era device must exist.

Must be confirmed either by `-y` or an interactive prompt, since this is a destructive operation 
that permanently modifies the virgin volume.

Steps:

1. **Unmount the filesystem.** Same as `recover` — flushes writes and prevents further modifications.

2. **Collect changed blocks via dm-era.** Same metadata snapshot and `era_invalidate` flow as 
   `recover`:
   ```
   dmsetup message bench_era 0 take_metadata_snap
   era_invalidate --metadata-snapshot --written-since <base_era> /dev/ram0 > changed.xml
   dmsetup message bench_era 0 drop_metadata_snap
   ```

3. **Tear down dm-era.**
   ```
   dmsetup remove bench_era
   ```

4. **Copy changed blocks from scratch to virgin.** This is the key difference from `recover`: the
   copy direction is reversed. Uses the same parallel block copy mechanism but with scratch as 
   source and virgin as destination. Progress must be displayed.

5. **Update app state:**
   - Set `is_mounted = false`.
   - Clear `current_era`.
   - Recompute and store the new `virgin_superblock_hash` (since the virgin has changed).
   - Save atomically.

### `status`

Reports the current status.

## App State

The app state lives in `/var/lib/schelk/state.json`. This is a system-wide location because schelk
requires root privileges to operate (dm-era, mount, block device access). Using a fixed path avoids
confusion when running with `sudo` (which would otherwise use root's home directory for XDG paths).

Every update should be performed robustly: atomic updates (write to temp file, fsync, rename), 
`fsync` the directory, etc.

## Volume Checks

The simplest way to check that the too volumes are equal are checking their super blocks.

## Other Notes

- Use `async`.
- The copying should be performed in parallel. The jobs should use workstealing approach by batches.
- `eyre` for error reporting.
- use `rustfmt` to format the project code.

## Testing

These are the acceptable testing parameters:

```
"virgin": "/dev/nvme1n1p1",
"scratch": "/dev/nvme1n1p2",
"ramdisk": "/dev/ram0",
"mount_point": "/schelk",
"mount_options": null,
```
