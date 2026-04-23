# schelk

Read the README.md and other documents. This is linux only.

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
- dm-era device name. Defaults to `bench_era`. Override with `--dm-era-name` to run multiple 
  schelk instances in parallel (each must use a unique name, separate state files, and separate
  volumes/ramdisks).

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
- dm-era device name. Defaults to `bench_era`. Same semantics as `init-new`.

Steps:

1. Checks (see below).
2. Copies the virgin volume to the scratch volume (full block-level copy).
3. Computes the superblock hash and saves app state.

Checks:

1. Simple volume check (see below).
2. The RAM disk is sufficiently sized for the given drive and granularity.
3. The size of the scratch volume and the virgin volume matches.

Must be confirmed either by `-y` or an interactive prompt.

**Pro mode (`--no-copy`):** Skips the full block copy from virgin to scratch. The user asserts that
both volumes are already identical (e.g., they prepared them manually or intend to run `full-recover`
themselves). schelk will still validate the volumes and save app state, but will not touch the 
scratch volume. This is dangerous — if the volumes are not actually identical, subsequent recovers 
will produce corrupt results.

> 🦄 Future Feature: interactive wizard. Help the user setting up in an interactive way.

Both commands should let the user know that schelk now expects to control both volumes. Mounting 
them outside of schelk may ruin them.

### `full-recover`

Performs copy from the virgin to the scratch. This is very costly and should be ideally performed 
only once. Must be confirmed either by -y.

If the state says "mounted" but neither the dm-era device nor the filesystem mount actually exist
(e.g., after a host reboot or power loss), `full-recover` detects this stale state, clears it, and
proceeds with the copy. If either the dm-era device or the mount is still live, it refuses with
"already mounted". The stale flag is only persisted as part of the final state save after the copy
completes — if `full-recover` crashes mid-copy the stale detection will re-trigger on the next run.

### `mount`

Checks:

1. Checks that the RAM disk is adequately sized for the job.
1. Spot checks state of volumes and verifies that it is the same as the expected per the state of the 
app.

Zeroes the ramdisk and initializes dm-era for the scratch volume using the configured device name
(stored in state as `dm_era_name`, defaults to `bench_era`).

```
dmsetup create <dm_era_name> ... era /dev/ram0 /dev/nvme_ <granularity>
dmsetup message <dm_era_name> 0 checkpoint
```

Mounts the scratch volume at the specified location. 
It should be fool proof, in that it should not allow mount after amount.

### `recover`
 
Pre-checks:
 
- If the volume is not mounted (`is_mounted` is false in app state), there is nothing to recover.
  This is a no-op: print a message and exit successfully (exit code 0). This is important for
  scripting — callers like CI pipelines should be able to run `schelk recover` unconditionally
  without it triggering unnecessary fallback logic (e.g., a costly `full-recover`).
- `dmsetup`, `era_invalidate` is available in the PATH.

Unmount the filesystem first. This prevents further modifications and also would give a safeguard
from removing a filesystem while there is still some activity. That also flushes the filesystem.

```
umount /schelk
```
 
Take dm-era snapshot, invalidate (using the `dm_era_name` from state). 

```
# Create a clone of metadata for userspace reading.
dmsetup message <dm_era_name> 0 take_metadata_snap
# Collect all the changed blocks into a file.
era_invalidate --metadata-snapshot --written-since "$BASE" /dev/ram0 > changed.xml
# Drop the metadata snapshot.
dmsetup message <dm_era_name> 0 drop_metadata_snap
```

At this point we obtained `changed.xml` containing all the blocks to restore.

> 🦄 In the future, we could save `changed.xml` into the appstate so that if recovery fails for
  some reason (cancelled by the user, app crashes, system crashes) the user does not have to perform
  full migration.

We should tear down the device mapper.

```
dmsetup remove <dm_era_name>
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
   dmsetup message <dm_era_name> 0 take_metadata_snap
   era_invalidate --metadata-snapshot --written-since <base_era> /dev/ram0 > changed.xml
   dmsetup message <dm_era_name> 0 drop_metadata_snap
   ```

3. **Tear down dm-era.**
   ```
   dmsetup remove <dm_era_name>
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

The state includes a `dm_era_name` field (the device-mapper name for the dm-era target). This 
defaults to `bench_era` and is backwards-compatible: older state files without this field will use
the default. To run multiple schelk instances in parallel, use `--state-path` with separate state
files and `--dm-era-name` with unique names per instance.

Every update should be performed robustly: atomic updates (write to temp file, fsync, rename), 
`fsync` the directory, etc.

## Volume Checks

The simplest way to check that the too volumes are equal are checking their super blocks.

## Other Notes

- Use `async`.
- The copying should be performed in parallel. The jobs should use workstealing approach by batches.
- `eyre` for error reporting.
- use `rustfmt` to format the project code.
