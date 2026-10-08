obj-m += t2sep.o
t2sep-y := t2sep_main.o t2sep_mailbox.o

KDIR ?= /lib/modules/$(shell uname -r)/build
KVER ?= $(shell uname -r)
PWD := $(shell pwd)
MODDEST ?= /lib/modules/$(KVER)/updates

all:
	$(MAKE) -C $(KDIR) M=$(PWD) modules

clean:
	$(MAKE) -C $(KDIR) M=$(PWD) clean
	find $(PWD) -type f \( -name '*.o' -o -name '*.ko' -o -name '*.mod' -o -name '*.mod.c' -o -name '.*.cmd' -o -name 'Module.symvers' -o -name 'modules.order' \) -delete

install: all
	install -d -o root -g root -m 0755 $(MODDEST)
	install -o root -g root -m 0644 t2sep.ko $(MODDEST)/
	depmod -a $(KVER)

.PHONY: all clean install

# Pure wire-contract tests; never load or contact SEP.
HOSTCC ?= cc
.PHONY: test-wire
test-wire:
	mkdir -p target
	$(HOSTCC) -O2 -Wall -Wextra -Werror test-aks-wire.c -o target/test-aks-wire
	./target/test-aks-wire

# Exercise the production polling code with a fake clock and FIFO; no MMIO.
.PHONY: test-mailbox
test-mailbox:
	mkdir -p target
	rustc --edition=2024 -D warnings tests/test-mailbox.rs -o target/mailbox-runner
	./target/mailbox-runner $(HOSTCC)
