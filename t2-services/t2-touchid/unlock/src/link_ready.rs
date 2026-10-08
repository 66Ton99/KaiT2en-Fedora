// SPDX-License-Identifier: GPL-3.0-or-later
use crate::os::{Invocation, Platform, checked};
use crate::Host;
use anyhow::{Context, Result, ensure};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

trait LinkOps {
    fn interface(&mut self) -> Result<String>;
    fn up(&mut self, name: &str) -> Result<bool>;
    fn carrier(&mut self, name: &str) -> Result<bool>;
    fn bring_up(&mut self, name: &str) -> Result<()>;
    fn loaded(&mut self) -> Result<bool>;
    fn load(&mut self) -> Result<()>;
    fn ready(&mut self, name: &str) -> Result<()>;
    fn elapsed(&self) -> Duration;
    fn wait(&mut self) -> Result<()>;
}
struct LinkHost { host: Host, started: Instant }
impl LinkOps for LinkHost {
    fn interface(&mut self) -> Result<String> { t2_bridgexpc::discovery::interface(None) }
    fn up(&mut self, name: &str) -> Result<bool> {
        let path = Path::new("/sys/class/net").join(name).join("flags");
        let flags = fs::read_to_string(&path).with_context(|| format!("Cannot read {}", path.display()))?;
        Ok(u32::from_str_radix(flags.trim().trim_start_matches("0x"), 16)? & libc::IFF_UP as u32 != 0)
    }
    fn carrier(&mut self, name: &str) -> Result<bool> {
        let path = Path::new("/sys/class/net").join(name).join("carrier");
        Ok(fs::read_to_string(&path).with_context(|| format!("Cannot read {}", path.display()))?.trim() == "1")
    }
    fn bring_up(&mut self, name: &str) -> Result<()> {
        checked(&mut self.host, Invocation::new("ip", ["link".into(), "set".into(), "dev".into(), name.into(), "up".into()]).bounded(5))?;
        Ok(())
    }
    fn loaded(&mut self) -> Result<bool> { Ok(crate::entry(Path::new("/sys/module/t2_touchid_link"))?.is_some()) }
    fn load(&mut self) -> Result<()> {
        let vermagic = checked(&mut self.host, Invocation::new("modinfo", ["-F".into(), "vermagic".into(), "t2_touchid_link".into()]).captured().bounded(5))?;
        ensure!(vermagic.split_whitespace().next() == Some(self.host.kernel()), "Install t2_touchid_link for the running kernel before enabling network readiness");
        let signer = checked(&mut self.host, Invocation::new("modinfo", ["-F".into(), "signer".into(), "t2_touchid_link".into()]).captured().bounded(5))?;
        ensure!(!signer.trim().is_empty(), "Installed t2_touchid_link must be signed for the running kernel");
        checked(&mut self.host, Invocation::new("modprobe", ["t2_touchid_link".into()]).bounded(5))?;
        Ok(())
    }
    fn ready(&mut self, name: &str) -> Result<()> {
        ensure!(t2_bridgexpc::discovery::ready_interface()? == name, "T2 interface identity changed during readiness check");
        Ok(())
    }
    fn elapsed(&self) -> Duration { self.started.elapsed() }
    fn wait(&mut self) -> Result<()> {
        ensure!(!crate::os::interrupted_now(), "Network readiness interrupted");
        std::thread::sleep(Duration::from_millis(200));
        Ok(())
    }
}
fn prepare(ops: &mut impl LinkOps) -> Result<()> {
    let name = loop {
        match ops.interface() {
            Ok(name) => break name,
            Err(error) if ops.elapsed() >= Duration::from_secs(30) => return Err(error).context("T2 CDC-NCM interface did not appear within 30 seconds"),
            Err(_) => ops.wait()?,
        }
    };
    if !ops.up(&name)? { ops.bring_up(&name)?; }
    if !ops.carrier(&name)? {
        ensure!(!ops.loaded()?, "T2 interface {name} has no carrier although t2_touchid_link is already loaded. No module unload, USB reset or automatic retry; inspect the network driver state");
        ops.load().context("Cannot prepare the installed T2 network module; no SEP operation was performed")?;
    }
    loop {
        match ops.ready(&name) {
            Ok(()) => { println!("T2_LINK_READY: {name}; local carrier and IPv6 ready. No SEP request."); return Ok(()); }
            Err(error) if ops.elapsed() >= Duration::from_secs(45) => return Err(error).context("T2 link did not become ready within 45 seconds; check NetworkManager's private link-local profile"),
            Err(_) => ops.wait()?,
        }
    }
}
pub fn run_link_ready() -> Result<()> {
    ensure!(unsafe { libc::geteuid() } == 0, "Run the private link-readiness helper through its root systemd service");
    let mut ops = LinkHost { host: Host::new()?, started: Instant::now() };
    prepare(&mut ops)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Model { available: bool, up: bool, carrier: bool, loaded: bool, ipv6: bool, fail_load: bool, clock: u64, changes: Vec<&'static str> }
    impl Default for Model {
        fn default() -> Self { Self { available: true, up: true, carrier: true, loaded: false, ipv6: true, fail_load: false, clock: 0, changes: Vec::new() } }
    }
    impl LinkOps for Model {
        fn interface(&mut self) -> Result<String> { ensure!(self.available, "No Apple NCM device"); Ok("fixture-ncm".into()) }
        fn up(&mut self, name: &str) -> Result<bool> { assert_eq!(name, "fixture-ncm"); Ok(self.up) }
        fn carrier(&mut self, _: &str) -> Result<bool> { Ok(self.carrier) }
        fn bring_up(&mut self, name: &str) -> Result<()> { assert_eq!(name, "fixture-ncm"); self.changes.push("up"); self.up = true; Ok(()) }
        fn loaded(&mut self) -> Result<bool> { Ok(self.loaded) }
        fn load(&mut self) -> Result<()> { self.changes.push("load-link"); ensure!(!self.fail_load, "Module load failed"); self.loaded = true; self.carrier = true; Ok(()) }
        fn ready(&mut self, name: &str) -> Result<()> { assert_eq!(name, "fixture-ncm"); ensure!(self.carrier && self.ipv6, "Link not ready"); Ok(()) }
        fn elapsed(&self) -> Duration { Duration::from_secs(self.clock) }
        fn wait(&mut self) -> Result<()> { self.clock += 1; Ok(()) }
    }
    #[test] fn ready_link_does_not_change_device_or_load_module() { let mut m = Model::default(); prepare(&mut m).unwrap(); assert!(m.changes.is_empty()); }
    #[test] fn missing_carrier_loads_only_network_module_once() { let mut m = Model { carrier: false, ..Model::default() }; prepare(&mut m).unwrap(); assert_eq!(m.changes, ["load-link"]); }
    #[test] fn down_device_is_brought_up_before_loading_module() { let mut m = Model { up: false, carrier: false, ..Model::default() }; prepare(&mut m).unwrap(); assert_eq!(m.changes, ["up", "load-link"]); }
    #[test] fn loaded_but_down_carrier_is_not_unloaded_or_retried() { let mut m = Model { carrier: false, loaded: true, ..Model::default() }; assert!(prepare(&mut m).is_err()); assert!(m.changes.is_empty()); }
    #[test] fn load_failure_is_not_retried() { let mut m = Model { carrier: false, fail_load: true, ..Model::default() }; assert!(prepare(&mut m).is_err()); assert_eq!(m.changes, ["load-link"]); }
    #[test] fn absent_device_is_bounded_without_changes() { let mut m = Model { available: false, ..Model::default() }; assert!(prepare(&mut m).is_err()); assert_eq!(m.clock, 30); assert!(m.changes.is_empty()); }
    #[test] fn missing_ipv6_is_bounded_without_changing_profile() { let mut m = Model { ipv6: false, ..Model::default() }; assert!(prepare(&mut m).is_err()); assert_eq!(m.clock, 45); assert!(m.changes.is_empty()); }
}
