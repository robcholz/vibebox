#!/bin/bash
set -eEux

trap 'echo "[vibebox][error] provisioning failed"; echo "VIBEBOX_PROVISION_FAILED"; systemctl poweroff || true; exit 1' ERR

# Wait for network + DNS before apt-get to avoid early boot flakiness.
wait_for_network() {
  echo "[vibebox] waiting for network/DNS readiness"
  local deadline=$((SECONDS + 60))
  while [ "$SECONDS" -lt "$deadline" ]; do
    local has_route=0
    if ip -4 route show default >/dev/null 2>&1; then
      has_route=1
    elif ip -6 route show default >/dev/null 2>&1; then
      has_route=1
    fi
    if [ "$has_route" -eq 1 ]; then
      if getent hosts deb.debian.org >/dev/null 2>&1; then
        return 0
      fi
    fi
    sleep 1
  done
  echo "[vibebox][warn] network/DNS still not ready after 60s; continuing" >&2
  echo "[vibebox][warn] /etc/resolv.conf:" >&2
  cat /etc/resolv.conf >&2 || true
  ip -br addr >&2 || true
  ip route >&2 || true
  ip -6 route >&2 || true
  return 0
}

apt_update_with_retries() {
  local attempt=1
  while [ "$attempt" -le 5 ]; do
    if apt-get update; then
      return 0
    fi
    echo "[vibebox][warn] apt-get update failed (attempt ${attempt}/5); retrying..." >&2
    attempt=$((attempt + 1))
    sleep 2
  done
  return 1
}

# Don't wait too long for slow mirrors.
echo 'Acquire::http::Timeout "2";' | tee /etc/apt/apt.conf.d/99timeout
echo 'Acquire::https::Timeout "2";' | tee -a /etc/apt/apt.conf.d/99timeout
echo 'Acquire::Retries "2";' | tee -a /etc/apt/apt.conf.d/99timeout

wait_for_network
apt_update_with_retries
apt-get install -y --no-install-recommends      \
        build-essential                         \
        pkg-config                              \
        libssl-dev                              \
        curl                                    \
        git                                     \
        ripgrep                                 \
        cloud-guest-utils                       \
        openssh-server                          \
        sudo

# Set hostname to "vibebox" so it's clear that you're inside the VM.
hostnamectl set-hostname vibebox

# SSH: host keys + base config (doesn't depend on runtime user)
ssh-keygen -A
mkdir -p /etc/ssh/sshd_config.d
cat >/etc/ssh/sshd_config.d/10-vibebox-base.conf <<'EOF'
PasswordAuthentication no
KbdInteractiveAuthentication no
ChallengeResponseAuthentication no
PubkeyAuthentication yes
PermitRootLogin no

# Speed up logins / avoid DNS delays
UseDNS no
GSSAPIAuthentication no
EOF

sshd -t
systemctl enable ssh
systemctl restart ssh

# Set this env var so claude doesn't complain about running as root.'
echo "export IS_SANDBOX=1" >> .bashrc

# Ensure cloned instances generate unique machine-id on first boot.
truncate -s 0 /etc/machine-id
rm -f /var/lib/dbus/machine-id
ln -sf /etc/machine-id /var/lib/dbus/machine-id

# Shutdown the VM when you logout 
cat > .bash_logout <<EOF
systemctl poweroff
sleep 100 # sleep here so that we don't see the login screen flash up before the shutdown.
EOF

# Done provisioning, power off the VM
echo "VIBEBOX_PROVISION_OK"
systemctl poweroff
