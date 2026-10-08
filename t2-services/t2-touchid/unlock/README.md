# Manual Touch ID keybag unlock

This opt-in command unlocks the existing macOS user keybag through KaiT2en's
`t2sep` transport, then restores the installed Touch ID bridge and fprintd.
It uses the macOS login password once, entered privately on `/dev/tty`.
It does not replace libfprint, modify PAM, enroll fingerprints, install a boot
service, store a password, or reset the T2/USB controller.

## Validation and limits

On a MacBookPro16,1 running Oracle UEK
`6.12.0-206.104.4.4.el10uek.t2bt.ble1`, an explicit manual attempt completed:

* EP0 control negotiation and both 16 KiB OOL registrations;
* an integrity-checked EP7 capability reply (`0x2`);
* keybag load, account alias, and both unlock acknowledgements;
* restoration of the existing bridge/fprintd services, followed by a registered
  finger match and the user's confirmation that fingerprint authentication worked.

The read-only SKS value changed from `0x00000044` to `0x00000208`; these raw
values alone are not authentication proof. No unenrolled-finger control was
performed. Cold power-on initialization, repeat unlocks in one boot, other
models/firmware, and DMA handoff to another OS have not been established.
The Rust command and its private AKS client replace the
Python/C userspace implementation used for that trial. Rust wire fixtures match
the original C request bodies and digest vectors, and its ioctl layout is checked
against the kernel UAPI. These changes, packaging and PCI-path generalization
are verified offline. A later hardware attempt using the Rust coordinator and
identical signed module completed EP0/OOL setup but timed out waiting for the
initial EP7 capability reply (`-110`). The kernel retained DMA and disabled the
exchange device; the private Rust AKS client never ran, and no keybag or macOS
password operation was sent. The cause of the missing EP7 reply is not established;
this attempt does not validate the Rust client's native unlock path. After a
regular Linux reboot without biometric service masks, another attempt reached
the same EP7 timeout with the network readiness service active. A Linux reboot
is therefore not a demonstrated recovery of the peer's AKS state; a different
GRUB entry and working CDC-NCM do not guarantee native SEP negotiation.
A subsequent trial with the discovery-only Rust sequence completed the initial
capability negotiation and reached the private client's macOS password prompt.
The user cancelled that input; no keybag load, alias or unlock was sent. This
validates capability negotiation for that trial, not the full Rust unlock path
or proof that discovery ordering alone resolved the earlier timeouts.

Earlier native experiments caused a whole-machine shutdown and a reported
SEPD/MDMA panic. After the later EP7 timeouts, the first macOS boot produced
another SEPD/MDMA panic; Touch ID subsequently worked in macOS and in the regular
Linux boot without a native module. Successful fingerprint recognition afterward
does not establish safe DMA handoff or validate another native attempt.
Native exchanges remain experimental and require explicit
acknowledgement at each launch, either in an interactive confirmation or via
`--accept-risk`. Save open work beforehand.
The module pins registered DMA buffers until reboot. **Never force-unload,
unbind, reset, or retry the transport after an ambiguous native result.**

## Export the encrypted keybag

Log into the macOS account containing the enrolled fingerprints. In macOS,
from the repository root:

```sh
bash scripts/macos/export-touchid-keybag.sh "$HOME/touchid-export"
```

The output directory must not exist. The script reads the logged-in account's
UID/UUID and copies only its unique `user.kb`, with private permissions. Transfer
`user.kb` and `macos-uid.txt` privately to Linux. Neither file belongs in source
control or bug reports. The password is not exported. If the script cannot
identify exactly one matching file it stops for private manual inspection.

## Build for the running kernel

Use the current repository sources and a prepared kernel build tree matching
`uname -r`, including the kernel configuration and `Module.symvers`. From the
repository root:

```sh
make -C modules/t2sep KDIR="$KDIR"
"$KDIR/scripts/sign-file" sha256 "$SIGNING_KEY" "$SIGNING_CERT" modules/t2sep/t2sep.ko
make -C modules/t2sep test-wire test-mailbox
make -C t2-services/t2-touchid test-unlock build-unlock build-unlock-sks
```

A Rust toolchain supporting edition 2024, Cargo, a host C compiler for the
kernel-UAPI/offline mailbox fixtures, and kernel build tools are required.
`build-unlock` builds the public `t2-touchid-unlock` command and its private Rust
AKS client and private link-readiness helper using the committed
`unlock/Cargo.lock`. `build-unlock-sks` builds the
private read-only SKS helper. No Python runtime or userspace C client is needed.

