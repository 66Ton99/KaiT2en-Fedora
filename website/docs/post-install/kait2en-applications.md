# KAIT2EN applications

KAIT2EN includes several applications for monitoring and configuring T2 Mac
hardware. The installer selects hardware-specific applications where
necessary. Graphical apps show up in the app drawer after installation. T2
Journal is used from a terminal.

## T2 Fan Control

Monitors temperatures and fan speeds and provides an editable fan curve. A
background service keeps automatic fan control active across login, suspend,
resume, and reboot. It also features an adjustable "system-chill wall" that
sets the fans to 100% to prevent case heating and in effect prochot.

## T2 SMC Control

Displays SMC temperatures, fan speeds, power data, and the hardware clock, and
can write the current system time to that clock. The battery charge limit is
shown but not set here: `t2smc` exposes it as the standard
`charge_control_end_threshold`, so the desktop environment offers it in its own
power settings. Note that this data shown in this app is direct hardware readings. It is
the single source of truth for temperatures and battery statistics. 
Desktop environment's readings of battery charge are only estimations.
As we promised to ship a vanilla Fedora with KaiT2en sugar on top, we did not
manipulate it to show SMC values in Gnome/Plasma. This will later be solved
when upstreaming SMC. 

## T2 CPU Control

Shows CPU frequency, temperature, package power, and throttling state. It can
configure PL1/PL2 power limits, Turbo Boost, maximum frequency, and the CPU
thermal target, and includes an automatic power-limit benchmark. It serves
the purpose of preventing prochot on T2 Macbooks.

## T2 Power Explorer

Presents the kernel device hierarchy together with runtime power-management
state and diagnostics. It helps identify devices that remain active and keep
the system from reaching deeper power-saving states.

## T2 Journal

Fetches Apple T2 bridgeOS logs over the internal `Apple T2 Bridge` network
link and merges them chronologically with the Linux journal. It can select one
boot or all retained boots, filter by regular expression or source, and emit
text or JSONL. The first query downloads a BridgeOS snapshot automatically; use
`t2journal refresh` to replace it explicitly.

The link is set up by the installer and managed by `kait2en-t2-remote`, the
same service that holds the T2 video encoder open. It is disconnected before
suspend and reconnected after resume, so there is nothing to configure.

Typical queries are:

```bash
t2journal -b
t2journal -b -1 --grep 'suspend|watchdog'
t2journal --allboots --source t2
t2journal -b --output jsonl > merged.jsonl
```

`t2journal refresh --sysdiagnose` additionally keeps a copy of the downloaded
sysdiagnose archive in the current directory, e.g. to extract panic logs from
it yourself.

Run `t2journal --help` for all filtering and refresh options.

## T2 Power Tune

Scans for available PCIe ASPM, runtime power-management, wakeup, LTR, and other
power-saving tunables. Selected changes can be tested temporarily or installed
as a persistent systemd service. Replaces powertop/tlp for reaching deeper
(pkg) c-states.

## T2 Force Click

Configures the Force Touch trackpad's normal-click pressure and its harder
Force Click threshold. Force Click is available as a separate event, so it can
be bound without changing normal clicks, tap-to-click, scrolling, or gestures.
Those remain libinput's job.

One action can be selected for a Force Click. Alternating copy/paste, a
recorded keyboard shortcut, or an advanced shell command. A physical
three-finger click already produces a middle click directly from the trackpad,
so it does not need a Force Click binding. Commands run as the active desktop
user.

## KAIT2EN Touch Bar

Optional. The installer asks at the very beginning whether to install it.
Without it, Apple's native firmware row (esc, brightness, volume and media
keys, F-keys while Fn is held) keeps working. Remove it again with
`sudo ./apps/t2-touchbar/uninstall.sh`. That restores the native row and keeps
personal settings and learned state.

Keeps the Touch Bar black until it is touched or Fn is pressed. The waking
touch is consumed, preventing accidental activation of an invisible key. A
short Fn press opens the remembered media or function-key row; holding Fn for
600 milliseconds switches that remembered row.

The initial five-second timeout is learned independently for both rows. It can
grow to thirty seconds after a quick wake-and-use continuation, and shrinks
slowly when the extra time repeatedly goes unused. Touch ID always replaces
the row with the fingerprint prompt during sudo and lock-screen
authentication. Accepted key presses produce a light impulse through the
trackpad actuator.

While dark, the separate `05ac:8102` brightness controller may runtime-suspend.
The `05ac:8302` display/touch device deliberately remains awake so the
first touch can still wake the row.

The installer switches that device from Apple's firmware row to the DRM
display through a root system service. Reboot once after the first install so
the desktop's user-systemd manager receives the new device-access group; a
logout alone may leave that manager running. Until then, the installer falls
back to the firmware row instead of leaving a black panel.

Use `kait2en-touchbar --status` to inspect learned state and
`kait2en-touchbar --reset-learning` to return it to five seconds. System
defaults are in `/etc/kait2en/touchbar.toml`; a user can override the complete
file at `$XDG_CONFIG_HOME/kait2en-touchbar/config.toml`. `key_color` sets the
glyph color as `#rrggbb`; the default cool white matches the keyboard
backlight.

## T2 GPU Control

Used on supported MacBook Pro models with Intel and AMD graphics. It selects
the primary GPU for the next boot and can power down the unused discrete GPU
or enable AMDGPU's power-saving profile.

## T2 Hybrid GPU Control

Used on the MacBookPro15,1, MacBookPro16,1 and MacBookPro16,4. It enables an
iGPU-driven desktop with PRIME offload to the AMD GPU, which wakes on demand
and returns to D3cold when idle. Suspend and resume work in this mode. A
discrete-GPU boot mode remains available as a recovery option.

## T2 Kernel Builder

Provides a graphical workflow for building customized Fedora kernels with the
required T2 configuration and selected patch groups. Completed builds can be
installed or removed through restricted privileged helpers.
