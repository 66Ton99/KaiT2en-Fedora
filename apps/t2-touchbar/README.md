# KAIT2EN Touch Bar

kait2en-touchbar replaces Apple's built-in Touch Bar row with its own
dark-first display. The bar stays black until you touch it or press Fn,
learns how long to stay lit, offers a media row and an F-key row (hold Fn
to switch), shows a fingerprint prompt for Touch ID and gives haptic
feedback on key presses. It saves power because the bar is off most of
the time.

Two fixed layers are available:

- media keys
- F-keys

A 600 ms Fn hold switches the persistent layer. A short Fn press and a touch
both wake the saved layer. The initial five-second illumination timeout learns
up to a hard thirty-second ceiling when a user repeatedly has to wake the bar
again. It decays slowly when the learned extension is unused.

While `t2-touchid` reports an authentication, all keys disappear and the bar
shows the fingerprint animation next to the sensor. This works for sudo and
the GNOME lock screen in the running user session.

Accepted key presses use `t2_trackpad_actuator` to give a bit of haptic feedback.

## Installation

For a standalone Fedora installation from the repository, run:

```sh
sudo ./apps/t2-touchbar/install.sh
```

The script installs its build dependencies, configures device access and the
user service, disables conflicting Touch Bar daemons, and removes the Cargo
build directory after copying the release binary.

The Fedora installer creates the `kait2en-touchbar` device-access group, adds
the invoking user, installs the global user unit, and replaces conflicting
Touch Bar daemons. A reboot is required after the first
group assignment.

State is stored at `$XDG_STATE_HOME/kait2en-touchbar/state.toml`. Inspect or
reset it with:

```sh
kait2en-touchbar --status
kait2en-touchbar --reset-learning
```

The installed defaults live in `/etc/kait2en/touchbar.toml`. A personal
`$XDG_CONFIG_HOME/kait2en-touchbar/config.toml` replaces them when present.
The learned values remain separate from configuration so editing the bounds
does not destroy the history. Values outside new bounds are clamped on load.

Keys are drawn anti-aliased in Adwaita Sans (an Inter derivative close to the
San Francisco legends on the keyboard) with round-capped icons.

The daemon is event-driven. With the bar dark it blocks on input and D-Bus
file descriptors. There is no periodic inference loop. "Dark" means a black
frame plus backlight level zero. The separate `05ac:8102` brightness controller
is then allowed to runtime-suspend. The `05ac:8302` display/touch device remains
awake, because suspending it would also remove touch-to-wake.

The DRM setup and input architecture are derived from tiny-dfr under its MIT
license. The KAIT2EN implementation is GPL-3.0-or-later. The upstream notice
is in `THIRD-PARTY-NOTICES.md`.
