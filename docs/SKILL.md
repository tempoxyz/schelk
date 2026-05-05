---
name: schelk
description: Use this skill when a user wants to set up, initialize, operate, or troubleshoot schelk: a Linux-only CLI for repeatable benchmarking on large stateful datasets using a virgin block device, a scratch block device, and dm-era metadata on a ramdisk. Use it for installation, prerequisite checks, init-new, init-from, mount, recover, promote, full-recover, status, and crash or reboot recovery.
---

# schelk

`schelk` is a Linux-only benchmarking helper for large on-disk datasets such as database or reth
datadirs. It keeps a pristine **virgin** block device, runs benchmarks on a **scratch** block
device, and uses `dm-era` metadata on a ramdisk to learn which blocks changed. Recovery then copies
only the changed blocks from virgin back to scratch, so the benchmark loop is fast without running
on top of CoW or overlay machinery.

Use this skill when the user wants to install schelk, prepare baseline volumes, run the
`mount -> benchmark -> recover` loop, promote a new baseline, or recover from an unsafe state.

## Start From The URL

Do not assume the repository is already cloned locally.

If the user points you at this skill by URL, treat this as the entrypoint:

- [schelk SKILL.md](https://github.com/tempoxyz/schelk/blob/master/docs/SKILL.md)

If the repo is not available locally and you need the source or docs, clone it first:

```bash
git clone https://github.com/tempoxyz/schelk.git
cd schelk
```

## Read These First

When behavior is unclear, prefer these project documents over guesswork:

- [README.md](https://github.com/tempoxyz/schelk/blob/master/README.md): overview,
  prerequisites, example commands, limitations.
- [principles.md](https://github.com/tempoxyz/schelk/blob/master/docs/principles.md): safety and
  UX principles.
- [spec.md](https://github.com/tempoxyz/schelk/blob/master/docs/spec.md): intended command
  semantics.
- [user-stories.md](https://github.com/tempoxyz/schelk/blob/master/docs/user-stories.md):
  representative workflows and failure cases.

## Core Rules

- Treat `init-new`, `init-from`, `full-recover`, and `promote` as destructive.
- Do not guess device paths. Have the user confirm the exact `--virgin`, `--scratch`, `--ramdisk`,
  and `--mount-point` before destructive commands.
- Refuse to proceed on non-Linux hosts. This repository intentionally fails to build on macOS and
  Windows.
- All operational commands require root privileges.
- Do not mount or write to the virgin or scratch devices outside schelk.
- Use `--no-copy` only if the user explicitly states that virgin and scratch are already
  byte-identical.
- After a host reboot or power loss while mounted, assume incremental recovery is unsafe. Use
  `full-recover`.
- If the user wants multiple schelk instances in parallel, assign a unique `--dm-era-name` and a
  separate `--state-path` for each instance.
- Do not overpromise perfect device reset semantics. schelk restores logical blocks, not NVMe
  controller internal state.

## Inputs You Need

Before setup, gather:

- Linux host with `sudo` or root access.
- Two equal-size block devices:
  - virgin: pristine benchmark baseline.
  - scratch: writable working copy used for benchmark runs.
- One ramdisk device for `dm-era` metadata, commonly `/dev/ram0`.
- A mount point such as `/schelk`.
- Filesystem type:
  - `init-new` always creates `ext4`.
  - `init-from` requires `--fstype`.
- Optional: mount options, non-default granularity, custom `--state-path`, custom `--dm-era-name`.

Defaults worth knowing:

- Granularity defaults to `4096` bytes and should only be changed deliberately.
- Granularity must be a multiple of `512` bytes.
- The default `dm-era` name is `bench_era`.

The default state file is `/var/lib/schelk/state.json`. Use `--state-path` or `SCHELK_STATE` only
when the user needs an alternate state location.

## Environment Checks

Before destructive work, verify the environment and explain any missing prerequisites before doing
anything else.

```bash
uname -s
id -u
which cargo dmsetup era_invalidate mkfs.ext4
blockdev --getsize64 /dev/virgin /dev/scratch /dev/ram0
ls -l /dev/virgin /dev/scratch /dev/ram0
```

Interpretation:

- `uname -s` must be `Linux`.
- `id -u` should be `0` for operational commands.
- `mkfs.ext4` is required for `init-new`.
- `dmsetup` and `era_invalidate` are required for `mount`, `recover`, and `promote`.
- Virgin and scratch must be different block devices of equal size.
- The ramdisk must be large enough for the chosen volume size and granularity.

## Install

If the repository is already cloned locally, from the repository root:

```bash
cargo install --path .
```

If the repository is not cloned locally, install directly from GitHub:

```bash
cargo install --git https://github.com/tempoxyz/schelk.git
```

For development builds from a local checkout:

```bash
cargo build --release
```

If the current machine is not Linux, stop and tell the user they need a Linux host for both build
and runtime.

## Ramdisk Setup

Typical setup:

```bash
sudo modprobe brd rd_size=4194304
```

`rd_size` is in KiB, so `4194304` is 4 GiB. This is only a starting point; trust schelk's runtime
checks over rules of thumb.

## Choose The Right Initialization

Use `init-new` when both volumes can be destroyed and a fresh ext4 baseline should be created:

```bash
sudo schelk init-new \
  --virgin /dev/virgin \
  --scratch /dev/scratch \
  --ramdisk /dev/ram0 \
  --mount-point /schelk
```

Use `init-from` when the virgin volume already contains the prepared dataset:

```bash
sudo schelk init-from \
  --virgin /dev/virgin \
  --scratch /dev/scratch \
  --ramdisk /dev/ram0 \
  --mount-point /schelk \
  --fstype ext4
```

Use `--no-copy` only when the user explicitly asserts that both volumes are already identical:

```bash
sudo schelk init-from \
  --virgin /dev/virgin \
  --scratch /dev/scratch \
  --ramdisk /dev/ram0 \
  --mount-point /schelk \
  --fstype ext4 \
  --no-copy
```

Do not add `-y` unless the user explicitly wants non-interactive execution or has already
confirmed the destructive action with the exact device paths.

## Normal Operating Loop

Start with status if state may already exist:

```bash
sudo schelk status
```

Mount the scratch volume with `dm-era` tracking:

```bash
sudo schelk mount
```

Run the system under test against the configured mount point.

Recover incrementally afterward:

```bash
sudo schelk recover
```

If unmount is blocked and the user accepts killing blockers:

```bash
sudo schelk recover --kill
```

Important behavior:

- `recover` is a safe no-op when schelk is not mounted.
- `mount` verifies the superblock hash of virgin and scratch before proceeding.
- Mount options and filesystem type come from the saved schelk state.

## Promote A New Baseline

Use this when the current scratch state should become the new virgin baseline, such as after a
schema migration or one-time snapshot load:

```bash
sudo schelk mount
# run the migration or snapshot load
sudo schelk promote
```

If unmount is blocked and the user accepts killing blockers:

```bash
sudo schelk promote --kill
```

`promote` is destructive. It permanently overwrites the virgin device.

## Full-Recover Fallback

Use:

```bash
sudo schelk full-recover
```

Prefer `full-recover` when:

- the host rebooted or lost power while schelk was mounted;
- state says mounted but the `dm-era` device is gone;
- virgin or scratch integrity checks show the baseline is no longer trustworthy;
- a previous `--no-copy` assumption was wrong;
- the user wants to completely reset scratch from virgin.

This overwrites all data on scratch.

## Decision Guide

- The user needs a fresh empty baseline: use `init-new`.
- The user already prepared the baseline volume: use `init-from`.
- The user wants fast repeatable benchmark runs: use `mount`, run the workload, then `recover`.
- The user wants the current scratch state to become future baseline: use `promote`.
- The machine rebooted while mounted: use `full-recover`, then continue with `mount`.
- `recover` says nothing is mounted: treat it as success, not failure.

## Agent Behavior

- If the user only gives you the skill URL, start from that URL and do not assume a local checkout
  exists yet.
- If you need source context and the repo is absent locally, clone
  `https://github.com/tempoxyz/schelk.git`.
- Explain the exact command before any destructive step.
- Prefer `schelk status` first when state may already exist.
- Keep the benchmark workflow transparent: tell the user whether you are initializing, mounting,
  recovering, promoting, or doing a full reset.
- Preserve schelk's assumptions: the virgin and scratch devices are dedicated to schelk once
  initialized.
- If a command fails because prerequisites are missing, stop and surface the missing tool or
  environmental issue directly instead of improvising around it.

## Example User Requests

This skill should trigger for prompts like:

- "Use schelk to set up two NVMe volumes and `/dev/ram0` for benchmarking."
- "Adopt this prepared reth datadir as the schelk baseline."
- "Mount schelk, run the benchmark, and recover afterward."
- "Promote the migrated scratch volume so future runs start from the new baseline."
- "schelk says state is inconsistent after a reboot; recover it safely."
