#!/usr/bin/env bash

set -Eeuo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)
work=$(mktemp -d "${TMPDIR:-/tmp}/kait2en-bt-rom-test.XXXXXX")
trap 'rm -rf "$work"' EXIT

if ! command -v cc >/dev/null 2>&1 || ! command -v rustc >/dev/null 2>&1; then
	printf 'Skipping bluetooth-bcm4364-rom test: cc and rustc are required.\n'
	exit 0
fi

patch_file="$repo_root/patches/runtime/bluetooth-bcm4364-rom/0001-Bluetooth-hci_event-normalize-Broadcom-ROM-advertising.patch"
[[ -f "$patch_file" ]] || {
	printf 'Missing patch file: %s\n' "$patch_file" >&2
	exit 1
}

bin="$work/bluetooth-bcm4364-rom"
rustc --edition=2021 -O "$repo_root/scripts/tests/bluetooth-bcm4364-rom.rs" -o "$bin"

# 1. Test repository patch directly
"$bin" "$patch_file" >/dev/null

# 2. Test self-test canonical block
"$bin" --self-test >/dev/null

# 3. Test auto-discovery of repo patch when run from repo root
(cd "$repo_root" && "$bin" >/dev/null)

# 4. Test negative case: missing normalization must fail
cat << 'C_EOF' > "$work/missing.c"
static void hci_le_adv_report_evt(struct hci_dev *hdev) {
	/* no normalization here */
}
static void other_func(void) {}
C_EOF

if "$bin" "$work/missing.c" >/dev/null 2>&1; then
	printf 'Expected failure on unpatched C file, but test succeeded!\n' >&2
	exit 1
fi

printf 'Bluetooth BCM4364 ROM tests passed.\n'
