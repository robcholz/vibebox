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
        openssh-server                          \
        locales                                 \
        sudo

# Set hostname to "vibe" so it's clear that you're inside the VM.
hostnamectl set-hostname vibe

# Locale (fix: setlocale: LC_CTYPE ... UTF-8)
sed -i 's/^# *en_US.UTF-8 UTF-8/en_US.UTF-8 UTF-8/' /etc/locale.gen
locale-gen
update-locale LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8

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

# Shutdown the VM when you logout 
cat > .bash_logout <<EOF
systemctl poweroff
sleep 100 # sleep here so that we don't see the login screen flash up before the shutdown.
EOF


# Install Rust
curl https://sh.rustup.rs -sSf | sh -s -- -y --profile minimal --component "rustfmt,clippy"


# Install Mise
curl https://mise.run | sh
echo 'eval "$(~/.local/bin/mise activate bash)"' >> .bashrc

export PATH="$HOME/.local/bin:$PATH"
eval "$(mise activate bash)"

mkdir -p .config/mise/

cat > .config/mise/config.toml <<MISE
    [settings]
    # Always use the venv created by uv, if available in directory
    python.uv_venv_auto = true
    experimental = true
    idiomatic_version_file_enable_tools = ["rust"]

    [tools]
    uv = "0.9.25"
    node = "24.13.0"
    "npm:@openai/codex" = "latest"
    "npm:@anthropic-ai/claude-code" = "latest"
MISE

touch .config/mise/mise.lock
mise install

# Done provisioning, power off the VM
systemctl poweroff
