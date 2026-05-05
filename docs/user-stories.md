# User Stories

Representative scenarios for testing schelk in database benchmarking workflows. The SUT (System
Under Test) is the application being benchmarked — in our case, typically
[reth](https://github.com/paradigmxyz/reth), an Ethereum execution client that stores its state
in a large on-disk database. Each story describes a user goal, the steps to achieve it, and the
expected behavior. Use these to validate that schelk meets its principles.

## 1. Bootstrap a reusable baseline from an empty filesystem

As a benchmarker, I want to create a fresh ext4 baseline, load a database snapshot once, and then
save that as the reusable benchmark starting point.

Steps:
1. `schelk init-new` with virgin, scratch, ramdisk, and mount point.
2. `schelk mount` — mounts the scratch volume.
3. Load the database snapshot onto the mounted filesystem.
4. `schelk promote` — promotes the scratch (with snapshot) to become the new virgin.
5. `schelk mount` again and verify the SUT starts from the promoted baseline.

## 2. Adopt an existing datadir

As a benchmarker, I already have a fully prepared datadir on a volume (e.g., a synced reth
datadir). I want schelk to adopt it as the virgin so I can run repeatable benchmarks without
re-preparing.

Steps:
1. `schelk init-from --virgin /dev/nvme1n1 --scratch /dev/nvme2n1 ...`
2. Wait for the full copy to complete.
3. `schelk mount` and verify the SUT starts and reads the datadir correctly.

## 3. Run a benchmark and recover

As a benchmarker, I want to run the SUT (e.g., reth block replay) against the scratch volume,
then quickly restore it to the original state so I can iterate on a different binary.

Steps:
1. `schelk mount`
2. Run the SUT against `/schelk`.
3. `schelk recover`
4. Verify the scratch volume is back to baseline (the SUT can start cleanly again).
5. Repeat with a different binary.

## 4. Multiple benchmark iterations without full copy

As a benchmarker, I want to run several back-to-back benchmarks (e.g., comparing different SUT
builds) and confirm that each recovery is fast and the volume state is identical each time.

Steps:
1. `schelk mount` → run benchmark → `schelk recover`. Note recovery duration and report.
2. `schelk mount` → run same benchmark → `schelk restore`. Note recovery duration and report,
   and verify the volume is mounted for the next run.
3. Verify recovery times are consistent and results are reproducible.

## 5. Promote after a schema migration

As a benchmarker, I ran a SUT version that performs a database migration. I want to promote
the migrated scratch volume to become the new virgin so future benchmarks start from the
migrated state.

Steps:
1. `schelk mount`
2. Run the SUT (which migrates the DB).
3. `schelk promote`
4. `schelk mount` → verify the SUT starts without re-migrating.

## 6. Recover after a process crash

As a benchmarker, the SUT crashed mid-benchmark (e.g., kill -9) but the host stayed up. I want
schelk to handle this gracefully and let me recover without a full copy.

Steps:
1. `schelk mount` → start the SUT → `kill -9` the SUT process.
2. `schelk recover` — should detect dirty state and recover incrementally.
3. Verify the volume is restored and the SUT starts cleanly.

## 7. Detect unsafe state after host reboot

As a benchmarker, the host rebooted or lost power while schelk was mounted. Since dm-era
metadata lives on a ramdisk, incremental recovery is not possible. I want schelk to detect
this and guide me to a safe recovery path.

Steps:
1. `schelk mount` → start the SUT → reboot the host.
2. `schelk recover` — should refuse incremental recovery and instruct to run `full-recover`.
3. `schelk full-recover` — detects that state says "mounted" but dm-era device and filesystem
   are gone, auto-clears the stale flag, and restores scratch from virgin.
4. Verify the SUT starts cleanly.

## 8. Full recovery fallback

As a benchmarker, when incremental recovery is unsafe (e.g., after host reboot, tampering, or
a bad `--no-copy` assumption), I want to run `full-recover` to restore scratch from virgin.

Steps:
1. `schelk full-recover -y`
2. Wait for the full copy to complete.
3. `schelk mount` → verify the SUT starts cleanly from the baseline.

## 9. Detect virgin volume tampering

As a benchmarker, I accidentally mounted the virgin volume outside of schelk. I want schelk to
detect this and refuse to proceed before I run a benchmark with a corrupted baseline.

Steps:
1. Mount the virgin volume manually and write a file.
2. Unmount it.
3. `schelk mount` — should detect superblock hash mismatch and refuse to proceed.

## 10. Prevent double mount

As a benchmarker, I accidentally run `schelk mount` twice. I want schelk to detect the existing
mount and refuse rather than corrupting state.

Steps:
1. `schelk mount`
2. `schelk mount` again — should fail with a clear error.

## 11. Init-from with --no-copy

As a benchmarker, I prepared both volumes identically myself (e.g., via dd). I want to skip the
full copy during init to save time.

Steps:
1. Manually copy virgin to scratch.
2. `schelk init-from --no-copy ...`
3. `schelk mount` → run benchmark → `schelk recover`.
4. Verify recovery produces correct results.

## 12. Environment validation on init

As a benchmarker on a fresh machine, I want schelk to tell me exactly what's missing before I
waste time on a partial setup.

Steps:
1. Remove `mkfs.ext4` from PATH.
2. `schelk init-new` — should fail with a clear message about the missing tool.
3. Install it, retry — should succeed.

## 13. Environment validation on recover

As a benchmarker, I want schelk to check that recovery tools are available before attempting
recovery.

Steps:
1. Remove `era_invalidate` from PATH.
2. `schelk recover` — should fail with a clear message about the missing tool.
3. Install it, retry — should succeed.

## 14. Destructive confirmation prompts

As a user, I want destructive commands (`init-new`, `init-from`, `full-recover`, `promote`) to
prompt for confirmation by default and proceed non-interactively only with `-y`.

Steps:
1. Run `schelk init-new ...` without `-y` — should prompt for confirmation.
2. Decline — should abort without changes.
3. Run `schelk init-new ... -y` — should proceed without prompting.

## 15. Reinitialization when state already exists

As a user, if schelk is already initialized, I want a second `init-*` call to prompt for
reinitialization rather than silently overwriting state.

Steps:
1. `schelk init-new ... -y` — initializes successfully.
2. `schelk init-new ...` again — should warn that state already exists and prompt to reinitialize.
3. Decline — state should remain unchanged.

## 16. Check status

As a benchmarker, I want `schelk status` to tell me whether schelk is initialized, mounted,
and safe to recover or promote.

Steps:
1. Before init — `schelk status` should report not initialized.
2. After init — should report initialized, not mounted.
3. After mount — should report initialized and mounted.
4. After recover — should report initialized, not mounted.

## 17. Cleanup after recover and promote

As a benchmarker, after `recover` or `promote`, I want schelk to leave no active dm-era device
or temporary state behind.

Steps:
1. `schelk mount` → run benchmark → `schelk recover`.
2. Verify no dm-era device exists (`dmsetup ls` should not show `bench_era`).
3. Verify no leftover `changed.xml` or temp files.

## 18. Recover is a no-op when not mounted

As a CI pipeline author, I want `schelk recover` to succeed (exit 0) when the volume is not
mounted, so that unconditional cleanup like `schelk recover || schelk full-recover` does not
trigger an unnecessary full recovery.

Steps:
1. `schelk init-new ... -y` — initialize without mounting.
2. `schelk recover` — should print that nothing is mounted and exit 0.
3. `schelk mount` → `schelk recover` — normal recovery, also exit 0.
4. `schelk recover` again — already recovered, should exit 0.

## 19. Transparent action logging

As a benchmarker, I want every action schelk takes to be printed to the screen so I can
understand what happened and diagnose issues.

Steps:
1. Run `schelk init-new ... -y` and observe output — should print each step (mkfs, copy, hash).
2. Run `schelk mount` — should print dm-era setup and mount actions.
3. Run `schelk recover` — should print unmount, era snapshot, block copy progress, and cleanup.

## 20. Refuse to write to a volume mounted outside schelk

As a benchmarker, I sometimes mount virgin or scratch outside of schelk by mistake (e.g.,
running `mount` for a quick inspection and forgetting to unmount). If I then run a destructive
operation that writes to that volume — `init-from`, `full-recover`, `recover`, or `promote` —
I want schelk to refuse before any block is written, so the live mounted filesystem is not
corrupted from underneath.

Steps:
1. `schelk init-new ... -y` — initialize as usual.
2. Manually mount the scratch volume outside schelk
   (e.g., `mount /dev/scratch /mnt/inspect`).
3. `schelk full-recover -y` — should fail with a clear "device is in use" error before any
   block is written.
4. Unmount the volume and retry — should succeed.
