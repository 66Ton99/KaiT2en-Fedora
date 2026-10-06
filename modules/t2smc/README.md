# t2smc

Minimal SMC driver for T2 Macs. Provides fan control, battery charge limit,
temperature, current, voltage and power sensors, a watchdog, SMC event
handling and RTC access. Hardware monitoring is exposed via
the standard Linux hwmon interface. Requires no other SMC driver.

This is based on applesmc with macsmc patches but rebuilt from scratch.
The idea is to only use keys we really need and leave the rest to ACPI and drivers
for maximum stability.

## Installation

### One-time test / non persistent

Build and load the module for the currently running kernel without persistant installation:

```sh
make
sudo modprobe -r applesmc # (if installed)
sudo insmod t2smc.ko
```

This is enough to test fan control, temperature sensors, battery charge limit,
and RTC registration without installing anything. If another RTC driver already
registered `rtc0`, the t2smc RTC appears as the next free `rtcN`.

### Persistent Installation with RTC

This is for Fedora (dracut). For other distros please use equivalent commands.

To make `t2smc` available early enough to become `rtc0` and set the system clock,
install it for the current kernel and rebuild only that kernel's initramfs with
the driver included:

```sh
make
sudo modprobe -r applesmc # (if installed)
sudo make install
sudo dracut --force --add-drivers "t2smc" /boot/initramfs-$(uname -r).img $(uname -r)
```

For the matching boot entry, add these kernel command line parameters:

```text
initcall_blacklist=cmos_init module_blacklist=acpi_tad,applesmc
```

`initcall_blacklist=cmos_init` prevents the built-in `rtc_cmos` driver from
registering as `rtc0`. `module_blacklist=acpi_tad,applesmc` prevents the ACPI
TAD RTC and `applesmc` module from loading. With `t2smc` in the initramfs, the
expected result is:

```text
t2smc APP0001:00: registered as rtc0
t2smc APP0001:00: setting system clock ...
```

To uninstall the module:

```sh
sudo make uninstall
```

## Usage

After loading, the hwmon device appears at `/sys/class/hwmon/hwmonN/` with
`name` set to `t2smc`. The exact number `N` depends on your system and can
change across boots. This commands will list available paths:

```sh
for f in /sys/class/hwmon/hwmon*/name; do
    echo "$f: $(cat "$f")"
done

HWMON="$(dirname "$(grep -l '^t2smc$' /sys/class/hwmon/hwmon*/name)")"
```

## Sensors

Sensors are discovered dynamically by key prefix. Every key with a float or
fixed point type becomes a standard hwmon channel. The label is the
four-character SMC key:

| Prefix | hwmon files                   | Unit              |
|--------|-------------------------------|-------------------|
| `T`    | `tempN_label`, `tempN_input`  | millidegree C     |
| `I`    | `currN_label`, `currN_input`  | milliampere       |
| `V`    | `inN_label`, `inN_input`      | millivolt         |
| `P`    | `powerN_label`, `powerN_input`| microwatt         |

Integer keys in these ranges are skipped because their unit differs between
models. Channel numbers depend on the machine, so match sensors by label:

```sh
paste "$HWMON"/power*_label "$HWMON"/power*_input
```

## Power telemetry

`t2smc` leaves the system battery and charger under the control of the
mainline ACPI SBS drivers. It exposes additional SMC telemetry on its hwmon
device without registering another battery:

```text
power_event_count
power_last_event_ns
smc_battery_capacity_percent
smc_battery_voltage_uv
smc_battery_current_ua
smc_battery_power_uw
smc_battery_charge_full_uah
smc_battery_charge_now_uah
smc_battery_cycle_count
smc_adapter_voltage_uv
smc_adapter_current_ua
smc_adapter_power_uw
smc_battery_time_to_empty_s
smc_battery_time_to_full_s
smc_battery_charge_current_ua
smc_battery_charge_voltage_uv
smc_battery_cell_voltage_max_uv
```

The time files return no data while the battery is neither charging nor
discharging. Battery current and battery power are positive while charging
and negative while discharging.

Files for SMC keys not available on a particular model return no data. Values
are read on demand. There is no periodic kernel polling.

The driver subscribes to the standard power-supply notifier chain. An ACPI SBS
battery or adapter notification schedules an SMC status snapshot and increments
`power_event_count`.

To verify the event path, read the counter, connect or disconnect the charger,
and read it again:

