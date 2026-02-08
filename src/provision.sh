#!/bin/bash
set -eux

# Don't wait too long for slow mirrors.
echo 'Acquire::http::Timeout "2";' | tee /etc/apt/apt.conf.d/99timeout
echo 'Acquire::https::Timeout "2";' | tee -a /etc/apt/apt.conf.d/99timeout
echo 'Acquire::Retries "2";' | tee -a /etc/apt/apt.conf.d/99timeout

apt-get update
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
systemctl poweroff
