# Vsock SSH Transport

macOS 26 Local Network Privacy (and compounding pf `network_isolation` rules) block
unprivileged processes from connecting to vmnet bridge addresses with `EHOSTUNREACH`.
Root and Terminal child processes are exempt, but vibebox-supervisor is not.
Fix: tunnel SSH through vsock, bypassing the network stack entirely.

**References:**
- [Local Network Privacy internals](https://eclecticlight.co/2026/01/18/last-week-on-my-mac-local-network-privacy-revealed/) — uses Network Extension packet filter, not standard pf; applies to all networking APIs
- [Apple engineer confirms Terminal exemption](https://mjtsai.com/blog/2024/10/02/local-network-privacy-on-sequoia/) — system apps bypass the check
- [EHOSTUNREACH from Local Network Privacy](https://developer.apple.com/forums/thread/765285) — returns misleading error instead of EPERM
- [UTM hit the same issue](https://github.com/utmapp/UTM/discussions/7472) — vmnet sandboxed in Sequoia+, no way to grant permission
- [VSCode identical symptom](https://github.com/microsoft/vscode/issues/228862) — ping works from Terminal, not from VSCode
- See `vibebox-bug.md` for local root-cause investigation

```
  CLI                  VM manager                      VM
  ┌──────────┐         ┌──────────────────┐         ┌──────────┐
  │ ssh -o   │         │  VZVirtualMachine │         │   sshd   │
  │ Proxy    │◀─unix──▶│    │              │         │ :22      │
  │ Command  │  sock   │  vsock:2222 ──────│─vsock──▶│ socat    │
  │          │ (byte   │    (VZVirtio      │         │ vsock→22 │
  │          │  relay) │     SocketDevice) │         │          │
  └──────────┘         └──────────────────┘         └──────────┘
                       NAT unchanged ─────────────────▶ curl, apt
```

CLI sends `vsock-connect` to the manager over the existing Unix socket. The manager opens a vsock connection to the VM and relays bytes bidirectionally over that socket. The CLI uses `vibebox vsock-proxy` as SSH `ProxyCommand` to pipe stdin/stdout through the relay. Falls back to TCP when vsock unavailable.

## Changes

**`vm.rs`** — Add `VZVirtioSocketDeviceConfiguration` to VM config. Store `VZVirtioSocketDevice` ref for `connectToPort`.

**`provision.sh`** — Add `socat` to apt-get install.

**`ssh.sh`** — After sshd verified, run `socat VSOCK-LISTEN:2222,reuseaddr,fork TCP:127.0.0.1:22 &`. Verify listening before `VIBEBOX_SSH_READY`. Skip if socat missing.

**`vm_manager.rs`** — New protocol message:
```
CLI → Manager:  "vsock-connect\n"
Manager → CLI:  "vsock-ok\n" then bidirectional byte relay
            or: "vsock-err:<reason>\n"
```
Call `connectToPort(2222)`, relay bytes between client Unix socket and vsock FD. Return error if VM stopping or forwarder not running.

**`vibebox.rs`** — Hidden `vsock-proxy <socket_path>` subcommand used as SSH `ProxyCommand`. Connects to manager socket, sends `vsock-connect`, relays stdin/stdout through the socket.

**`instance.rs`** — Try vsock first via `ProxyCommand=vibebox vsock-proxy <socket>`. Failure: fall back to existing TCP `ssh_port_open()` loop with warning log.

## Fallback

Old image without socat or old manager without `vsock-connect`: vsock fails, falls back to TCP. Works on macOS 15. Fails on macOS 26 with error suggesting `vibebox reset`.
