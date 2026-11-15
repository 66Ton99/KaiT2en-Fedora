# t2_touchid_link

Out-of-tree kernel quirk module for Apple T2 internal CDC-NCM network interfaces (`05ac:8233`).

## Overview

On Apple T2 MacBooks under Linux, the internal USB CDC-NCM Ethernet device connects the Intel host to BridgeOS for Touch ID biometric services (`BiometricKit`, `SKS`). The firmware does not send the standard CDC link-state notification during boot, leaving the interface in a `NO-CARRIER` state (`carrier == 0`).

This module triggers `usbnet_link_change(usbnet, true, false)` on the internal CDC-NCM device during module initialization, notifying `usbnet` to bring the carrier up so that NetworkManager or systemd-networkd can configure IPv6 link-local communication.

## Parameters

- `ifname`: Optional interface name override (e.g. `enp4s0f1u1`). If omitted, the module automatically scans registered net devices for the `05ac:8233` CDC-NCM device.
