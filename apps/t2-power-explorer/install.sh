#!/usr/bin/env bash
set -euo pipefail

APP_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
source "$APP_DIR/../../scripts/fedora/lib.sh"
require_root
install_kait2en_fonts
require_fedora
require_command dnf make runuser

dnf install -y cargo gcc gtk4-devel libadwaita-devel

target_user=${SUDO_USER:-}
[[ -n "$target_user" && "$target_user" != root ]] ||
	fail "t2-power-explorer must be built for the user who invoked sudo"
build_group=$(id -gn "$target_user")
if [[ -d "$APP_DIR/target" ]]; then
	chown -R "$target_user:$build_group" "$APP_DIR/target"
fi
runuser -u "$target_user" -- make -C "$APP_DIR" build
make -C "$APP_DIR" install
clean_cargo_build "$APP_DIR" "$target_user"
info "t2-power-explorer installed"
