# Tasks

1. [x] Confirm requirements and scope from `implementations.md`.
2. [x] Define `SessionManager` responsibilities and public API (create, load, list, update, delete, bump last_active, refcount handling, cleanup orphaned index entries).
3. [x] Choose 3rd-party crates for UUIDv7, TOML persistence, and error handling (e.g., `uuid` with v7, `serde` + `toml`, `thiserror`).
4. [x] Write user journeys and unit test cases first (happy paths + error paths) for session lifecycle and index persistence.
5. [x] Implement `SessionManager` and supporting types with `Result`-based errors, filesystem IO, and atomic writes.
6. [x] Add tests for edge cases (missing index, invalid TOML, duplicate sessions, refcount transitions, cleanup on missing instance dir).
7. [ ] Run tests and coverage; target >=80% line/branch coverage using a Rust coverage tool (e.g., `cargo llvm-cov`).
8. [x] Refactor for clarity and reliability while keeping tests green.
9. [ ] Add TUI interface.
10. [ ] Integrate VM and SessionManager together.
