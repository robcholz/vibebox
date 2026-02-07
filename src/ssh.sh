#!/bin/sh

SSH_USER="__SSH_USER__"
SUDO_PASSWORD="__SUDO_PASSWORD__"
PROJECT_NAME="__PROJECT_NAME__"
KEY_PATH="__KEY_PATH__"

if [ -d /root/${PROJECT_NAME}/.vibebox ]; then
  mount -t tmpfs tmpfs /root/${PROJECT_NAME}/.vibebox
fi

if ! command -v sshd >/dev/null 2>&1; then
  apt-get update && apt-get install -y openssh-server sudo
fi

systemctl enable ssh >/dev/null 2>&1 || true
id -u ${SSH_USER} >/dev/null 2>&1 || useradd -m -s /bin/bash ${SSH_USER}
echo "${SSH_USER}:${SUDO_PASSWORD}" | chpasswd
usermod -aG sudo ${SSH_USER}
install -d -m 700 /home/${SSH_USER}/.ssh
install -m 600 ${KEY_PATH} /home/${SSH_USER}/.ssh/authorized_keys
chown -R ${SSH_USER}:${SSH_USER} /home/${SSH_USER}/.ssh
rm -f /home/${SSH_USER}/.bash_logout
mkdir -p /etc/ssh/sshd_config.d
cat >/etc/ssh/sshd_config.d/vibebox.conf <<'VIBEBOX_SSHD'
PasswordAuthentication no
KbdInteractiveAuthentication no
ChallengeResponseAuthentication no
PubkeyAuthentication yes
PermitRootLogin no
AllowUsers __SSH_USER__
VIBEBOX_SSHD
systemctl restart ssh
echo VIBEBOX_SSH_READY

echo "=== generated network file ==="
sed -n '1,200p' /run/systemd/network/10-netplan-all-en.network || true

while true; do
  ip=$(ip -4 -o addr show scope global | awk '{print $4}' | cut -d/ -f1 | head -n 1)
  if [ -n "$ip" ]; then
    echo VIBEBOX_IPV4=$ip
    break
  fi
  sleep 1
done
