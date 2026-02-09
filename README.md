<p align="center">
  <a href="https://vibebox.robcholz.com">
    <picture>
      <img src="docs/banner.png" alt="VibeBox logo">
    </picture>
  </a>
</p>
<p align="center">an ultrafast, open-source sandbox for running coding agents safely.</p>

<p align="center">
  <a href="https://crates.io/crates/vibebox">
    <img alt="Crates.io" src="https://img.shields.io/crates/v/vibebox.svg">
  </a>
  <a href="https://github.com/robcholz/vibebox/blob/main/LICENSE">
    <img alt="MIT licensed" src="https://img.shields.io/badge/license-MIT-blue.svg">
  </a>
  <a href="https://github.com/robcholz/vibebox/actions?query=workflow%3ACI+branch%3Amain">
    <img alt="Build Status" src="https://github.com/robcholz/vibebox/workflows/CI/badge.svg">
  </a>
</p>

<p align="center">
  <a href="README.md">English</a> |
  <a href="README.zh.md">简体中文</a>
</p>

**VibeBox is a per-project micro-VM sandbox for running coding agents on macOS (Apple Virtualization Framework).**
It’s optimized for a *daily-driver* workflow: fast warm re-entry, explicit mounts, and reusable sessions.

**Who it’s for:** macOS users running coding agents who want real isolation without giving up a fast daily workflow.

**Quick facts:** warm re-entry is typically **<5s** on my M3 (varies by machine/cache); first run downloads and
provisions a Debian base image (network dependent).

**Security model:** Linux guest VM with explicit mount allowlists from `vibebox.toml` (repo-first, everything else
opt-in).

- **enter/attach in seconds:** `vibebox` drops you into a reusable sandbox for the current repo
- **project-scoped by default:** explicit mounts + repo-contained changes (repo-first, everything else is allowlisted)
- **sessioned:** multi-instance + session management (reuse, multiple terminals, cleanup)

### Quick Demo

```bash
# from any repo
cd my-project
vibebox
```

What you should see (roughly):

```text
vibebox: starting (session: my-project)
vibebox: attaching...
vibecoder@vibebox:~/my-project$
```

