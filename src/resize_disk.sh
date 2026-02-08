#!/bin/bash
set -euo pipefail

ROOT_DEV="$(findmnt / -n -o SOURCE || true)"
ROOT_FSTYPE="$(findmnt / -n -o FSTYPE || true)"

if [ -z "$ROOT_DEV" ]; then
  exit 0
fi

DISK_DEV=""
PART_NUM=""

if command -v lsblk >/dev/null 2>&1; then
  DISK_DEV="$(lsblk -no pkname "$ROOT_DEV" 2>/dev/null | head -n1 || true)"
  PART_NUM="$(lsblk -no PARTNUM "$ROOT_DEV" 2>/dev/null | head -n1 || true)"
fi

if [ -z "$DISK_DEV" ] || [ -z "$PART_NUM" ]; then
  ROOT_BASENAME="$(basename "$ROOT_DEV")"
  if echo "$ROOT_BASENAME" | grep -Eq '^nvme.+p[0-9]+$'; then
    DISK_DEV="/dev/${ROOT_BASENAME%p[0-9]*}"
    PART_NUM="${ROOT_BASENAME##*p}"
  elif echo "$ROOT_BASENAME" | grep -Eq '^[a-z]+[0-9]+$'; then
    DISK_DEV="/dev/${ROOT_BASENAME%%[0-9]*}"
    PART_NUM="${ROOT_BASENAME##*[a-z]}"
  fi
fi

if [ -n "$DISK_DEV" ] && [ -n "$PART_NUM" ]; then
  if command -v growpart >/dev/null 2>&1; then
    growpart "$DISK_DEV" "$PART_NUM" || true
  elif command -v sfdisk >/dev/null 2>&1; then
    sfdisk -N "$PART_NUM" --force "$DISK_DEV" <<'EOF' || true
,,
EOF
  fi
fi

if command -v partprobe >/dev/null 2>&1; then
  partprobe "$DISK_DEV" || true
fi

case "$ROOT_FSTYPE" in
  ext4|ext3|ext2)
    if command -v resize2fs >/dev/null 2>&1; then
      resize2fs "$ROOT_DEV" || true
    fi
    ;;
  xfs)
    if command -v xfs_growfs >/dev/null 2>&1; then
      xfs_growfs / || true
    fi
    ;;
esac
