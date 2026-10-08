# t2sep

Experimental PCI mailbox transport for Apple T2 SEP (`106b:1802`). The optional
[manual Touch ID command](../../t2-services/t2-touchid/unlock/README.md) uses it
for AppleKeyStore keybag unlock; ordinary fingerprint matching continues over
the existing BCE/NCM BridgeXPC link.

`t2sep` is excluded from the default DKMS installation, has `AUTOINSTALL=no`,
and has no `MODULE_DEVICE_TABLE`. It does not autoload. A plain explicit
`insmod t2sep.ko` maps the mailbox and exposes status without sending requests.
Active legacy IOP/testing/control/discovery probes are rejected before PCI
setup. Use the coordinated manual command for the narrow experimental unlock
mode, rather than loading active flags independently.

## Transport invariants

The manual mode requires `manual_unlock_trial=1 register_ool=1
probe_capabilities=1`, with all other probe/start flags off. It performs a tagged
EP0 NOP and registers two 16 KiB buffers with a validated 34-bit coherent DMA
range. `/dev/t2sep` exposes `T2SEP_IOC_EXCHANGE` for bounded EP7 requests.
Capabilities must be integrity-checked before any user keybag/password request.

Control replies match endpoint, tag, operation and target. AKS replies match
endpoint, operation and transaction. Send/receive/unrelated replies consume one
absolute monotonic five-second deadline; polling uses actual elapsed time after
each wakeup. This bounds waiting, not scheduler latency or DMA ownership.

After any posted registration error the buffers are retained. Ambiguous native
exchanges poison the transport and retain potentially firmware-visible DMA;
completed exchanges wipe input/output payloads. **There is no established safe
unregister: registration pins the module until reboot. Never force-unload,
unbind or reset it. Cross-OS DMA handoff remains unproven.**

An explicit manual keybag unlock succeeded on one MacBookPro16,1 with subsequent
enrolled-finger recognition. Earlier experiments caused a whole-machine
shutdown and a reported SEPD/MDMA panic. See the manual README for the exact
validation scope and the once-per-boot coordinator; this is opt-in experimental
support, not a default boot-time SEP initializer.

## Build and offline checks

```sh
make KDIR=/path/to/prepared/running-kernel-tree
make test-wire test-mailbox
```

`test-wire` checks DMA boundaries and EP0/EP7 correlation. The Rust
`test-mailbox` runner compiles the production polling C with a fake clock/FIFO and covers ordering, oversleep,
expired-ready descriptors and shared deadlines. These tests do not touch hardware.
Sign the built module using the running kernel's trusted signing mechanism
before installing it in the manual command's per-kernel artifact directory.
No module autoload installation is required for the manual command. The existing kernel, initramfs and GRUB configuration are not
changed by this build.
