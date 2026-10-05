#!/usr/bin/env bash

source "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)/lib.sh"

require_root
require_repo_root
require_fedora
require_command systemctl sed grep chronyd

CHRONY_CONF="/etc/chrony.conf"
BEGIN_MARK="# BEGIN Kait2en SMC RTC"
END_MARK="# END Kait2en SMC RTC"

# The kernel's NTP RTC sync (chrony rtcsync) never writes t2smc on x86, it
# stops at the legacy CMOS clock. Let chrony set rtc0 itself instead. It only
# trims the RTC while synchronized and tracks the SMC clock drift. t2smc itself
# synchronizes the RTC before suspend and at shutdown.
configure_chrony() {
	[[ -f "$CHRONY_CONF" ]] || fail "$CHRONY_CONF not found"
	sed -i 's/^rtcsync\b/#&/' "$CHRONY_CONF"
	sed -i "/^$BEGIN_MARK\$/,/^$END_MARK\$/d" "$CHRONY_CONF"
	cat >>"$CHRONY_CONF" <<EOF
$BEGIN_MARK
rtcdevice /dev/rtc0
rtcfile /var/lib/chrony/rtc
rtconutc
rtcautotrim 30
$END_MARK
EOF
	systemctl try-restart chronyd.service
}

run_step "configure chrony to maintain the T2 SMC RTC" configure_chrony
