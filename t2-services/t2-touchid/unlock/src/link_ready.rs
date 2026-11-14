// SPDX-License-Identifier: GPL-3.0-or-later
use crate::os::{Invocation, checked};
use crate::Host;
use anyhow::{Context, Result, ensure};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

trait LinkOps {
    fn interface(&mut self) -> Result<String>;
    fn up(&mut self, name: &str) -> Result<bool>;
    fn bring_up(&mut self, name: &str) -> Result<()>;
    fn carrier(&mut self, name: &str) -> Result<bool>;
    fn trigger_carrier_quirk(&mut self, name: &str) -> Result<()>;
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
    fn bring_up(&mut self, name: &str) -> Result<()> {
        checked(&mut self.host, Invocation::new("ip", ["link".into(), "set".into(), "dev".into(), name.into(), "up".into()]).bounded(5))?;
        Ok(())
    }
    fn carrier(&mut self, name: &str) -> Result<bool> {
        let path = Path::new("/sys/class/net").join(name).join("carrier");
        match fs::read_to_string(&path) {
            Ok(carrier) => Ok(carrier.trim() == "1"),
            Err(error) if error.raw_os_error() == Some(libc::EINVAL) => Ok(false),
            Err(error) => Err(error).with_context(|| format!("Cannot inspect carrier for {}", path.display())),
        }
    }
    fn trigger_carrier_quirk(&mut self, name: &str) -> Result<()> {
        if Path::new("/sys/module/t2_touchid_link").exists() {
            let _ = checked(&mut self.host, Invocation::new("modprobe", ["-r".into(), "t2_touchid_link".into()]).bounded(5));
        }
        let load = checked(&mut self.host, Invocation::new("modprobe", ["t2_touchid_link".into(), format!("ifname={name}").into()]).bounded(5));
        if load.is_err() {
            checked(&mut self.host, Invocation::new("modprobe", ["t2_touchid_link".into()]).bounded(5))
                .context("Failed to load t2_touchid_link quirk module")?;
        }
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
        let _ = ops.trigger_carrier_quirk(&name);
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
    struct Model { available: bool, up: bool, carrier: bool, ipv6: bool, clock: u64, changes: Vec<&'static str> }
    impl Default for Model {
        fn default() -> Self { Self { available: true, up: true, carrier: true, ipv6: true, clock: 0, changes: Vec::new() } }
    }
    impl LinkOps for Model {
        fn interface(&mut self) -> Result<String> { ensure!(self.available, "No Apple NCM device"); Ok("fixture-ncm".into()) }
        fn up(&mut self, name: &str) -> Result<bool> { assert_eq!(name, "fixture-ncm"); Ok(self.up) }
        fn bring_up(&mut self, name: &str) -> Result<()> { assert_eq!(name, "fixture-ncm"); self.changes.push("up"); self.up = true; Ok(()) }
        fn carrier(&mut self, name: &str) -> Result<bool> { assert_eq!(name, "fixture-ncm"); Ok(self.carrier) }
        fn trigger_carrier_quirk(&mut self, name: &str) -> Result<()> {
            assert_eq!(name, "fixture-ncm");
            self.changes.push("quirk");
            self.carrier = true;
            Ok(())
        }
        fn ready(&mut self, name: &str) -> Result<()> { assert_eq!(name, "fixture-ncm"); ensure!(self.carrier && self.ipv6, "Link not ready"); Ok(()) }
        fn elapsed(&self) -> Duration { Duration::from_secs(self.clock) }
        fn wait(&mut self) -> Result<()> { self.clock += 1; Ok(()) }
    }
    #[test] fn ready_link_does_not_change_device() { let mut m = Model::default(); prepare(&mut m).unwrap(); assert!(m.changes.is_empty()); }
    #[test] fn down_device_is_brought_up() { let mut m = Model { up: false, ..Model::default() }; prepare(&mut m).unwrap(); assert_eq!(m.changes, ["up"]); }
    #[test] fn missing_carrier_triggers_quirk() {
        let mut m = Model { carrier: false, ..Model::default() };
        prepare(&mut m).unwrap();
        assert_eq!(m.changes, ["quirk"]);
    }
    #[test] fn absent_device_is_bounded_without_changes() { let mut m = Model { available: false, ..Model::default() }; assert!(prepare(&mut m).is_err()); assert_eq!(m.clock, 30); assert!(m.changes.is_empty()); }
    #[test] fn missing_ipv6_is_bounded_without_changing_profile() { let mut m = Model { ipv6: false, ..Model::default() }; assert!(prepare(&mut m).is_err()); assert_eq!(m.clock, 45); assert!(m.changes.is_empty()); }
}