Set `KDIR`, `SIGNING_KEY`, and `SIGNING_CERT` to your kernel's build tree and
trusted signing credentials. Keep private signing keys outside the repository.
The coordinator requires a signed module for the exact running kernel; the
kernel enforces its own signature policy. The existing T2 NCM link, Touch ID
bridge and fprintd integration must already be installed and working as a
transport. Only the native manual unlock is new here.

## One-time installation

Install the built command, private helpers and signed module:

```sh
sudo make -C t2-services/t2-touchid install-unlock
sudo install -m 0600 /private/export/user.kb /var/lib/kait2en/touchid-unlock/user.kb
sudoedit /etc/kait2en/touchid-unlock.json
```

Replace `/private/export` with your Linux export directory. Set `macos_uid` to
exactly the UID in the exported `macos-uid.txt`. The configuration contains only
account and file settings, for example:

```json
{
  "macos_uid": 501
}
```

Risk is confirmed at runtime, not stored in this file. If an older configuration
contains `accept_prior_shutdown_risk`, remove that field. For compatibility the
command accepts it with a deprecation warning but ignores its value: even `true`
cannot skip the runtime confirmation. The encrypted keybag, configuration and
report directory remain private to root. Passwords do not belong in the configuration.

Default installed paths:

| File | Path |
| --- | --- |
| Public Rust command | `/usr/local/bin/t2-touchid-unlock` |
| Account and file configuration | `/etc/kait2en/touchid-unlock.json` |
| Encrypted keybag | `/var/lib/kait2en/touchid-unlock/user.kb` |
| Signed module and private helpers | `/usr/local/libexec/kait2en/touchid-unlock/<uname -r>/` |
| Private snapshots and reports | `/var/lib/kait2en/touchid-unlock/reports/` |

The per-kernel artifact directory contains `t2sep.ko`, `t2-keybag-unlock` and
`sks-lock-state`; helpers are root-only executables. Install a signed matching
module in each kernel's directory before using the command on that kernel.
`UNLOCK_MODULE` and `UNLOCK_KERNEL` may override their locations at installation;
for example, `make install-unlock UNLOCK_MODULE=/path/to/signed/t2sep.ko`.
An optional absolute `keybag` path in the root-owned JSON overrides the default.
The exported file must remain private, owned by root or the calling user, with
no symbolic or hard links.

Custom installation prefixes must be used consistently for build and install.
`UNLOCK_CONFIG`, `UNLOCK_ASSET_DIR` and `UNLOCK_DATA_DIR` are compiled into the
Rust command by the Makefile; user-supplied environment variables cannot change
its runtime paths. `DESTDIR` stages installation without changing compiled
paths. Executing a binary inside `default-install-stage` still uses the real
`/etc`, `/usr/local/libexec` and `/var/lib` paths; staging does not make a portable
installation. Complete installation on the host before running it.
The normal `make install` and default DKMS installer do not install or
autoload this manual unlock support.

## Read-only preflight

```sh
t2-touchid-unlock --check-only
```

The command requests sudo authorization automatically when protected state
needs to be inspected. Local readiness of the Apple CDC-NCM link (administrative
UP, carrier and IPv6 link-local address) is also checked without sending packets.
A missing carrier fails before confirmation, snapshot creation or service changes.
On affected systems, prepare the already installed `t2_touchid_link` quirk first;
the unlock command never loads that module or resets USB automatically.
For persistent startup readiness, install the separate private Rust helper and
service described below.
An already present native module/device is refused
before sudo. This mode never loads a module, invokes the SKS/AKS helpers, asks
for the macOS password, writes a snapshot/report, or changes services. It does
not display a risk confirmation. `--check-only` and `--accept-risk` cannot be
combined.

`PRECHECK_REFUSED` with a nonzero exit status stops immediately when a native
transport is present, a previous attempt/guard remains, protected state cannot
be inspected, or another unlock holds the lock. Dangling links count as existing
state. The check also validates configured UID/keybag metadata, protected
installed directories/files, executable permissions and the module's signature
metadata and compatibility with the running kernel. Invalid installation or
configuration fails with a nonzero exit status.

`PRECHECK_PASS` means these local checks passed, not that SEP is unlocked or
that a native exchange cannot fail. No hardware state is queried. Normal unlock
repeats state checks under its exclusive lock before creating service guards,
and checks again before native initialization. Do not remove an attempt marker
or guard to get past a refusal.

## Installation errors

On failure, the command exits nonzero and identifies the failed operation and
path, a setup hint, and the original OS error. For example, a missing configuration
is reported as:

