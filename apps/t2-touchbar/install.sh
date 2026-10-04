#!/usr/bin/env bash
set -Eeuo pipefail

APP_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
source "$APP_DIR/../../scripts/fedora/lib.sh"

require_root
require_repo_root
require_fedora
require_command dnf sudo systemctl udevadm getent groupadd usermod

target_user="${SUDO_USER:-}"
[[ -n "$target_user" && "$target_user" != root ]] ||
	fail "run this installer through sudo as the desktop user"
target_uid=$(id -u "$target_user")

user_systemctl() {
	local runtime="/run/user/$target_uid"
	[[ -S "$runtime/bus" ]] || return 1
	sudo -H -u "$target_user" env \
		XDG_RUNTIME_DIR="$runtime" \
		DBUS_SESSION_BUS_ADDRESS="unix:path=$runtime/bus" \
		systemctl --user "$@"
}

device_group_is_live() {
	local group_id manager_pid

	group_id=$(getent group kait2en-touchbar | cut -d: -f3) || return 1
	[[ "$group_id" =~ ^[0-9]+$ ]] || return 1
	manager_pid=$(systemctl show "user@$target_uid.service" --property=MainPID --value 2>/dev/null) || return 1
	[[ "$manager_pid" =~ ^[1-9][0-9]*$ && -r "/proc/$manager_pid/status" ]] || return 1
	awk -v wanted="$group_id" '
		$1 == "Groups:" {
			for (field = 2; field <= NF; field++)
				if ($field == wanted)
					found = 1
		}
		END { exit !found }
	' "/proc/$manager_pid/status"
}

user_manager_is_running() {
	systemctl is-active --quiet "user@$target_uid.service"
}

clean_build() {
	clean_cargo_build "$APP_DIR" "$target_user"
}

install_dependencies() {
	dnf install -y adwaita-sans-fonts cargo gcc libinput-devel make pkgconf-pkg-config rust systemd-devel
	require_command cargo make
}

prepare_device_access() {
	getent group kait2en-touchbar >/dev/null || groupadd --system kait2en-touchbar
	usermod -aG kait2en-touchbar "$target_user"
}

build_and_install() {
	if ! sudo -H -u "$target_user" make -C "$APP_DIR" build; then
		clean_build
		return 1
	fi
	if ! make -C "$APP_DIR" install; then
		clean_build
		return 1
	fi
	clean_build
}

configure_system() {
	if systemctl list-unit-files tiny-dfr.service --no-legend 2>/dev/null | grep -q tiny-dfr; then
		systemctl disable --now tiny-dfr.service || warn "could not stop system tiny-dfr.service"
	fi
	if user_systemctl is-enabled tiny-dfr.service &>/dev/null ||
		user_systemctl is-active tiny-dfr.service &>/dev/null; then
		user_systemctl disable --now tiny-dfr.service || warn "could not stop user tiny-dfr.service"
	fi
	if user_systemctl is-enabled react-drm.service &>/dev/null ||
		user_systemctl is-active react-drm.service &>/dev/null; then
		user_systemctl disable --now react-drm.service || warn "could not stop react-drm.service"
	fi

	systemctl daemon-reload
	systemctl enable kait2en-touchbar-attach.service
	udevadm control --reload
	udevadm trigger --subsystem-match=usb --attr-match=idVendor=05ac --attr-match=idProduct=8102 --action=add
	udevadm trigger --subsystem-match=usb --attr-match=idVendor=05ac --attr-match=idProduct=8302 --action=add
	systemctl restart kait2en-touchbar-attach.service
	udevadm settle
	udevadm trigger --subsystem-match=drm --action=add
	udevadm trigger --subsystem-match=input --action=add
	udevadm trigger --subsystem-match=backlight --action=add
	udevadm trigger --subsystem-match=hid --action=add
	udevadm trigger --subsystem-match=misc --action=add
	systemctl --global enable kait2en-touchbar.service
}

activate_user_service() {
	if ! user_manager_is_running; then
		info "kait2en-touchbar will start at the next graphical login"
	elif ! device_group_is_live; then
		# A logout does not necessarily restart user@UID.service. Never leave the
		# panel in DRM configuration 2 while that manager still lacks access.
		if [[ -S "/run/user/$target_uid/bus" ]]; then
			user_systemctl stop kait2en-touchbar.service || true
		fi
		/usr/local/bin/kait2en-touchbar --detach
		warn "reboot once so kait2en-touchbar receives its device-access group; the firmware Touch Bar remains available until then"
	elif [[ -S "/run/user/$target_uid/bus" ]] &&
		user_systemctl is-active --quiet graphical-session.target; then
		user_systemctl daemon-reload
		user_systemctl restart kait2en-touchbar.service
	else
		info "kait2en-touchbar will start at the next graphical login"
	fi
}

run_step "install kait2en-touchbar dependencies" install_dependencies
if (( STEP_STATUS == 0 )); then
	run_step "prepare kait2en-touchbar device access" prepare_device_access
fi
if (( STEP_STATUS == 0 )); then
	run_step "build and install kait2en-touchbar" build_and_install
fi
if (( STEP_STATUS == 0 )); then
	run_step "configure kait2en-touchbar integration" configure_system
fi
if (( STEP_STATUS == 0 )); then
	run_step "activate kait2en-touchbar user service" activate_user_service
fi
if (( STEP_STATUS == 0 )); then
	info "kait2en-touchbar installed"
else
	exit "$STEP_STATUS"
fi
