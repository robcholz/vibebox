# Implementations

## Definitions

### Vibebox Command

Commands for vibebox, the format is `:<command>`

### Box Command

Commands for general vm spawned by the vibebox, a typical linux command,
the format is as same as the linux, `<command>`

### Data Storage

Global session index is stored in `~/.vibebox/sessions.toml`.

Global images are stored in `~/.vibebox/images`.

Project config is stored in `[project_dir]/vibebox.toml`, this should be committed to VCS.

Project cache should be stored in `[project_dir]/.vibebox/`.

### Session management

Each session represents an underlying VM instance, users can quit, enter, spawn a session.

A session is represented by a UUID（RFC 4122 variant），and it is UUIDv7.

Example:

`019bf290-cccc-7c23-ba1d-dce7e6d40693`

Each session has the following members:

- `id`: session id
- `directory`: project directory (absolute), if directory changes name, this should also change.
- `last_active`: utc time indicating the last active time for the session

Sessions info are stored in `~/.vibebox/sessions.toml`

A session can be shut down, resumed, deleted, created.

A session links to a directory. Currently, a directory can only have a single session; a session can be connected to
multiple vibeboxes.

Instance data are stored in `project_dir/.vibebox`.

Deleting `[project_dir]/.vibebox/` permanently deletes the session. The global index entry (if any) will be removed by
any command that uses the global index but failed to locate.

There is a reference count for each session, and this represents the number of `vibebox` using that session.

A session (VM) will only shut down after `AUTO_SHUTDOWN_GRACE_MS` milliseconds if the reference count is 0. Shutting a
session down can save some resources, but the first startup time will be larger than resuming an active session.

When a session shuts down, the VM stops, but the instance disk in project_dir/.vibebox/ is preserved for faster
later boots.

#### Behavior

In host cli:

- use `vibebox` without config to connect to an exising session, or create a new one if not existed.
- use `vibebox delete <session_id>` to delete an existed session, delete <session_id> removes
  `[session.directory]/.vibebox/` and deletes the global index entry.
- use `vibebox list` to list a list of sessions

In vibebox:

- use `:new` to prompt user to delete and create a session.
- use `:exit` to exit vibebox.

### (Shows all the mounts/network/visibility)

Each session has mounts, meaning it has a file system mapping from host to the inner VM, this has a default value, and
users can add new mounts to it.

Each session also has its own network mapping, users can choose to use blocklist or allowlist mode to control the
traffic by hostname/domain.

The command can display the mapping and network strategy.

They are also stored per project, in `vibebox.toml`

#### Behaviors

- use `:explain` to display:
    - mounts: host_path → guest_path, ro/rw
    - network: mode (allowlist/blocklist) and entries
    - storage: paths to vibebox.toml and .vibebox/ (relative from the project_dir)

## Connection

### SSH

- In Project cache, generate and store ssh pair
- In provisioning, install and enable openssh-server in VM
- Mount ssh pair to VM when starting up
- get ipv4 address of VM, store it to project cache
- and connect to VM via ssh with ip and ssh key