# Existing Features (Legacy Snapshot)

This document summarizes the currently implemented features in the repo at the time of review.

## Core Purpose
- Single-binary CLI to spin up a Linux VM on Apple Silicon macOS for sandboxing LLM agents.
- Uses Apple Virtualization framework with a lightweight Debian nocloud image.

## VM Lifecycle
- Downloads a Debian base image on first run and verifies SHA-512 before use.
- Decompresses the base image and provisions it once, saving as a default template.
- Creates a per-project instance disk at `.vibe/instance.raw` on first run.
- Instance disks persist across runs until manually deleted.

## Default Sharing and Mounts
- Automatically shares the current project directory into the VM.
- Shares common cache/config directories when present:
- `~/.m2`, `~/.cargo/registry`, `~/.codex`, `~/.claude`.
- Maintains a dedicated guest mise cache at `~/.cache/vibe/.guest-mise-cache` on the host.
- Supports disabling default mounts with `--no-default-mounts`.
- Supports explicit mounts via `--mount host:guest[:read-only|:read-write]`.

## CLI Options and Automation
- `--cpus` and `--ram` to configure virtual CPU count and memory size.
- `--script` to upload and run a shell script inside the VM.
- `--send` to type a command into the VM console.
- `--expect` to wait for console output before continuing with script or send actions.
- Optional disk image argument to boot an existing raw disk directly.

## Console and Login Experience
- Auto-login as root by waiting for login prompt.
- Mounts shared directories using virtiofs, then bind-mounts to target paths.
- Prints a startup MOTD showing host and guest mount mappings.
- Streams VM console to the terminal with raw mode for interactive use.

## Security and Signing
- Checks for `com.apple.security.virtualization` entitlement on startup.
- Self-signs the binary with the required entitlement if missing, then re-execs.

## Networking and Devices
- NAT networking enabled by default.
- Virtio block device for storage and virtio entropy device for randomness.
- Serial console plumbing between host and guest for I/O.

## Platform Assumptions
- ARM-based macOS (Ventura or newer) is required.
- First run requires network access to fetch the base image.