```text
t2-touchid-unlock: Cannot open manual-unlock configuration '/etc/kait2en/touchid-unlock.json': required file or directory is missing. Run 'sudo make -C t2-services/t2-touchid install-unlock' from the repository root, then configure this file.: No such file or directory (os error 2)
```

Missing installation directories point to `install-unlock`; missing per-kernel
artifacts name the running kernel and required directory. A missing keybag points
to the private `user.kb` import, and malformed JSON names the configuration file.
Permission failures retain their OS cause; review ownership, permissions and
security policy instead of making private files public. A command startup failure
also names the executable; `ENOENT` may refer to its missing interpreter or loader.
These setup failures stop before any native exchange. They do not trigger retries,
module replacement, automatic file creation or a password prompt for macOS.

## Persistent network readiness

On affected T2 systems the firmware's CDC network-connection notification may
be missing. The locally installed signed `t2_touchid_link` quirk recovers carrier
with `usbnet_link_change`, without resetting USB or talking to SEP. A preparation
hook attached only to the biometric bridge never runs if that bridge is masked
by a diagnostic boot; this leaves carrier down and SKS discovery times out.

Install the standalone preparation service after building the Rust helpers:

```sh
sudo make -C t2-services/t2-touchid install-link-ready
sudo systemctl daemon-reload
sudo systemctl enable --now kait2en-t2-link-ready.service
```

This requires the signed `t2_touchid_link` module to be installed for the running
kernel and the existing private IPv6 link-local NetworkManager profile. The
service runs independently of biometric service masks. It waits for the Apple
05ac:8233 CDC-NCM device, brings up only that interface if necessary, verifies
module kernel/signature metadata and loads only the network quirk when carrier
is absent. It then waits for local carrier and IPv6 readiness. The new Rust
pre-start hook rechecks readiness whenever the bridge starts.

An existing local `kait2en-t2-touchid.service.d/link-ready.conf` with the old
`/usr/local/libexec/t2-touchid-link-ready` Bash hook must be replaced by the
installed drop-in, not kept alongside it. The helper is private under
`/usr/local/libexec/kait2en/`; the public manual command remains
`t2-touchid-unlock`. Installation does not enable biometrics through a boot mask
or automatically unlock SEP.

This fixes the startup dependency that caused the observed failure. It cannot
guarantee against all hardware/firmware faults: if carrier drops while the quirk
is already loaded, the helper reports the condition rather than force-unloading
a module, resetting USB, changing the network profile or retrying SEP. Missing
devices and IPv6 readiness waits have fixed time limits.

## Manual invocation

Use this command only when the existing enrolled finger cannot authenticate.
If Touch ID already works, leave the native transport unloaded; no manual unlock
is needed. A local preflight pass alone does not establish that an unlock is needed.

In a terminal, with no `t2sep` transport already loaded:

```sh
t2-touchid-unlock
```

After Linux sudo authorization and successful local input/state checks, the
command displays a warning about the earlier shutdown/panic and asks on
`/dev/tty`:

```text
Continue with one manual SEP unlock? Type 'yes' to confirm [default: no]:
```

Only the exact answer `yes` permits the attempt. Enter, any other answer, EOF
or interruption cancels before a snapshot, service change or native request.
Redirected stdin cannot supply confirmation. If there is no interactive terminal,
the command stops with a hint about the explicit flag.

To acknowledge the same warning automatically for this invocation:

```sh
t2-touchid-unlock --accept-risk
```

The warning is still printed. This flag skips only the risk confirmation; sudo
authorization, the private macOS password prompt, artifact/state checks and
once-per-boot restrictions still apply. It does not configure automatic retries
or boot-time unlock. State is checked again after interactive confirmation in
case it changed while waiting.

No preparer command or shell wrapper is needed. Preparation is an
internal step: the command validates installed inputs first, then snapshots
artifacts into a new private directory. Its manifest records artifact hashes,
the running Rust executable hash, module signer, kernel and current boot ID.
The same invocation validates and consumes that bundle; stale bundles are never
selected from previous runs. Hashes detect changes, not trustworthiness of
unreviewed code.

Confirm Linux sudo authorization first; the separate hidden macOS password
prompt appears only after the native capability check succeeds. The coordinator
copies the private keybag into root-only `/run` storage and removes that copy
on exit. The private Rust client keeps the password and wire buffers in locked
memory, disables dumps, restores terminal echo on ordinary errors or handled
signals, and wipes its mapping on exit. It retains the kernel SHA-256 hashing
interface and checks response hashes/status/bounds before accepting either
acknowledgement. The kernel transport wipes completed DMA payloads.

