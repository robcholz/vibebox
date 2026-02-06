#!/usr/bin/env bash
set -euo pipefail

APP=vibebox
REPO_URL="https://github.com/opencode-ai/vibebox"
REQUESTED_VERSION=${VERSION:-}

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
ORANGE='\033[38;2;255;140;0m'
NC='\033[0m' # No Color

print_message() {
  local level=$1
  local message=$2
  local color=""

  case $level in
    info) color="${GREEN}" ;;
    warning) color="${YELLOW}" ;;
    error) color="${RED}" ;;
  esac

  echo -e "${color}${message}${NC}"
}

require_cmd() {
  local cmd=$1
  local hint=$2

  if ! command -v "$cmd" >/dev/null 2>&1; then
    print_message error "Missing required command: ${cmd}"
    if [[ -n "$hint" ]]; then
      print_message info "$hint"
    fi
    exit 1
  fi
}

require_cmd cargo "Install Rust (cargo) from https://rustup.rs and retry."
require_cmd git "Install git and retry."

CARGO_HOME=${CARGO_HOME:-$HOME/.cargo}
INSTALL_DIR="$CARGO_HOME/bin"

installed_version=""
if command -v "$APP" >/dev/null 2>&1; then
  installed_version=$("$APP" --version 2>/dev/null | awk '{print $2}' | head -n1 || true)
fi

if [[ -n "$REQUESTED_VERSION" && "$installed_version" == "$REQUESTED_VERSION" ]]; then
  print_message info "Version ${YELLOW}$REQUESTED_VERSION${GREEN} already installed"
  exit 0
fi

install_args=(install "$APP" --locked --git "$REPO_URL")
if [[ -n "$REQUESTED_VERSION" ]]; then
  install_args+=(--tag "v$REQUESTED_VERSION")
fi

if command -v "$APP" >/dev/null 2>&1; then
  install_args+=(--force)
fi

print_message info "Installing ${ORANGE}${APP}${GREEN}..."
print_message info "Using cargo install from ${ORANGE}${REPO_URL}${GREEN}..."

cargo "${install_args[@]}"

add_to_path() {
  local config_file=$1
  local command=$2

  if [[ -w $config_file ]]; then
    echo -e "\n# vibebox" >> "$config_file"
    echo "$command" >> "$config_file"
    print_message info "Added ${ORANGE}${APP}${GREEN} to \$PATH in $config_file"
  else
    print_message warning "Manually add the directory to $config_file (or similar):"
    print_message info "  $command"
  fi
}

XDG_CONFIG_HOME=${XDG_CONFIG_HOME:-$HOME/.config}
current_shell=$(basename "$SHELL")

case $current_shell in
  fish)
    config_files="$HOME/.config/fish/config.fish"
  ;;
  zsh)
    config_files="$HOME/.zshrc $HOME/.zshenv $XDG_CONFIG_HOME/zsh/.zshrc $XDG_CONFIG_HOME/zsh/.zshenv"
  ;;
  bash)
    config_files="$HOME/.bashrc $HOME/.bash_profile $HOME/.profile $XDG_CONFIG_HOME/bash/.bashrc $XDG_CONFIG_HOME/bash/.bash_profile"
  ;;
  ash)
    config_files="$HOME/.ashrc $HOME/.profile /etc/profile"
  ;;
  sh)
    config_files="$HOME/.ashrc $HOME/.profile /etc/profile"
  ;;
  *)
    config_files="$HOME/.bashrc $HOME/.bash_profile $XDG_CONFIG_HOME/bash/.bashrc $XDG_CONFIG_HOME/bash/.bash_profile"
  ;;
esac

config_file=""
for file in $config_files; do
  if [[ -f $file ]]; then
    config_file=$file
    break
  fi
done

if [[ -z $config_file ]]; then
  print_message error "No config file found for $current_shell. Checked files: ${config_files[@]}"
  exit 1
fi

if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
  case $current_shell in
    fish)
      add_to_path "$config_file" "fish_add_path $INSTALL_DIR"
    ;;
    *)
      add_to_path "$config_file" "export PATH=$INSTALL_DIR:\$PATH"
    ;;
  esac
fi

if [ -n "${GITHUB_ACTIONS-}" ] && [ "${GITHUB_ACTIONS}" == "true" ]; then
  echo "$INSTALL_DIR" >> "$GITHUB_PATH"
  print_message info "Added $INSTALL_DIR to \$GITHUB_PATH"
fi
