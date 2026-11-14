#!/bin/bash
set -euo pipefail

KVER="${1:-$(uname -r)}"
ASSETS_DIR="${T2_UNLOCK_ASSET_DIR:-/usr/local/libexec/kait2en/touchid-unlock}"
SRC_DIR="$ASSETS_DIR/source"

if [ -z "${KDIR:-}" ]; then
    if [ -d "/lib/modules/$KVER/build" ]; then
        KDIR="/lib/modules/$KVER/build"
    elif [ -d "/run/booted-system/kernel/lib/modules/$KVER/build" ]; then
        KDIR="/run/booted-system/kernel/lib/modules/$KVER/build"
    elif [ -d "/run/current-system/kernel/lib/modules/$KVER/build" ]; then
        KDIR="/run/current-system/kernel/lib/modules/$KVER/build"
    else
        KDIR="/lib/modules/$KVER/build"
    fi
fi

if [ ! -d "$KDIR" ]; then
    echo "t2-touchid-unlock: kernel headers for $KVER not found at $KDIR" >&2
    exit 1
fi

# Locate sources: check installed source dir, then repository fallback
if [ ! -f "$SRC_DIR/Makefile" ]; then
    SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    REPO_SRC="$(cd "$SCRIPT_DIR/../../../../modules/t2sep" 2>/dev/null && pwd || true)"
    if [ -n "$REPO_SRC" ] && [ -f "$REPO_SRC/Makefile" ]; then
        SRC_DIR="$REPO_SRC"
    else
        echo "t2-touchid-unlock: t2sep source directory not found" >&2
        exit 1
    fi
fi

TMP_BUILD=$(mktemp -d /tmp/t2sep-build-XXXXXX)
trap 'rm -rf "$TMP_BUILD"' EXIT

cp -a "$SRC_DIR"/Makefile "$SRC_DIR"/*.c "$SRC_DIR"/*.h "$TMP_BUILD/" 2>/dev/null || true

# Locate signing key and cert
SIGN_FILE="$KDIR/scripts/sign-file"
SIGN_KEY="${SIGN_KEY:-}"
SIGN_CERT="${SIGN_CERT:-}"

if [ -z "$SIGN_KEY" ]; then
    for key in "$SRC_DIR/signing_key.pem" \
               /etc/kait2en/signing_key.pem; do
        if [ -f "$key" ]; then
            SIGN_KEY="$key"
            break
        fi
    done
fi

if [ -z "$SIGN_CERT" ]; then
    for cert in "$SRC_DIR/signing_key.x509" \
                "$SRC_DIR/cert.pem" \
                /etc/kait2en/signing_key.x509 \
                /etc/kait2en/cert.pem; do
        if [ -f "$cert" ]; then
            SIGN_CERT="$cert"
            break
        fi
    done
fi

make -C "$KDIR" M="$TMP_BUILD" clean >/dev/null 2>&1 || true
make -C "$KDIR" M="$TMP_BUILD" modules >/dev/null 2>&1

if [ ! -f "$TMP_BUILD/t2sep.ko" ]; then
    echo "t2-touchid-unlock: failed to compile t2sep.ko for $KVER" >&2
    exit 1
fi

SIGNED=0
if [ -x "$SIGN_FILE" ] && [ -n "$SIGN_KEY" ] && [ -n "$SIGN_CERT" ]; then
    "$SIGN_FILE" sha256 "$SIGN_KEY" "$SIGN_CERT" "$TMP_BUILD/t2sep.ko"
    SIGNED=1
else
    echo "t2-touchid-unlock: warning: could not sign t2sep.ko (place key and cert in /etc/kait2en/signing_key.{pem,x509})" >&2
fi

DEST_DIR="$ASSETS_DIR/$KVER"
mkdir -p "$DEST_DIR"
cp "$TMP_BUILD/t2sep.ko" "$DEST_DIR/t2sep.ko"
chmod 0644 "$DEST_DIR/t2sep.ko"

# Copy helper binaries if available
for helper in t2-keybag-unlock sks-lock-state; do
    if [ -f "$ASSETS_DIR/$helper" ]; then
        cp "$ASSETS_DIR/$helper" "$DEST_DIR/$helper"
        chmod 0700 "$DEST_DIR/$helper"
    fi
done

# If building for the running kernel, also update the top-level fallback
if [ "$KVER" = "$(uname -r)" ]; then
    cp "$TMP_BUILD/t2sep.ko" "$ASSETS_DIR/t2sep.ko"
    chmod 0644 "$ASSETS_DIR/t2sep.ko"
fi

if [ "$SIGNED" -eq 1 ]; then
    echo "T2: prepared signed t2sep module for kernel $KVER"
else
    echo "T2: prepared unsigned t2sep module for kernel $KVER (signing key not found in /etc/kait2en/)"
fi