If biometric services were masked by the selected GRUB entry, the command
prints a specific warning, preserves those masks and does not try to start the
masked services. Native keybag unlock cannot by itself enable fingerprint
recognition while these services remain masked. For normal operation, select the
regular kernel entry without `systemd.mask=kait2en-t2-touchid.service` and
`systemd.mask=fprintd.service`.

The coordinator prevents concurrent runs and verifies temporary runtime
conditions blocking both biometric services, including D-Bus activation. It
then completes one direct RemoteXPC discovery of the advertised BiometricKit
port, closes that connection and makes one native attempt. No SBIO/SKS command
is sent before the initial EP7 negotiation. Only after both unlock
acknowledgements does the optional SKS status read use that already discovered
port; it does not start a second discovery while SEP DMA is registered.
The report records `biometric_service_port`; `sks_before` is no longer queried.
This removes the earlier pre-negotiation BiometricKit command, but has not yet
been verified to prevent the timeout on this machine. A quiet interval is a sequencing precaution, not a DMA-drain
acknowledgement. PCI numbering is discovered from the bound `106b:1802` device.

If both unlocks succeed, the coordinator restores only services previously
active. If a native outcome is unconfirmed, the service guards remain and no
recovery is assumed. A private report records the stage and restoration result,
without the keybag or password. Inspect it before further action. The once-per-
boot marker and existing-module refusal prevent automatic retries.

Cancelling password input before any keybag operation is reported separately.
Only the private client, after its confirmed capability check, can return the
reserved nonzero status `20` for this case. The coordinator records
`keybag_operation_not_sent=true`, wipes/removes its temporary keybag copy,
removes its activation guards and restores only previously active biometric
services. This is a cancelled command, not a successful unlock. The native
module/DMA and once-per-boot marker remain; a second invocation is still refused.
A generic client error, signal termination, timeout or missing client result
cannot claim this outcome and retains the guards. Do not delete markers or
force-unload the native transport to resume password entry.

An incomplete native capability negotiation is recorded in `capabilities`,
including `requested=1 complete=0`, together with `exchange_device_present`.
A missing exchange device is recorded as `false`, rather than stopping the
diagnosis with an ENOENT inspection error; other metadata errors remain failures.
The error includes the sysfs path/status and exchange-device presence. To inspect
an already failed attempt, read its report and the `t2sep` entries in
`journalctl -k -b`; these reads do not send another SEP request. A successful
`insmod` exit alone is not negotiation success: the driver can remain bound to
retain DMA while refusing to expose `/dev/t2sep`. Neither masking biometric
services nor passing network preflight proves that EP7 is responsive. Do not
remove guards or retry an ambiguous native exchange to collect more evidence.

## Handling a failed native attempt

A Linux reboot does not prove that the peer's AKS state or retained DMA
ownership was reset. The two recorded timeouts persisted across a regular Linux
reboot. Do not repeatedly invoke unlock or increase the deadline to compensate.
[The original project's recovery report](https://github.com/jmurth1234/t2-touchid-linux/blob/ea46d8a0aef3e73b0e2f747aa18721dbcd265bce/docs/LINUX_BRINGUP_TROUBLESHOOTING.md)
documents the same EP7 timeout, a discovery/startup race and recovery through
macOS on a different model/firmware. Its recovery sequence is to log into macOS,
verify an existing enrolled finger, shut macOS down, wait about 30 seconds and
then boot Linux. Do not add/delete fingerprints. This is a reported recovery
procedure, not proof of safe cross-OS DMA handoff or a guarantee for every T2.
On the tested MacBookPro16,1, the first subsequent macOS boot itself produced
another `SEPD/MDMA` panic on SEPOS `3151.161.2`. Existing Touch ID then worked,
and the next regular Linux boot recorded a successful enrolled-finger match
without loading `t2sep`. Do not describe the transition through macOS as a safe
or automatic repair, and do not repeat it in a loop to obtain a native reply.

After normal fingerprint operation resumes, stop: no new native attempt is
needed. Keep the failed-attempt report and panic evidence for lifecycle analysis.
If fingerprint operation remains unavailable, another test requires an explicit
case-by-case review of those reports; a reboot or recovery sequence alone does
not authorize it. The local `--check-only` can inspect installation and network
readiness without a SEP request, but cannot prove peer AKS readiness or safe DMA
handoff. Do not unload, reset or retry an ambiguous native exchange. The private
macOS password prompt is reached only after EP7 succeeds.

After a successful command, `fprintd-verify` can check the enrolled finger.
The native module remains pinned; the normal bridge handles fingerprint
recognition. No additional native request is needed for that verification.

See [transport notes](research/README.md) for the wire contract and lifecycle
constraints. All included tests use fabricated data or loopback peers.
