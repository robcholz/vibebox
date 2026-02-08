# Contributing to Vibebox

Thanks for your interest in contributing! This guide keeps PRs small, reviewable, and consistent with the project’s
development workflow.

**Prerequisites**

- macOS on Apple Silicon (required for the virtualization backend)
- Rust `1.91.1` or newer (see `Cargo.toml`)

**Getting Started**

1. Fork the repo and create a feature branch.
2. Build once to validate your toolchain:

```bash
cargo build --locked
```

**Development Commands**

- Format: `cargo fmt --all -- --check`
- Lint: `cargo clippy --all-targets --all-features -- -D warnings`
- Test: `cargo test --locked`
- Build: `cargo build --locked`

**Submitting Changes**

- Keep changes focused and scoped to one problem.
- Update or add tests when behavior changes.
- If you change user-facing behavior, update docs or help text.
- Avoid adding heavy dependencies without a clear reason.

**Reporting Issues**
Please include:

- macOS version and hardware (Apple Silicon model)
- Vibebox version (`vibebox --version`)
- Steps to reproduce
- Logs from `.vibebox/cli.log`, `.vibebox/vm_root.log` and `.vibebox/vm_manager.log`

**Security**
If you believe you’ve found a security issue, please avoid public disclosure. Open a private report via GitHub Security
Advisories instead.
