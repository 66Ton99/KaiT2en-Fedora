CARGO ?= cargo
INSTALL ?= install
PREFIX ?= /usr/local
DESTDIR ?=

BIN := kait2en-touchbar
BIN_SRC := target/release/$(BIN)
BIN_DIR := $(DESTDIR)$(PREFIX)/bin
USER_UNIT_DIR := $(DESTDIR)$(PREFIX)/lib/systemd/user
SYSTEM_UNIT_DIR := $(DESTDIR)$(PREFIX)/lib/systemd/system
UDEV_DIR := $(DESTDIR)$(PREFIX)/lib/udev/rules.d
CONFIG_DIR := $(DESTDIR)/etc/kait2en
LICENSE_DIR := $(DESTDIR)$(PREFIX)/share/licenses/kait2en-touchbar

.PHONY: all build test check-build install uninstall clean
all: build

build:
	$(CARGO) build --release --locked

test:
	$(CARGO) test --locked

check-build:
	@test -x "$(BIN_SRC)" || { echo "error: $(BIN_SRC) is missing; run make build first" >&2; exit 1; }

install: check-build
	$(INSTALL) -d -m 0755 $(BIN_DIR) $(USER_UNIT_DIR) $(SYSTEM_UNIT_DIR) $(UDEV_DIR) $(CONFIG_DIR) $(LICENSE_DIR)
	$(INSTALL) -m 0755 $(BIN_SRC) $(BIN_DIR)/$(BIN)
	$(INSTALL) -m 0644 integration/systemd/user/kait2en-touchbar.service $(USER_UNIT_DIR)/kait2en-touchbar.service
	$(INSTALL) -m 0644 integration/systemd/system/kait2en-touchbar-attach.service $(SYSTEM_UNIT_DIR)/kait2en-touchbar-attach.service
	$(INSTALL) -m 0644 integration/udev/99-zz-kait2en-touchbar.rules $(UDEV_DIR)/99-zz-kait2en-touchbar.rules
	$(INSTALL) -m 0644 THIRD-PARTY-NOTICES.md $(LICENSE_DIR)/THIRD-PARTY-NOTICES.md
	@test -e $(CONFIG_DIR)/touchbar.toml || $(INSTALL) -m 0644 config/touchbar.toml $(CONFIG_DIR)/touchbar.toml
	rm -f $(DESTDIR)$(PREFIX)/lib/modprobe.d/kait2en-touchbar.conf

uninstall:
	rm -f $(BIN_DIR)/$(BIN)
	rm -f $(USER_UNIT_DIR)/kait2en-touchbar.service
	rm -f $(SYSTEM_UNIT_DIR)/kait2en-touchbar-attach.service
	rm -f $(UDEV_DIR)/99-zz-kait2en-touchbar.rules
	rm -f $(DESTDIR)$(PREFIX)/lib/modprobe.d/kait2en-touchbar.conf
	rm -f $(LICENSE_DIR)/THIRD-PARTY-NOTICES.md

clean:
	$(CARGO) clean
