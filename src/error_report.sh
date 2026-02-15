#!/usr/bin/env bash
set -Eeuo pipefail
__vibebox_err_reported=0
__vibebox_report_error() {
  local rc="$1"
  local line="$2"
  local msg="${3:-}"
  if [ "$__vibebox_err_reported" -eq 0 ]; then
    msg="${msg//$'\n'/ }"
    msg="${msg//$'\r'/ }"
    if [ -n "$msg" ]; then
      echo "VIBEBOX_SCRIPT_ERROR:__LABEL__:${line}:${rc} ${msg}"
    else
      echo "VIBEBOX_SCRIPT_ERROR:__LABEL__:${line}:${rc}"
    fi
    __vibebox_err_reported=1
  fi
}
vibebox_fail() {
  local msg="${1:-script failed}"
  local rc="${2:-1}"
  __vibebox_report_error "$rc" "${LINENO}" "$msg"
  exit "$rc"
}
trap 'rc="$?"; __vibebox_report_error "$rc" "${LINENO}" "command failed: ${BASH_COMMAND:-unknown}"' ERR
trap 'rc="$?"; if [ "$rc" -ne 0 ]; then __vibebox_report_error "$rc" "${LINENO}" "script exited with code ${rc}"; fi' EXIT

__SCRIPT_BODY__
