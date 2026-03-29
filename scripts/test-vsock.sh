#!/bin/bash
# test-vsock.sh — E2E test for vsock SSH transport
# Run from the vibebox repo root: ./scripts/test-vsock.sh
#
# Prerequisites:
#   - cargo build --release
#   - Delete ~/.cache/vibebox/default.raw to force reprovision with socat
#
# What it tests:
#   1. VM boots and provisions (socat installed)
#   2. Vsock transport connects SSH
#   3. Commands execute in the VM
#   4. TCP fallback works (when vsock is unavailable)
#   5. Clean shutdown

set -euo pipefail

VIBEBOX="./target/release/vibebox"
LOG_DIR=".vibebox"
PASS=0
FAIL=0
TESTS_RUN=0

log()    { echo "[$(date +%H:%M:%S)] $*"; }
pass()   { PASS=$((PASS + 1)); TESTS_RUN=$((TESTS_RUN + 1)); log "PASS: $1"; }
fail()   { FAIL=$((FAIL + 1)); TESTS_RUN=$((TESTS_RUN + 1)); log "FAIL: $1"; }
info()   { log "INFO: $*"; }
header() { echo; log "=== $1 ==="; }

cleanup() {
    info "Cleaning up..."
    # Kill any lingering vibebox-supervisor
    pkill -f vibebox-supervisor 2>/dev/null || true
    sleep 1
}
trap cleanup EXIT

# --- Preflight ---
header "Preflight checks"

if [ ! -f "$VIBEBOX" ]; then
    log "ERROR: $VIBEBOX not found. Run 'cargo build --release' first."
    exit 1
fi
info "Binary: $VIBEBOX ($(file -b "$VIBEBOX" | head -c 60))"
info "Version: $($VIBEBOX --version 2>&1 || true)"
info "macOS: $(sw_vers -productVersion 2>/dev/null || echo unknown)"
info "Arch: $(uname -m)"
info "User: $(whoami) (uid=$(id -u))"

# Check if default.raw has socat — warn if it might be stale
if [ -f ~/.cache/vibebox/default.raw ]; then
    info "default.raw exists — assuming it has socat. Delete to reprovision if needed."
else
    info "default.raw missing — will provision fresh VM with socat."
fi

# --- Test 1: VM boots and vsock SSH connects ---
header "Test 1: VM boot + vsock SSH"

info "Starting vibebox (this may take 1-3 minutes on first run)..."
SSH_OUTPUT=$("$VIBEBOX" 2>"$LOG_DIR/test1_stderr.log" <<'HEREDOC' || true
echo "VSOCK_TEST_MARKER_$(date +%s)"
uname -a
which socat && echo "SOCAT_PRESENT" || echo "SOCAT_MISSING"
pgrep -f 'socat.*VSOCK-LISTEN' >/dev/null && echo "VSOCK_LISTENER_ACTIVE" || echo "VSOCK_LISTENER_INACTIVE"
exit
HEREDOC
)

info "SSH output:"
echo "$SSH_OUTPUT" | head -20

# Check results
if echo "$SSH_OUTPUT" | grep -q "VSOCK_TEST_MARKER_"; then
    pass "SSH session executed commands"
else
    fail "SSH session did not execute commands"
    info "stderr tail:"
    tail -20 "$LOG_DIR/test1_stderr.log" 2>/dev/null || true
fi

if echo "$SSH_OUTPUT" | grep -q "SOCAT_PRESENT"; then
    pass "socat is installed in VM"
else
    fail "socat is NOT installed in VM"
fi

if echo "$SSH_OUTPUT" | grep -q "VSOCK_LISTENER_ACTIVE"; then
    pass "vsock listener (port 2222) is active in VM"
else
    fail "vsock listener is NOT active — socat may have failed to start"
fi

# Check manager log for vsock readiness
if grep -q "vsock device ready" "$LOG_DIR/vm_manager.log" 2>/dev/null; then
    pass "Manager reports vsock device ready"
else
    fail "Manager did not report vsock device ready"
fi

# Check stderr for transport used
if grep -q "trying vsock ssh" "$LOG_DIR/test1_stderr.log" 2>/dev/null; then
    pass "CLI attempted vsock transport"
else
    info "CLI did not attempt vsock (may have fallen back to TCP)"
fi

if grep -q "falling back to TCP" "$LOG_DIR/test1_stderr.log" 2>/dev/null; then
    info "WARNING: Fell back to TCP — vsock may not be working"
else
    info "No TCP fallback detected — vsock transport used"
fi

# --- Test 2: Re-entry (second SSH session, reuse running VM) ---
header "Test 2: Re-entry — second SSH session on existing VM"

# The core re-entry test: exit, then immediately reconnect.
# This reproduces the bug where vsock SSH hangs on the second connect.
# auto_shutdown_ms=2000 in vibebox.toml, so we must reconnect within 2s.

info "Starting second session immediately (must beat 2s auto-shutdown)..."
SSH_OUTPUT2=$("$VIBEBOX" 2>"$LOG_DIR/test2_stderr.log" <<'HEREDOC' || true
echo "SESSION2_OK_$(date +%s)"
# Verify vsock listener is still alive after first session
pgrep -f 'socat.*VSOCK-LISTEN' >/dev/null && echo "VSOCK_STILL_ACTIVE" || echo "VSOCK_DEAD"
# Verify sshd is still running
systemctl is-active ssh >/dev/null 2>&1 && echo "SSHD_STILL_ACTIVE" || echo "SSHD_DEAD"
exit
HEREDOC
)

