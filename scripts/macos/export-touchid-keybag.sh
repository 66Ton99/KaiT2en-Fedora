#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Read-only macOS export for one account. Compatible with macOS Bash 3.2.
set -euo pipefail
umask 077
[[ $(uname -s) == Darwin ]] || { echo "Run this script in macOS." >&2; exit 1; }
[[ $# == 1 ]] || { echo "Usage: bash $0 PRIVATE_OUTPUT_DIRECTORY" >&2; exit 2; }
account=$(stat -f '%Su' /dev/console)
[[ $account != root && $account != loginwindow ]] || { echo "Log into your macOS account first." >&2; exit 1; }
uid=$(dscl . -read "/Users/$account" UniqueID | awk '{print $2}')
uuid=$(dscl . -read "/Users/$account" GeneratedUID | awk '{print toupper($2)}')
[[ $uid =~ ^[0-9]+$ && $uuid =~ ^[A-F0-9-]+$ && ${#uuid} == 36 ]] || exit 1
[[ ! -e $1 && ! -L $1 ]] || { echo "Output directory must not exist." >&2; exit 1; }
mkdir -m 700 "$1"
output=$(cd "$1" && pwd -P)
[[ $(stat -f '%Lp' "$output") == 700 && $(stat -f '%u' "$output") == $(id -u) ]] || {
    echo "Output filesystem cannot provide private permissions." >&2; exit 1;
}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
sudo -v
for root in /System/Volumes/Preboot /private/var /Library /Users; do
    [[ -d $root ]] || continue
    sudo find "$root" -xdev -type f -iname user.kb -size -16M -print 2>/dev/null || true
done | sort -u > "$work/candidates"
count=0
selected=
while IFS= read -r candidate; do
    upper=$(printf '%s' "$candidate" | tr '[:lower:]' '[:upper:]')
    # The account UUID must be an entire path component, not an APFS volume ID.
    case "$upper" in
        */"$uuid"/*) count=$((count + 1)); selected=$candidate ;;
    esac
done < "$work/candidates"
[[ $count == 1 ]] || {
    echo "No unique user.kb path for this account was found; no keybag was exported." >&2
    echo "Manual private inspection is needed; do not send keybag files in chat." >&2
    exit 1
}
sudo cat "$selected" > "$output/user.kb"
chmod 600 "$output/user.kb"
printf '%s\n' "$uid" > "$output/macos-uid.txt"
[[ $(stat -f '%Lp' "$output/user.kb") == 600 ]] || exit 1
size=$(stat -f '%z' "$output/user.kb")
[[ $size -gt 0 && $size -le 16000 ]] || { rm -f "$output/user.kb"; echo "Unexpected keybag size." >&2; exit 1; }
echo "Exported user.kb and macos-uid.txt into the private output directory."
echo "Transfer privately to Linux; do not commit these files or upload them to this chat."
