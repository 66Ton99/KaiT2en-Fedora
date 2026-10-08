CARGO ?= cargo
CARGO_FLAGS ?= --locked
PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin
SYSTEMD_UNIT_DIR ?= $(PREFIX)/lib/systemd/system
DATADIR ?= $(PREFIX)/share
SYSCONFDIR ?= /etc
TARGET_DIR ?= target

.PHONY: all build test install install-bin install-integration clean
all: build
build:
	$(CARGO) build --release $(CARGO_FLAGS) --target-dir $(TARGET_DIR)
test:
	$(CARGO) test $(CARGO_FLAGS) --target-dir $(TARGET_DIR)
install: install-bin install-integration
install-bin:
	install -D -m 0755 $(TARGET_DIR)/release/t2-touchid $(DESTDIR)$(BINDIR)/t2-touchid
install-integration:
	install -d $(DESTDIR)$(SYSTEMD_UNIT_DIR)
	sed 's|@BINDIR@|$(BINDIR)|g' integration/systemd/kait2en-t2-touchid.service >$(DESTDIR)$(SYSTEMD_UNIT_DIR)/kait2en-t2-touchid.service
	chmod 0644 $(DESTDIR)$(SYSTEMD_UNIT_DIR)/kait2en-t2-touchid.service
	install -D -m 0644 integration/fprintd/fprintd-kait2en-t2-touchid.conf $(DESTDIR)$(SYSTEMD_UNIT_DIR)/fprintd.service.d/kait2en-t2-touchid.conf
	install -D -m 0644 integration/dbus/org.kait2en.TouchId.conf $(DESTDIR)$(DATADIR)/dbus-1/system.d/org.kait2en.TouchId.conf
	install -d $(DESTDIR)$(SYSCONFDIR)/kait2en
	test -e $(DESTDIR)$(SYSCONFDIR)/kait2en/t2-touchid.conf || install -m 0644 config/t2-touchid.conf $(DESTDIR)$(SYSCONFDIR)/kait2en/t2-touchid.conf
clean:
	$(CARGO) clean --target-dir $(TARGET_DIR)

# Manual keybag support is deliberately excluded from the default install.
LIBEXECDIR ?= $(PREFIX)/libexec/kait2en
UNLOCK_CONFIG ?= $(SYSCONFDIR)/kait2en/touchid-unlock.json
UNLOCK_ASSET_DIR ?= $(LIBEXECDIR)/touchid-unlock
UNLOCK_DATA_DIR ?= /var/lib/kait2en/touchid-unlock
UNLOCK_KERNEL ?= $(shell uname -r)
UNLOCK_MODULE ?= ../../modules/t2sep/t2sep.ko
UNLOCK_ENV = T2_UNLOCK_CONFIG="$(UNLOCK_CONFIG)" T2_UNLOCK_ASSET_DIR="$(UNLOCK_ASSET_DIR)" T2_UNLOCK_DATA_DIR="$(UNLOCK_DATA_DIR)"
.PHONY: build-unlock build-unlock-rust build-unlock-sks test-unlock install-unlock
build-unlock: build-unlock-rust
build-unlock-rust:
	$(UNLOCK_ENV) $(CARGO) build --release $(CARGO_FLAGS) --manifest-path unlock/Cargo.toml --target-dir $(TARGET_DIR)/unlock-rust
build-unlock-sks:
	$(CARGO) build --release $(CARGO_FLAGS) --manifest-path protocols/t2-biometrickit/Cargo.toml --example sks-lock-state --target-dir $(TARGET_DIR)
test-unlock:
	$(UNLOCK_ENV) $(CARGO) test $(CARGO_FLAGS) --manifest-path unlock/Cargo.toml --target-dir $(TARGET_DIR)/unlock-rust
# Install a signed module for each kernel that should support the command.
# Keybag import and account configuration are explicit one-time setup.
# Native risk is acknowledged separately for each invocation.
install-unlock:
	test -f "$(UNLOCK_MODULE)"
	install -D -m 0755 $(TARGET_DIR)/unlock-rust/release/t2-touchid-unlock $(DESTDIR)$(BINDIR)/t2-touchid-unlock
	install -D -m 0700 $(TARGET_DIR)/unlock-rust/release/t2-keybag-unlock $(DESTDIR)$(UNLOCK_ASSET_DIR)/$(UNLOCK_KERNEL)/t2-keybag-unlock
	install -D -m 0700 $(TARGET_DIR)/release/examples/sks-lock-state $(DESTDIR)$(UNLOCK_ASSET_DIR)/$(UNLOCK_KERNEL)/sks-lock-state
	install -D -m 0644 "$(UNLOCK_MODULE)" $(DESTDIR)$(UNLOCK_ASSET_DIR)/$(UNLOCK_KERNEL)/t2sep.ko
	install -d -m 0700 $(DESTDIR)$(UNLOCK_DATA_DIR) $(DESTDIR)$(UNLOCK_DATA_DIR)/reports
	install -d $(dir $(DESTDIR)$(UNLOCK_CONFIG))
	test -e $(DESTDIR)$(UNLOCK_CONFIG) || install -m 0600 unlock/config/touchid-unlock.example.json $(DESTDIR)$(UNLOCK_CONFIG)

# Standalone network preparation also works when diagnostic boots mask biometrics.
LINK_READY_UNIT_DIR ?= $(SYSCONFDIR)/systemd/system
.PHONY: install-link-ready
install-link-ready:
	install -D -m 0755 $(TARGET_DIR)/unlock-rust/release/t2-touchid-link-ready $(DESTDIR)$(LIBEXECDIR)/t2-touchid-link-ready
	install -d $(DESTDIR)$(LINK_READY_UNIT_DIR)/kait2en-t2-touchid.service.d
	sed 's|@LIBEXECDIR@|$(LIBEXECDIR)|g' integration/systemd/kait2en-t2-link-ready.service >$(DESTDIR)$(LINK_READY_UNIT_DIR)/kait2en-t2-link-ready.service
	sed 's|@LIBEXECDIR@|$(LIBEXECDIR)|g' integration/systemd/touchid-link-ready.conf >$(DESTDIR)$(LINK_READY_UNIT_DIR)/kait2en-t2-touchid.service.d/link-ready.conf
	chmod 0644 $(DESTDIR)$(LINK_READY_UNIT_DIR)/kait2en-t2-link-ready.service $(DESTDIR)$(LINK_READY_UNIT_DIR)/kait2en-t2-touchid.service.d/link-ready.conf