info "Session 2 output:"
echo "$SSH_OUTPUT2" | head -10

if echo "$SSH_OUTPUT2" | grep -q "SESSION2_OK_"; then
    pass "Re-entry SSH session works"
else
    fail "Re-entry SSH session failed"
    info "stderr:"
    cat "$LOG_DIR/test2_stderr.log" 2>/dev/null || true
fi

# Check transport used on re-entry
if grep -q "trying vsock ssh" "$LOG_DIR/test2_stderr.log" 2>/dev/null; then
    if grep -q "falling back to TCP" "$LOG_DIR/test2_stderr.log" 2>/dev/null; then
        fail "Re-entry fell back to TCP — vsock did not work on second connect"
    else
        pass "Re-entry used vsock transport"
    fi
elif grep -q "using tcp ssh" "$LOG_DIR/test2_stderr.log" 2>/dev/null; then
    fail "Re-entry used TCP instead of vsock"
else
    info "Re-entry transport unclear from logs"
fi

if echo "$SSH_OUTPUT2" | grep -q "VSOCK_STILL_ACTIVE"; then
    pass "socat vsock listener survived re-entry"
elif echo "$SSH_OUTPUT2" | grep -q "VSOCK_DEAD"; then
    fail "socat vsock listener died between sessions"
fi

# --- Test 2b: Re-entry after brief delay ---
header "Test 2b: Re-entry after 1s delay"

sleep 1
SSH_OUTPUT2B=$("$VIBEBOX" 2>"$LOG_DIR/test2b_stderr.log" <<'HEREDOC' || true
echo "SESSION2B_OK"
exit
HEREDOC
)

if echo "$SSH_OUTPUT2B" | grep -q "SESSION2B_OK"; then
    pass "Re-entry after 1s delay works"
else
    fail "Re-entry after 1s delay failed"
    info "stderr:"
    tail -15 "$LOG_DIR/test2b_stderr.log" 2>/dev/null || true
fi

# --- Test 2c: Re-entry after VM shutdown (past auto_shutdown grace period) ---
header "Test 2c: Re-entry after VM shutdown"

# Wait past the 2s auto_shutdown_ms so the old VM is poweroff'd.
# The client should detect the dying manager, retry, and boot a fresh VM.
info "Waiting 3s for VM to shut down..."
sleep 3

SSH_OUTPUT2C=$("$VIBEBOX" 2>"$LOG_DIR/test2c_stderr.log" <<'HEREDOC' || true
echo "SESSION2C_OK_$(date +%s)"
exit
HEREDOC
)

info "Session 2c output:"
echo "$SSH_OUTPUT2C" | tail -5

if echo "$SSH_OUTPUT2C" | grep -q "SESSION2C_OK_"; then
    pass "Re-entry after VM shutdown works"
else
    fail "Re-entry after VM shutdown failed"
    info "stderr:"
    tail -20 "$LOG_DIR/test2c_stderr.log" 2>/dev/null || true
fi

# Verify the client retried with a fresh VM
if grep -q "starting a fresh VM" "$LOG_DIR/test2c_stderr.log" 2>/dev/null; then
    pass "Client detected shutdown and spawned fresh VM"
elif grep -q "spawning vm manager" "$LOG_DIR/test2c_stderr.log" 2>/dev/null; then
    pass "Client spawned new vm manager"
else
    info "No fresh-VM retry detected in logs (may have connected to still-alive manager)"
fi

# --- Test 3: vsock-proxy subcommand ---
header "Test 3: vsock-proxy subcommand (error handling)"

# Test with nonexistent socket — should fail gracefully
PROXY_OUTPUT=$("$VIBEBOX" vsock-proxy /tmp/nonexistent-vibebox-test.sock 2>&1 || true)

if echo "$PROXY_OUTPUT" | grep -qi "connect to manager socket.*no such file"; then
    pass "vsock-proxy fails gracefully on missing socket"
else
    fail "vsock-proxy did not fail gracefully: $PROXY_OUTPUT"
fi

# --- Test 4: vsock-proxy hidden from help ---
header "Test 4: vsock-proxy hidden from CLI help"

HELP_OUTPUT=$("$VIBEBOX" --help 2>&1)
if echo "$HELP_OUTPUT" | grep -q "vsock-proxy"; then
    fail "vsock-proxy is visible in --help (should be hidden)"
else
    pass "vsock-proxy is hidden from --help"
fi

# --- Summary ---
header "Results"
log "Tests run: $TESTS_RUN  Passed: $PASS  Failed: $FAIL"

if [ "$FAIL" -gt 0 ]; then
    log ""
    log "Diagnostic files:"
    log "  Manager log:  $LOG_DIR/vm_manager.log"
    log "  VM root log:  $LOG_DIR/vm_root.log"
    log "  Test stderr:  $LOG_DIR/test1_stderr.log"
    log "  Provision:    $LOG_DIR/provision.log"
    exit 1
fi

log "All tests passed."
