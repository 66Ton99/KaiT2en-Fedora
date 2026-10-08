# SEP transport contract and lifecycle

The implementation uses KaiT2en's existing PCI SEP driver and the existing
BridgeXPC/BiometricKit bridge. Related protocol work is available in
[jmurth1234/t2-touchid-linux](https://github.com/jmurth1234/t2-touchid-linux).
No Apple firmware images, extracted executables, user keybags or account
credentials are distributed in this repository.

## Native contract

* PCI `106b:1802`, mailbox BAR4; coherent 16 KiB OOL buffers, 34-bit DMA mask.
  The entire allocation must fit the mask, not only its first page.
* EP0 NOP/OOL acknowledgements must match the control endpoint, nonzero tag,
  operation and target. A stale or mismatched descriptor cannot establish
  readiness. EP7 acknowledgements must match endpoint, operation, transaction
  and success status.
* Every locked native exchange uses one absolute monotonic five-second deadline
  for send, receive and ignored descriptors. A delayed scheduler wake does not
  allow another MMIO operation after expiry.
* AKS requests use an 84-byte (`0x50` after the leading size word) v2 header and
  truncated SHA-256 integrity field. The observed mixed `0x50`/v1 response
  excludes the calendar bytes from its digest; v2 includes them.
* Capability operation `0x4d` must indicate version 2. The user-space sequence is
  load `0x03`, account alias `0x0d`, then unlock `0x04` for both the loaded handle
  and the negative macOS UID alias. A timeout, rejected status, malformed length
  or bad digest never counts as success.

## Ownership and failure handling

No safe OOL unregister/rollback acknowledgement has been established. Even a
negative registration reply may follow a posted address. After a posted
ambiguous failure the driver keeps DMA memory and refuses subsequent exchanges;
it must not free storage still potentially visible to SEP. Completed request
and response storage is wiped. The module remains pinned after registration.

Earlier active tests caused a shutdown and a reported SEPD/MDMA panic. Legacy
IOP start/testing/control/discovery combinations are rejected before PCI setup.
The only active opt-in is `manual_unlock_trial=1 register_ool=1
probe_capabilities=1`, with all legacy flags off. No reset or active IOP start is
used. Keeping Linux memory allocated does not prove a safe transition to a new
OS, which remains an explicit unresolved lifecycle limit.

The successful manual trial and its scope are documented in the
[manual command README](../README.md). The kernel module and original C exchange sequence
were tested on hardware. The current Rust client preserves those request bytes,
integrity rules and ioctl ABI, verified by offline fixtures; later attempts using the Rust coordinator reached an EP7 capability timeout
after successful EP0/OOL registration, before the private Rust client ran.
One timeout recurred after a regular Linux reboot with the network link ready
and without biometric masks. The missing response remains unexplained; neither
reboot nor CDC-NCM readiness is a demonstrated native SEP recovery. The private
Rust client's unlock path remains verified offline only.

## Discovery ordering and stale peer state

The original project's [bring-up report](https://github.com/jmurth1234/t2-touchid-linux/blob/ea46d8a0aef3e73b0e2f747aa18721dbcd265bce/docs/LINUX_BRINGUP_TROUBLESHOOTING.md)
reports a persistent EP7 timeout caused by overlapping RemoteXPC discovery and
native negotiation, and macOS-mediated recovery on MacBookPro15,2/23P350.
Our failed attempts do not establish that identical race on MacBookPro16,1.
The manual coordinator now finishes direct RSD discovery before native setup,
sends no BiometricKit SKS request before capabilities, and reuses the discovered
port for its post-unlock read. Discovery failure or an invalid port stops before
`insmod`. This sequencing change is tested offline; the kernel wire format,
DMA limits, mailbox timeout and no-retry policy remain unchanged. It does not
attempt to repair an already failed or retained native transport.

## Subsequent SEPD/MDMA panic

After the two EP7 timeouts, the user reported a panic on the first macOS boot.
The supplied log identifies T2 SEPOS `3151.161.2` and an ARM64_T8010 kernel, with
`SEP Panic: :SEPD/MDMA: 0x0000b659 0x0003c3b7 0x0000a123 0x000095d1
0x00003317 0x0000343b [rmlp]`. The subsystem matches the earlier reported panic;
these undocumented status words do not identify which Linux operation caused it.
The temporal association is evidence to investigate, not proof of causality.
The log's Boot/Calendar fields span 6,613 seconds of T2 uptime; this is not a
measurement of the macOS host's startup duration.

The user subsequently confirmed working Touch ID. In the next regular Linux
boot on the same BLE kernel, the existing bridge logged an enrolled-finger match
with no native module or exchange device present. This establishes restored
fingerprint operation, not validation of the new Rust native sequence or safe
cross-OS DMA ownership. No additional native request was made. Raw panic logs
and machine-specific reports remain private and outside source control.

## Cancellation after confirmed capabilities

A later discovery-only Rust trial confirmed the initial native capability reply
and the private client's capability check, then reached the hidden macOS password
prompt. The user explicitly cancelled input. No keybag load/alias/unlock was
sent. The old coordinator treated every nonzero client exit as an ambiguous
native outcome, leaving the previously active bridge/fprintd blocked.

The private client now reserves exit status 20 solely for password-input errors
before any keybag operation, after its successful capability exchange. The
coordinator preserves this status even when a parent interrupt initiates child
cleanup, records `keybag_operation_not_sent`, and restores prior biometric
service activity. Killed children, timeout and other errors provide no such
proof. Native DMA and the once-per-boot marker are never released for a retry.
The cleanup distinction is verified offline with terminal, subprocess-signal and
service-lifecycle tests; a full Rust native keybag unlock is still not validated.
