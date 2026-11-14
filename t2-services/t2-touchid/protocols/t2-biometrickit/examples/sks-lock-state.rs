// SPDX-License-Identifier: GPL-3.0-or-later
// Discovery-only readiness before native setup; optional SKS query after it.
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--check-service"] {
        let port = t2_biometrickit::discover_service_port(None, None)?;
        println!("BIOMETRIC_SERVICE_PORT={port}");
        return Ok(());
    }
    anyhow::ensure!((1..=2).contains(&args.len()), "usage: sks-lock-state --check-service | MACOS_UID [DISCOVERED_PORT]");
    let uid = args[0].parse::<u32>()?;
    let state = if let Some(port) = args.get(1) {
        t2_biometrickit::query_sks_lock_state_at(None, None, uid, port.parse()?)?
    } else {
        t2_biometrickit::query_sks_lock_state(None, None, uid)?
    };
    println!("SKS_LOCK_STATE_RAW={state:#010x}");
    Ok(())
}
