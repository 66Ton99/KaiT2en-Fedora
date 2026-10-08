# BCM4364B3 ROM firmware: legacy BLE advertising reports

Broadcom ROM firmware on Apple T2 Macs can set vendor bits in legacy LE
Advertising Report `Event_Type`. The standard handler discards values such as
`0x13`, `0x14`, `0x20` and `0x24`, which can interrupt peripheral discovery or
Chrome's Android phone passkey proximity scan.

The patch in `series` adapts the
[t2linux patch](https://github.com/t2linux/linux-t2-patches/blob/main/9002-Bluetooth-hci_event-strip-Broadcom-vendor-bits-from-adv-type.patch)
by Jose Miguel Ochoa to the UEK 6.12 handler context. It strips upper bits only
for out-of-range Broadcom (manufacturer 15) legacy event types. Valid types,
other vendors, RSSI and advertisement data are unchanged; invalid low nibbles
remain invalid. This does not change UART speed or install firmware.

## Apply to a separate kernel build

Download the current kernel sources for your distribution and use a separate
build/output directory and a distinct kernel release suffix. From this
repository, with `KERNEL_SOURCE` pointing to the extracted kernel tree:

```sh
patch_dir=$(realpath patches/runtime/bluetooth-bcm4364-rom)
git -C "$KERNEL_SOURCE" apply --check "$patch_dir/0001-Bluetooth-hci_event-normalize-Broadcom-ROM-advertising.patch"
git -C "$KERNEL_SOURCE" apply "$patch_dir/0001-Bluetooth-hci_event-normalize-Broadcom-ROM-advertising.patch"
rustc --edition=2024 scripts/tests/bluetooth-bcm4364-rom.rs -o /tmp/bluetooth-bcm4364-rom
/tmp/bluetooth-bcm4364-rom "$KERNEL_SOURCE/net/bluetooth/hci_event.c"
```

If the current source already contains the fix, do not apply it twice. If its
handler context differs, adapt the seven-line normalization block to the legacy
advertising handler and rerun the fixture. Build/package using the distribution's
normal process, rebuild the external T2 modules for the new release, and retain
the working kernel/boot entry while testing. The repository does not overwrite
an installed kernel or modify GRUB as part of this patch.

The standalone Rust tool uses only the standard library, rustc and a C compiler.
It extracts and compiles the actual patched C statements, then calls that object
from a Rust fixture. Rust assertions cover all 256 event bytes for all 65,536
manufacturers, valid/invalid types, RSSI and the complete advertisement payload.
It makes no Bluetooth or kernel-module operation and requires no Python runtime.

## Hardware result

A MacBookPro16,1 with BCM4364B3 using ROM firmware was tested with Oracle UEK
`6.12.0-206.104.4.4.el10uek.t2bt.ble1`; the user reported good BLE operation.
After booting your build, repeat Android phone authentication with a fresh Chrome
QR code and inspect the kernel journal for unknown advertising types. The older
[LKML throughput report](https://lkml.iu.edu/hypermail/linux/kernel/2112.3/01793.html)
concerns a different UART speed/transfer issue and is not fixed by this patch.