[![VibeBox Terminal UI](docs/screenshot.png)](https://vibebox.robcholz.com)

---

### Why I built VibeBox

I use coding agents daily, and I wanted to give them a real shell without handing them my host machine.
Lock things down and you get nonstop confirmations; loosen it up and you worry about deleting files, touching secrets,
or wandering outside the repo.

VibeBox is the middle ground: a per-repo sandbox with a hard VM boundary, fast re-entry, and explicit mounts. It’s built
to be “always on” for agent work without turning safety into a chore.

### Why a micro-VM (vs containers)?

Containers are great. VibeBox isn’t trying to replace Docker/devcontainers for building services.

I specifically wanted a VM-shaped default for agent workflows on macOS:

- **guest-kernel isolation boundary by default:** when I’m letting an agent run arbitrary commands, I want “safe mode”
  to
  be a Linux guest, not my host.
- **sessions as a first-class workflow:** attach/reuse per repo, multiple terminals into the same sandbox, reliable
  cleanup to avoid orphan environments.
- **explicit mount allowlists as the primary UX:** repo-scoped by default; anything else is an explicit decision.
- **minimal per-repo setup:** you *can* reproduce parts of this with compose/devcontainers, but I wanted a single
  command
  that works repo-to-repo without maintaining container configs for the basic “safe shell” workflow.

### Comparison

Here’s why I didn’t just use existing options:

- **vibe**: super convenient and nails “zero-config, just go”. VibeBox is intentionally on a different axis: per-repo
  config + sessions + multi-instance lifecycle.
- **QEMU**: powerful, but the configuration surface area is huge. For day-to-day sandboxing it’s not “open a repo and
  go” — it’s a project on its own.
- **Docker / devcontainers / devpods**: great ecosystem. My friction wasn’t raw startup time, it was the day-to-day
  overhead of keeping per-repo agent sandboxes *safe-by-default* (mount allowlists, secrets exposure, attach/reuse,
  cleanup) without maintaining container configs per repo for the basic workflow.

That’s what pushed me to build **VibeBox**: I wanted a per-project sandbox that’s fast to enter (just `vibebox`),
supports real configuration + sessions, and keeps a hard isolation boundary.

### Installation

```bash
# install script
curl -fsSL https://raw.githubusercontent.com/robcholz/vibebox/main/install | bash

# package managers
cargo install vibebox

# manual install
curl -LO https://github.com/robcholz/vibebox/releases/download/latest/vibebox-macos-arm64.zip
unzip vibebox-macos-arm64.zip
mkdir -p ~/.local/bin
mv vibebox ~/.local/bin
export PATH="$HOME/.local/bin:$PATH"
```

**Requirements**

- macOS on Apple Silicon (VibeBox uses Apple's virtualization APIs).

**First Run**

The first `vibebox` run downloads a Debian base image and provisions it. After that, per-project instances reuse the
cached base image for much faster startups.

### Documentation

**Quick Start**

```bash
cd /path/to/your/project
vibebox
```

On first run, VibeBox creates `vibebox.toml` in your project (if missing) and a `.vibebox/` directory for instance data.

**Configuration (`vibebox.toml`)**

`vibebox.toml` lives in your project root by default. You can override it with `vibebox -c path/to/vibebox.toml` or the
`VIBEBOX_CONFIG_PATH` env var, but the path must stay inside the project directory.

Default config (auto-created when missing):

```toml
[box]
cpu_count = 2
ram_mb = 2048
disk_gb = 5
mounts = [
    "~/.codex:~/.codex:read-write",
    "~/.claude:~/.claude:read-write",
]

[supervisor]
auto_shutdown_ms = 20000
```

`disk_gb` is only applied when the instance disk is first created. If you change it later, run `vibebox reset` to
recreate the disk.

**Mounts**

- Your project is mounted read-write at `~/<project-name>`, and the shell starts there.
- If a `.git` directory exists, it is masked with a tmpfs mount inside the VM to discourage accidental edits from the
  guest.
- Extra mounts come from `box.mounts` with the format `host:guest[:read-only|read-write]`.
- Host paths support `~` expansion. Relative guest paths are treated as `/root/<path>`.
- Guest paths that use `~` are linked into `/home/<ssh-user>` for convenience. Run `vibebox explain` to see the resolved
  host/guest mappings.

**CLI Commands**

```bash
vibebox             # start or attach to the current project VM
vibebox list        # list known project sessions
vibebox reset       # delete .vibebox for this project and recreate on next run
vibebox purge-cache # delete the global cache (~/.cache/vibebox)
vibebox explain     # show mounts and network info
```

**Inside the VM**

- Default SSH user: `vibecoder`
- Hostname: `vibebox`
- Base image provisioning installs: build tools, `git`, `curl`, `ripgrep`, `openssh-server`, and `sudo`.
- On first login, VibeBox installs `mise` and configures tools like `uv`, `node`, `@openai/codex`, and
  `@anthropic-ai/claude-code` (best-effort).
- Shell aliases: `:help` and `:exit`.

**State & Cache**

- Project state lives in `.vibebox/` (instance disk, SSH keys, logs, manager socket/pid). `vibebox reset` removes it.
- Global cache lives in `~/.cache/vibebox` (base image + shared guest cache). `vibebox purge-cache` clears it.
- Session index lives in `~/.vibebox/sessions` and is shown by `vibebox list`.

### Contributing

If you're interested in contributing to VibeBox, please read our [contributing docs](CONTRIBUTING.md) before
submitting a pull request.

### FAQ

#### How is this different from other sandboxes?

VibeBox is built for fast, repeatable local sandboxes with minimal ceremony. What’s different here:

- Warm re-entry is typically **<5s** on my M3 (varies by machine/cache), so you can jump back in quickly.
- One simple command — `vibebox` — drops you into the sandbox from your project.
- Configuration lives in `vibebox.toml`, where you can set CPU, RAM, disk size, and mounts.
- Sessions are first-class: reuse, multiple terminals, cleanup.

### Special thanks

[vibe](https://github.com/lynaghk/vibe) by lynaghk.

And the amazing Rust community — without the ecosystem and toolchain like [crates.io](https://crates.io), this wouldn't
be possible!

---

**Follow me on X** [x.com/robcholz](https://x.com/robcholz)
