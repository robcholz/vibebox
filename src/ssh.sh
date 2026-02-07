#!/bin/sh
set -eu

SSH_USER="__SSH_USER__"
PROJECT_NAME="__PROJECT_NAME__"
KEY_PATH="__KEY_PATH__"

# 1) tmpfs mount
TARGET="/root/${PROJECT_NAME}/.vibebox"
if [ -d "$TARGET" ] && ! mountpoint -q "$TARGET"; then
  mount -t tmpfs tmpfs "$TARGET"
fi

# 2)
if ! id -u "$SSH_USER" >/dev/null 2>&1; then
  useradd -m -s /bin/bash -U "$SSH_USER"
  usermod -aG sudo "$SSH_USER" || true
fi

install -d -m 700 -o "$SSH_USER" -g "$SSH_USER" "/home/${SSH_USER}/.ssh"
install -m 600 -o "$SSH_USER" -g "$SSH_USER" "$KEY_PATH" "/home/${SSH_USER}/.ssh/authorized_keys"

# 3)
systemctl start ssh >/dev/null 2>&1 || true

# 4)
i=0
while :; do
  if ss -lnt 2>/dev/null | awk '{print $4}' | grep -qE '(:22)$'; then
    break
  fi
  i=$((i+1))
  [ "$i" -ge 40 ] && break   # ~4s
  sleep 0.1
done

echo VIBEBOX_SSH_READY

find_ip() {
  if command -v ip >/dev/null 2>&1; then
    ip -4 -o addr show scope global 2>/dev/null | awk '{print $4}' | cut -d/ -f1 | head -n 1
    return
  fi
  if command -v hostname >/dev/null 2>&1; then
    hostname -I 2>/dev/null | awk '{print $1}'
    return
  fi
}

i=0
while :; do
  ip="$(find_ip || true)"
  if [ -n "$ip" ]; then
    echo VIBEBOX_IPV4=$ip
    break
  fi
  i=$((i+1))
  [ "$i" -ge 60 ] && break
  sleep 0.5
done
