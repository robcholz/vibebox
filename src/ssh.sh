#!/bin/sh
set -eu

SSH_USER="__SSH_USER__"
PROJECT_NAME="__PROJECT_NAME__"
KEY_PATH="__KEY_PATH__"

diag() { echo "[vibebox][diag] $*" >&2; }

# Extract default route facts (no hardcode)
default_route_line() { ip -4 route show default 2>/dev/null | head -n 1 || true; }
default_dev() {
  default_route_line | awk '{for(i=1;i<=NF;i++) if($i=="dev"){print $(i+1); exit}}'
}
default_gw() {
  default_route_line | awk '{for(i=1;i<=NF;i++) if($i=="via"){print $(i+1); exit}}'
}

dump_diag() {
  diag "=== default route ==="
  default_route_line >&2 || true

  diag "=== systemctl status ssh ==="
  systemctl status ssh --no-pager >&2 || true

  diag "=== journalctl -u ssh (tail) ==="
  journalctl -u ssh -n 120 --no-pager >&2 || true

  diag "=== sshd -t ==="
  sshd -t >&2 || true

  diag "=== sshd -T (listen/addressfamily) ==="
  sshd -T 2>/dev/null | egrep -i '^(listenaddress|addressfamily|port|permitrootlogin|allowusers)\b' >&2 || true

  diag "=== listeners on :22 ==="
  ss -lntp 2>/dev/null | awk 'NR==1 || $4 ~ /:22$/' >&2 || true

  diag "=== ip -br addr ==="
  ip -br addr >&2 || true

  diag "=== ip route ==="
  ip route >&2 || true

  gw="$(default_gw || true)"
  if [ -n "$gw" ]; then
    diag "=== ping default gateway ($gw) ==="
    ping -c1 -W1 "$gw" >/dev/null 2>&1 && diag "ping gw OK" || diag "ping gw FAIL"
  fi
}

# 1) tmpfs mount
TARGET="/root/${PROJECT_NAME}/.vibebox"
if [ -d "$TARGET" ] && ! mountpoint -q "$TARGET"; then
  mount -t tmpfs tmpfs "$TARGET"
fi

# 2) user + authorized_keys
if ! id -u "$SSH_USER" >/dev/null 2>&1; then
  useradd -m -s /bin/bash -U "$SSH_USER"
  usermod -aG sudo "$SSH_USER" || true
fi

install -d -m 700 -o "$SSH_USER" -g "$SSH_USER" "/home/${SSH_USER}/.ssh"
install -m 600 -o "$SSH_USER" -g "$SSH_USER" "$KEY_PATH" "/home/${SSH_USER}/.ssh/authorized_keys"

# Ensure codex/claude are visible in the user's HOME
USER_HOME="$(getent passwd "$SSH_USER" | cut -d: -f6 2>/dev/null || true)"
if [ -z "$USER_HOME" ]; then
  USER_HOME="/home/${SSH_USER}"
fi
install -d -m 755 /usr/local/codex /usr/local/claude
if [ ! -e "${USER_HOME}/.codex" ]; then
  ln -s /usr/local/codex "${USER_HOME}/.codex"
fi
if [ ! -e "${USER_HOME}/.claude" ]; then
  ln -s /usr/local/claude "${USER_HOME}/.claude"
fi
chown -h "${SSH_USER}:${SSH_USER}" "${USER_HOME}/.codex" "${USER_HOME}/.claude" 2>/dev/null || true

# Install Mise
curl https://mise.run | sh
echo 'eval "$(~/.local/bin/mise activate bash)"' >> "${USER_HOME}/.bashrc"

export PATH="${USER_HOME}/.local/bin:$PATH"

mkdir -p "${USER_HOME}/.config/mise"

cat > "${USER_HOME}/.config/mise/config.toml" <<MISE
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

touch "${USER_HOME}/.config/mise/mise.lock"
mise install

# 3) start ssh (don't swallow failures)
# If ssh is already active, don't force start/restart.
if ! systemctl is-active --quiet ssh; then
  if ! systemctl start ssh; then
    diag "systemctl start ssh failed"
    dump_diag
    exit 1
  fi
fi

# 4) obtain stable IPv4 on the default-route interface (wait up to ~30s)
ip_on_dev() {
  dev="$1"
  ip -4 -o addr show dev "$dev" scope global 2>/dev/null \
    | awk '{print $4}' | cut -d/ -f1 | head -n 1 || true
}

ip=""
dev=""
gw=""
t=0
while [ "$t" -lt 60 ]; do
  dev="$(default_dev || true)"
  gw="$(default_gw || true)"
  if [ -n "$dev" ]; then
    ip="$(ip_on_dev "$dev")"
  else
    ip=""
  fi

  if [ -n "$dev" ] && [ -n "$ip" ]; then
    # optional: if a gateway exists, require it to answer to avoid "ip exists but link dead"
    if [ -z "$gw" ] || ping -c1 -W1 "$gw" >/dev/null 2>&1; then
      break
    fi
  fi

  t=$((t+1))
  sleep 0.5
done

if [ -z "$dev" ] || [ -z "$ip" ]; then
  diag "no stable IPv4 on default route interface"
  dump_diag
  exit 1
fi

# 5) strong verify: ssh must listen externally (0.0.0.0:22 or $ip:22 or [::]:22)
listens_ok() {
  ss -lnt 2>/dev/null \
    | awk 'NR>1 {print $4}' \
    | grep -Eq "^(0\.0\.0\.0:22|\\[::\\]:22|${ip}:22)$"
}

i=0
while ! listens_ok && [ "$i" -lt 80 ]; do   # ~8s
  i=$((i+1))
  sleep 0.1
done

if ! listens_ok; then
  diag "sshd not listening on 0.0.0.0:22 / ${ip}:22"
  dump_diag
  exit 1
fi

echo VIBEBOX_SSH_READY
echo "VIBEBOX_IPV4=$ip"
