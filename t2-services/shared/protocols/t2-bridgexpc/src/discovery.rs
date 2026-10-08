// SPDX-License-Identifier: GPL-3.0-or-later

use std::fs;
use std::net::Ipv6Addr;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail, ensure};

pub const DEFAULT_HOST: &str = "fe80::aede:48ff:fe33:4455";
pub const FIRST_DYNAMIC_PORT: u16 = 49152;
pub const LAST_DYNAMIC_PORT: u16 = 65535;

pub fn interface(explicit: Option<String>) -> Result<String> {
    if let Some(interface) = explicit {
        return Ok(interface);
    }
    let mut matches = Vec::new();
    for item in fs::read_dir("/sys/class/net")? {
        let item = item?;
        let driver = item.path().join("device/driver");
        if fs::canonicalize(driver)
            .ok()
            .and_then(|p| p.file_name().map(|n| n == "cdc_ncm"))
            != Some(true)
        {
            continue;
        }
        let device = fs::canonicalize(item.path().join("device"))?;
        if apple_ncm(&device) {
            matches.push(item.file_name().to_string_lossy().into_owned());
        }
    }
    match matches.as_slice() {
        [name] => Ok(name.clone()),
        [] => bail!("no Apple 05ac:8233 CDC-NCM interface found"),
        _ => bail!("multiple Apple CDC-NCM interfaces found; use --interface"),
    }
}

fn apple_ncm(path: &Path) -> bool {
    path.ancestors().any(|parent| {
        let vendor = fs::read_to_string(parent.join("idVendor")).ok();
        let product = fs::read_to_string(parent.join("idProduct")).ok();
        vendor
            .as_deref()
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("05ac"))
            && product
                .as_deref()
                .is_some_and(|p| p.trim().eq_ignore_ascii_case("8233"))
    })
}

/// Inspect only local sysfs/proc state. No ping, sockets, helpers or device changes.
pub fn ready_interface() -> Result<String> {
    let interface = interface(None)?;
    let path = Path::new("/sys/class/net").join(&interface);
    let flags = fs::read_to_string(path.join("flags"))
        .with_context(|| format!("Cannot read T2 interface flags: {}", path.display()))?;
    let flags = u32::from_str_radix(flags.trim().trim_start_matches("0x"), 16)
        .context("Invalid T2 interface flags")?;
    ensure!(flags & libc::IFF_UP as u32 != 0,
        "T2 interface {interface} is administratively down; bring up only this CDC-NCM interface before manual unlock");
    let carrier = fs::read_to_string(path.join("carrier"))
        .with_context(|| format!("Cannot inspect carrier for T2 interface {interface}"))?;
    ensure!(carrier.trim() == "1",
        "T2 interface {interface} has no carrier; BiometricKit/SKS cannot be reached. On affected T2 systems, prepare the installed t2_touchid_link module before manual unlock. No USB reset or SEP retry was performed");
    ensure_link_local(&interface)?;
    Ok(interface)
}

pub fn host(interface: &str, explicit: Option<String>) -> Result<Ipv6Addr> {
    ensure_link_local(interface)?;
    if let Some(host) = explicit {
        return host.parse().context("invalid IPv6 host");
    }
    let _ = Command::new("ping")
        .args(["-6", "-c", "1", "-W", "1", &format!("ff02::1%{interface}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let output = Command::new("ip")
        .args(["-j", "-6", "neighbor", "show", "dev", interface])
        .output();
    if let Ok(output) = output {
        if output.status.success() {
            let entries: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            let mut hosts: Vec<Ipv6Addr> = entries
                .as_array()
                .into_iter()
                .flatten()
                .filter(|entry| usable_neighbor_state(entry.get("state")))
                .filter_map(|entry| entry.get("dst")?.as_str()?.split('%').next()?.parse().ok())
                .filter(|address: &Ipv6Addr| (address.segments()[0] & 0xffc0) == 0xfe80)
                .collect();
            hosts.sort_unstable();
            hosts.dedup();
            if let [host] = hosts.as_slice() {
                return Ok(*host);
            }
        }
    }
    DEFAULT_HOST.parse().context("invalid built-in T2 address")
}

fn ensure_link_local(interface: &str) -> Result<()> {
    let addresses = fs::read_to_string("/proc/net/if_inet6")
        .context("IPv6 is unavailable; cannot reach the T2 link-local service")?;
    let found = addresses.lines().any(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        fields.len() == 6 && fields[3] == "20" && fields[5] == interface
    });
    ensure!(
        found,
        "interface {interface} has no IPv6 link-local address; enable IPv6 on the CDC-NCM link"
    );
    Ok(())
}

// iproute2 emits an array; accept the scalar format used by older producers too.
// Missing, unknown, or unresolved states are not evidence of a usable peer.
fn usable_neighbor_state(state: Option<&serde_json::Value>) -> bool {
    let Some(state) = state else { return false };
    let valid = |value: &serde_json::Value| {
        matches!(value.as_str(), Some("REACHABLE" | "STALE" | "DELAY" | "PROBE" | "PERMANENT" | "NOARP"))
    };
    match state {
        serde_json::Value::String(_) => valid(state),
        serde_json::Value::Array(states) => !states.is_empty() && states.iter().all(valid),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::usable_neighbor_state;
    use serde_json::json;

    #[test]
    fn neighbor_states_reject_unresolved_iproute2_entries() {
        for state in [json!(["FAILED"]), json!(["INCOMPLETE"]), json!("FAILED"),
            json!("INCOMPLETE"), json!([]), json!(null), json!(["STALE", "FAILED"]),
            json!(["UNKNOWN"])] {
            assert!(!usable_neighbor_state(Some(&state)), "{state}");
        }
        assert!(!usable_neighbor_state(None));
        for state in [json!(["REACHABLE"]), json!(["STALE"]), json!(["DELAY"]),
            json!(["PROBE"]), json!(["PERMANENT"]), json!(["NOARP"]), json!("REACHABLE")] {
            assert!(usable_neighbor_state(Some(&state)), "{state}");
        }
    }
}