```sh
HWMON="$(dirname "$(grep -l '^t2smc$' /sys/class/hwmon/hwmon*/name)")"
cat "$HWMON/power_event_count"
# Connect or disconnect the charger.
cat "$HWMON/power_event_count"
```

### Fan control

We do offer a program to control the fans on T2 MacBooks (https://github.com/deqrocks/t2-fancontrol)
Anyways here are examples to manual control the fans:

```sh
# Identify the t2smc hwmon device
HWMON="$(dirname "$(grep -l '^t2smc$' /sys/class/hwmon/hwmon*/name)")"

# Read current fan speeds
cat "$HWMON/fan1_input"
cat "$HWMON/fan2_input"

# Set fan 1 to 3000 RPM (enters manual mode automatically)
echo 3000 | sudo tee "$HWMON/fan1_target"
```

The fan speed values are in RPM. Writing to `fanN_target` switches the fan to
manual mode and sets the target speed.

The attributes `fanN_min` and `fanN_max` are limits reported by the SMC.
`fanN_min` is writable and `fanN_max` is read-only. If the SMC provides a fan
name (`FnID`), it appears in `fanN_label`.

After resume the driver restores manual mode and the last target speed for
every fan that was set through `fanN_target`.

### Battery charge limit

`t2smc` also attaches the limit to `BAT0` as the standard
`charge_control_end_threshold` property, so any desktop environment that
speaks that interface can set it through its own power settings.

We do offer a GUI program to inspect sensor data and the battery charge limit:
(https://github.com/deqrocks/t2-smc-control).
Anyways here is how to set battery charge limit manually:

```sh
# Read current charge limit
cat "$HWMON/battery_charge_limit"

# Set charge limit to 80 percent
echo 80 | sudo tee "$HWMON/battery_charge_limit"
echo 80 | sudo tee /sys/class/power_supply/BAT0/charge_control_end_threshold
```

Valid range is 0-100. Persistence across reboot, shutdown, or SMC reset is not
guaranteed and may depend on the machine and firmware state.

### Temperature sensors

```sh
# List all sensor labels
cat "$HWMON"/temp*_label

# Read a specific sensor, e.g. GPU proximity
cat "$HWMON/temp21_input"
```

Temperature sensor labels are the SMC key names. Relevant GPU sensors on T2
Macs include:

| Label  | Sensor                  |
|--------|-------------------------|
| TG0P   | GPU proximity           |
| TGDD   | GPU die (digital)       |
| TGDF   | GPU die (filtered)      |
| TGVP   | GPU voltage regulator   |
| TC0E   | CPU 1 Diode Virtual     |
| TC0F   | CPU 1 Diode Filtered    |
| TC0P   | CPU proximity           |
| TB0T   | Battery temperature     |

Values are in millidegrees Celsius. Divide by 1000 for degrees.

### Watchdog

If the SMC has the `OSWD` key, or `NATi` and `NATJ` on older firmware,
`t2smc` registers a standard Linux watchdog (`/dev/watchdogN`). systemd can
use it through `RuntimeWatchdogSec=` in `/etc/systemd/system.conf`. The
timeout range is 1 to 255 seconds. The watchdog is stopped before system
sleep and restarted on resume, and it is stopped on reboot and poweroff.

### SMC events

Every SMC command sleeps until the SMC signals completion with its KeyDone
interrupt. The driver never polls the status register. It therefore
requires the SMC interrupt, the I/O port window that carries the event ID
and the `NTOK` key, and it does not load without them. `NTOK` is set as the
first command after mapping MMIO and again on resume.

When the SMC reports imminent power loss (event `0x40`), the driver syncs
all filesystems and flushes the block devices. macOS reacts to the same event
by telling every AHCI and NVMe disk to prepare for abrupt power loss. Log
messages from the SMC (event `0x4c`) appear in the kernel log as `SMC log:`.
A BridgeOS panic is logged as a warning. Thermal level changes are only
logged at debug level because the SMC sends them about once per second under
load. A command that gets no KeyDone within one second fails.

At load the driver logs the cause of the previous shutdown from `MSSD`, the
same value macOS prints as `Previous shutdown cause`. Like macOS it clears
the one-shot causes -64 and -62 after reporting them.

Each logged cause carries its source. `Apple` descriptions come from Apple's
PowerManagement sources (`common/CommonLib.c`, PowerManagement-1846).
`community` descriptions come from the Eclectic Light Company and George
Garside lists. Only causes on which those lists agree, or that only one of
them names, are included. Apple wins every conflict. The community entries
are unverified and the log is meant to confirm or refute them. Any other
value is logged as `unknown`.

| Cause | Source    | Meaning |
|-------|-----------|---------|
| 0 | Apple | Battery disconnected |
| 1 | Apple | Normal warm reset |
| 2 | Apple | Power supply disconnected |
| 3 | Apple | Power button pressed for > 4 sec |
| 5 | Apple | Software initiated shutdown |
| 7 | Apple | Normal shutdown by SOC |
| -3 | community | Multiple temperature sensors too high |
| -14 | community | Electricity spike or surge |
| -20 | community | BridgeOS (T2) initiated shutdown |
| -60 | Apple | Battery fully drained |
| -61 | community | Watchdog detected unresponsive app, shutting down |
| -62 | community | Watchdog detected unresponsive app, restarting |
| -64 | community | Kernel panic |
| -71 | community | Memory temperature too high |
| -74 | community | Battery temperature too high |
| -75 | community | Power adapter communication problem |
| -78 | community | Incorrect input current from power adapter |
| -79 | community | Incorrect current from battery |
| -81 | Apple | Thermal shutdown for overtemp |
| -86 | community | Proximity temperature too high |
| -100 | community | Power supply temperature too high |
| -101 | community | Display temperature too high |
| -102 | community | Overvoltage |
| -103 | community | Battery voltage too low |
| -104 | community | Unknown battery fault |
| -127 | community | PMU/SMC forced shutdown for another cause |

The SMC publishes its current thermal levels for CPU, IO and GPU. They are
read on demand from the hwmon files `smc_thermal_level_cpu`,
`smc_thermal_level_io` and `smc_thermal_level_gpu`.

### Module parameters

| Parameter     | Default | Meaning                                         |
|---------------|---------|-------------------------------------------------|
| `wdt_timeout` | 60      | Watchdog timeout in seconds                     |
| `nowayout`    | kernel  | Watchdog cannot be stopped once started         |

### RTC

If the SMC RTC keys are present, `t2smc` registers a standard Linux RTC device.
The exact number `N` depends on load order. With early setup it can be `rtc0`;
with a late `insmod` it may be `rtc1`, `rtc2`, or higher.

```sh
# List RTC devices
for f in /sys/class/rtc/rtc*/name; do
    echo "$f: $(cat "$f")"
done

# Select the t2smc RTC
RTC="$(dirname "$(grep -l '^t2smc ' /sys/class/rtc/rtc*/name)")"
RTC_DEV="/dev/$(basename "$RTC")"

# Read the t2smc hardware clock
sudo hwclock --rtc "$RTC_DEV" --show

# Write the current system time to the t2smc hardware clock
sudo hwclock --rtc "$RTC_DEV" --systohc
```

The RTC follows Apple's key protocol.`CLKO` is read and sign-extended during
probe, while `CLKR` supplies the counter frequency (32768 Hz if unavailable).
If an initial `CLKL` read succeeds, each RTC operation obtains the counter with
Apple's asynchronous latch exchange. Write six `0xff` bytes, wait 20 ms, then
poll the 48-bit result.  Machines without that path use the direct `CLKM`
counter. Calendar time is `(counter + CLKO) / CLKR`. Setting the RTC changes
`CLKO`.

As with AppleSMCRTC, `CLKL` traffic is generated when clients request
RTC/calendar time. As AppleSMCRTC, the driver writes `CLKO` only when the
offset changed. Setting the RTC tolerates a quarter second, since the time
comes in whole seconds.

Before system sleep and at shutdown the driver synchronizes `CLKO` with the
system clock at nanosecond resolution, like macOS does before power
transitions. The T2 derives its own clock from the SMC RTC while the host
sleeps or is off. Kait2en also sets the RTC once chrony has synchronized at
boot. The kernel's NTP sync does not reach this RTC on x86, because it writes
the legacy CMOS clock instead.

`tools/t2smc-rtc-client.c` reads the RTC at a fixed interval to exercise the
`CLKL` path while testing. It is not built with the module:

```bash
cc -O2 -Wall -Wextra -o t2smc-rtc-client tools/t2smc-rtc-client.c
sudo ./t2smc-rtc-client /dev/rtc0 1000
```

## License

GPL-2.0-only.
