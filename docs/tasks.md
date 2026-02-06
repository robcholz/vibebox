# Tasks

## SessionManager

1. [x] Confirm requirements and scope from `implementations.md`.
2. [x] Define `SessionManager` responsibilities and public API (create, load, list, update, delete, bump last_active,
   refcount handling, cleanup orphaned index entries).
3. [x] Choose 3rd-party crates for UUIDv7, TOML persistence, and error handling (e.g., `uuid` with v7, `serde` + `toml`,
   `thiserror`).
4. [x] Write user journeys and unit test cases first (happy paths + error paths) for session lifecycle and index
   persistence.
5. [x] Implement `SessionManager` and supporting types with `Result`-based errors, filesystem IO, and atomic writes.
6. [x] Add tests for edge cases (missing index, invalid TOML, duplicate sessions, refcount transitions, cleanup on
   missing instance dir).
7. [ ] Run tests and coverage; target >=80% line/branch coverage using a Rust coverage tool (e.g., `cargo llvm-cov`).
8. [x] Refactor for clarity and reliability while keeping tests green.

## TUI

1. [x] Review `docs/tui.md` requirements and translate into concrete UI sections and state model.
2. [x] Add required dependencies for ratatui/crossterm/tokio/color-eyre/futures and pick a text input widget crate.
3. [x] Write unit tests for layout calculations (header/terminal/input/status/completions), completion state
   transitions, and CLI argument parsing.
4. [x] Implement TUI state model (header info, terminal history, input area, completion list, status bar visibility).
5. [x] Implement rendering functions for header, terminal area, input area, completions, and status bar.
6. [x] Implement async event loop (keyboard, resize, tick) with crossterm EventStream + tokio.
7. [x] Add a standalone TUI CLI binary (no main.rs wiring) with placeholder VM info and TODOs for VM integration.
8. [ ] Run tests and validate coverage for the new module.

## TUI

1. [x] Fix the terminal component height issue.
2. [x] Fix the input field that does not expand its height (currently, it just roll the text horizontally). The
   inputfield it should not be scrollable.

## Integration

1. [x] Wire up the vm and tui.
2. [ ] Use ssh to connect to vm.
3. [ ] wire up SessionManager.
4. [ ] VM should be separated by per-session VM daemon process (only accepts if to shutdown vm and itself).
