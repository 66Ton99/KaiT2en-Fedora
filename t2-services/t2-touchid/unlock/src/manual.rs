// SPDX-License-Identifier: GPL-3.0-or-later
use crate::os::{Invocation, Platform, checked, flock};
use crate::prepare::field;
use crate::{FILES, Manifest, Paths, UNITS, claim, digest, entry, io_error, private_write, regular, sync_directory, valid_uid};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::ffi::OsString;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub(crate) struct UnlockOptions {
    pub check_only: bool,
    pub accept_risk: bool,
    pub uid: u32,
    pub keybag: PathBuf,
    pub bundle: PathBuf,
}

fn root_private(info: &Metadata, root: u32) -> bool { info.uid() == root && info.mode() & 0o077 == 0 }
pub(crate) fn native_absent(paths: &Paths) -> Result<()> {
    ensure!(entry(&paths.module)?.is_none() && entry(&paths.device)?.is_none(),
            "Native transport already present; no replacement, unload or retry.");
    Ok(())
}
pub(crate) fn available(platform: &impl Platform, lock_held: bool) -> Result<()> {
    let paths = platform.paths();
    native_absent(paths)?;
    if let Some(info) = entry(&paths.state)? {
        ensure!(info.is_dir() && root_private(&info, platform.root_uid()), "Unsafe operation-state directory");
    }
    ensure!(entry(&paths.state.join("manual-active"))?.is_none(),
            "A previous trial guard exists. No retry or automatic recovery.");
    for path in paths.conditions() {
        ensure!(entry(&path)?.is_none(), "A previous trial guard exists. No retry or automatic recovery.");
    }
    ensure!(entry(&paths.state.join(format!("manual-attempt-{}", platform.boot())))?.is_none(),
            "Native trial already attempted in this boot; no retry.");
    let path = paths.state.join("unlock.lock");
    if !lock_held && let Some(info) = entry(&path)? {
        ensure!(info.is_file() && root_private(&info, platform.root_uid()), "Unsafe operation lock");
        let file = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path)?;
        let opened = file.metadata()?;
        ensure!(info.dev() == opened.dev() && info.ino() == opened.ino(), "Operation lock changed during preflight");
        flock(&file)?;
    }
    Ok(())
}
fn private_export(info: &Metadata, root: u32, caller: u32) -> Result<()> {
    ensure!(info.is_file() && [root, caller].contains(&info.uid()) && info.mode() & 0o077 == 0
            && info.nlink() == 1 && (1..=16000).contains(&info.len()),
            "Exported keybag must be a private, owned regular file without links (1..16000 bytes).");
    Ok(())
}
fn validate(platform: &mut impl Platform, args: &UnlockOptions) -> Result<(Manifest, Metadata, (u32, u32))> {
    ensure!(platform.euid() == 0, "Root access is required to verify all protected preparation state.");
    ensure!(args.check_only || args.accept_risk, "Explicit prior-shutdown risk acknowledgement required for native unlock.");
    valid_uid(args.uid)?;
    let manifest: Manifest = serde_json::from_reader(File::open(args.bundle.join("manifest.json"))?)
        .context("Invalid internally prepared Rust bundle; old script manifests are not accepted")?;
    validate_manifest(platform, args, manifest)
}
pub(crate) fn validate_manifest(platform: &mut impl Platform, args: &UnlockOptions, manifest: Manifest)
    -> Result<(Manifest, Metadata, (u32, u32))> {
    ensure!(platform.euid() == 0, "Root access is required to verify all protected preparation state.");
    ensure!(args.check_only || args.accept_risk, "Confirm the runtime risk warning or use --accept-risk before native unlock.");
    valid_uid(args.uid)?;
    ensure!(manifest.format_version == 1, "Unsupported manifest version");
    ensure!(manifest.runner_sha256 == digest(platform.runner())?, "Reviewed Rust coordinator changed; no native operation.");
    ensure!(manifest.kernel == platform.kernel() && manifest.reviewed_boot == platform.boot(),
            "Kernel or reviewed boot differs. Do not reuse this bundle blindly.");
    ensure!(manifest.sha256.keys().map(String::as_str).eq(FILES), "Incomplete artifact manifest");
    for (name, expected) in &manifest.sha256 {
        let path = args.bundle.join(name);
        let info = regular(&path)?;
        ensure!(name.ends_with(".ko") || info.mode() & 0o111 != 0, "Artifact is not executable");
        ensure!(digest(&path)? == *expected, "Artifact hash changed; no native operation.");
    }
    let module = args.bundle.join("t2sep.ko");
    let vermagic = field(platform, "vermagic", &module)?;
    let signer = field(platform, "signer", &module)?;
    ensure!(vermagic.split_whitespace().next() == Some(manifest.kernel.as_str()) && signer == manifest.signer && !signer.is_empty(),
            "Module kernel/signature metadata mismatch");
    let caller = platform.caller()?;
    let source = fs::symlink_metadata(&args.keybag).map_err(|error| io_error(error, "Cannot inspect encrypted macOS keybag", &args.keybag,
        "Privately import the exported user.kb at this path (mode 0600), or correct 'keybag' in the root-owned configuration."))?;
    private_export(&source, platform.root_uid(), caller.0)
        .with_context(|| format!("Invalid encrypted keybag '{}'", args.keybag.display()))?;
    Ok((manifest, source, caller))
}

#[derive(Serialize)]
struct Report {
    boot_id: String,
    kernel: String,
    macos_uid: u32,
    module_sha256: String,
    runner_sha256: String,
    native_attempted: bool,
    keybag_unlock_succeeded: bool,
    keybag_operation_not_sent: bool,
    service_restoration_allowed: bool,
    service_restoration_completed: bool,
    stage: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    biometric_service_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sks_after: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capabilities: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exchange_device_present: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    post_query_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    finalization_error: Option<String>,
}
struct Trial {
    report: Report,
    report_path: PathBuf,
    caller: (u32, u32),
    prior: [bool; 2],
    guards: Vec<PathBuf>,
    marker_created: bool,
    private: Option<tempfile::TempDir>,
}
impl Trial {
    fn checkpoint(&mut self, platform: &impl Platform, stage: &'static str) -> Result<()> {
        self.report.stage = stage;
        private_write(&self.report_path, &serde_json::to_vec_pretty(&self.report)?)?;
        platform.owns(&self.report_path, self.caller.0, self.caller.1)?;
        println!("MANUAL_STAGE={stage}");
        Ok(())
    }
}
fn active(platform: &mut impl Platform, unit: &str) -> Result<bool> {
    Ok(platform.execute(Invocation::new("systemctl", ["is-active".into(), "--quiet".into(), unit.into()]).bounded(5))?.success)
}
fn system(platform: &mut impl Platform, action: &str, units: impl IntoIterator<Item = &'static str>) -> Result<()> {
    checked(platform, Invocation::new("systemctl", std::iter::once(action.into()).chain(units.into_iter().map(OsString::from))).bounded(30))?;
    Ok(())
}
fn discover_biometric_service(platform: &mut impl Platform, bundle: &Path) -> Result<u16> {
    let output = checked(platform, Invocation::new(bundle.join("sks-lock-state"),
        ["--check-service".into()]).captured().bounded(25))?;
    let value = output.trim().strip_prefix("BIOMETRIC_SERVICE_PORT=")
        .context("Invalid completed BiometricKit discovery response; no native request")?;
    ensure!(value.len() == 5 && value.bytes().all(|byte| byte.is_ascii_digit()),
        "Invalid BiometricKit service port; no native request");
    let port: u16 = value.parse().context("Invalid BiometricKit service port; no native request")?;
    ensure!((49152..=65535).contains(&port), "Invalid BiometricKit service port; no native request");
    Ok(port)
}
fn sks(platform: &mut impl Platform, bundle: &Path, uid: u32, port: u16) -> Result<String> {
    let output = checked(platform, Invocation::new(bundle.join("sks-lock-state"),
        [uid.to_string().into(), port.to_string().into()]).captured().bounded(25))?;
    let value = output.trim().strip_prefix("SKS_LOCK_STATE_RAW=0x").context("Invalid read-only SKS response; no state inferred")?;
    ensure!(value.len() == 8 && value.bytes().all(|byte| byte.is_ascii_hexdigit()), "Invalid read-only SKS response");
    Ok(format!("0x{:08x}", u32::from_str_radix(value, 16)?))
}
pub(crate) fn capability_path(driver: &Path) -> Result<PathBuf> {
    let mut devices = Vec::new();
    for path in fs::read_dir(driver)? {
        let path = path?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else { continue; };
        let bytes = name.as_bytes();
        let bdf = bytes.len() == 12 && bytes[4] == b':' && bytes[7] == b':' && bytes[10] == b'.'
            && (b'0'..=b'7').contains(&bytes[11])
            && bytes.iter().enumerate().all(|(index, byte)| [4, 7, 10].contains(&index) || byte.is_ascii_hexdigit());
        if bdf && fs::read_to_string(path.join("vendor"))?.trim() == "0x106b"
               && fs::read_to_string(path.join("device"))?.trim() == "0x1802" { devices.push(path); }
    }
    ensure!(devices.len() == 1, "Expected exactly one bound T2 SEP device");
    Ok(devices.remove(0).join("capabilities"))
}

pub(crate) fn unlock_with(platform: &mut impl Platform, args: &UnlockOptions) -> Result<()> {
    available(platform, false).context("PRECHECK_REFUSED")?;
    let (manifest, source, caller) = validate(platform, args)?;
    if args.check_only {
        println!("PRECHECK_PASS: current boot/kernel/artifacts and local trial state verified.");
        println!("No password, files, services or hardware were changed. SEP state was not queried; native risk remains.");
        return Ok(());
    }
    let paths = platform.paths().clone();
    if entry(&paths.state)?.is_none() { crate::private_directory(&paths.state)?; }
    let info = fs::symlink_metadata(&paths.state)?;
    ensure!(info.is_dir() && root_private(&info, platform.root_uid()), "Unsafe operation-state directory");
    let lock = OpenOptions::new().read(true).write(true).create(true).truncate(false).mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(paths.state.join("unlock.lock"))?;
    ensure!(lock.metadata()?.is_file() && root_private(&lock.metadata()?, platform.root_uid()), "Unsafe operation lock");
    flock(&lock).context("PRECHECK_REFUSED")?;
    available(platform, true).context("PRECHECK_REFUSED")?;
    let parent = args.bundle.parent().context("Bundle has no report parent")?;
    let directory = tempfile::Builder::new().prefix("manual-attempt-").tempdir_in(parent)?.keep();
    platform.owns(&directory, caller.0, caller.1)?;
    sync_directory(&directory)?;
    sync_directory(parent)?;
    let mut trial = Trial {
        report: Report {
            boot_id: platform.boot().to_owned(), kernel: manifest.kernel, macos_uid: args.uid,
            module_sha256: manifest.sha256["t2sep.ko"].clone(), runner_sha256: manifest.runner_sha256,
            native_attempted: false, keybag_unlock_succeeded: false, keybag_operation_not_sent: false,
            service_restoration_allowed: false, service_restoration_completed: false,
            stage: "preflight", biometric_service_port: None, sks_after: None, capabilities: None, exchange_device_present: None,
            error: None, post_query_error: None, finalization_error: None,
        },
        report_path: directory.join("summary.json"), caller, prior: [false; 2],
        guards: Vec::new(), marker_created: false, private: None,
    };
    let operation = execute_trial(platform, args, &source, &lock, &mut trial);
    if let Err(error) = &operation {
        trial.report.error = Some(format!("{error:#}"));
        eprintln!("Manual trial stopped: {error:#}");
    }
    platform.begin_cleanup();
    let finalization = finalize(platform, &paths, &mut trial);
    if let Err(error) = &finalization { trial.report.finalization_error = Some(format!("{error:#}")); }
    // Always preserve the native outcome, including failures during restoration.
    trial.checkpoint(platform, "finished")?;
    println!("Report: {}", trial.report_path.display());
    finalization?;
    operation
}
fn execute_trial(platform: &mut impl Platform, args: &UnlockOptions, source: &Metadata,
                 lock: &File, trial: &mut Trial) -> Result<()> {
    let paths = platform.paths().clone();
    for (index, unit) in UNITS.into_iter().enumerate() { trial.prior[index] = active(platform, unit)?; }
    trial.checkpoint(platform, "preflight")?;
    trial.private = Some(tempfile::Builder::new().prefix("bag-").tempdir_in(&paths.state)?);
    let mut input = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(&args.keybag)?;
    let current = input.metadata()?;
    ensure!((current.dev(), current.ino(), current.len()) == (source.dev(), source.ino(), source.len()), "Export changed during preflight");
    private_export(&current, platform.root_uid(), trial.caller.0)?;
    let mut bytes = Vec::new();
    input.by_ref().take(16001).read_to_end(&mut bytes)?;
    ensure!(bytes.len() == source.len() as usize, "Export changed while copying");
    let private = trial.private.as_ref().context("Missing private keybag directory")?.path().join("user.kb");
    private_write(&private, &bytes)?;
    checked(platform, Invocation::new(args.bundle.join("t2-keybag-unlock"),
        ["--check-inputs".into(), private.as_os_str().to_owned(), args.uid.to_string().into()]).bounded(5))?;
    let marker = paths.state.join("manual-active");
    claim(&marker, format!("{}\n", platform.boot()).as_bytes())?;
    trial.marker_created = true;
    for path in paths.conditions() {
        fs::create_dir_all(path.parent().context("Missing guard parent")?)?;
        claim(&path, format!("[Unit]\nConditionPathExists=!{}\n", marker.display()).as_bytes())?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
        trial.guards.push(path);
    }
    system(platform, "daemon-reload", [])?;
    system(platform, "stop", UNITS)?;
    // D-Bus activation and the fprintd Requires= dependency must both be blocked.
    for unit in UNITS {
        let load_state = checked(platform, Invocation::new("systemctl",
            ["show".into(), unit.into(), "-p".into(), "LoadState".into(), "--value".into()]).captured().bounded(5))?;
        if load_state.trim() == "masked" {
            ensure!(!active(platform, unit)?, "Masked biometric service is still active; no native operation");
            eprintln!("{unit} is already masked. Its mask is preserved; this invocation will not enable fingerprint authentication. Use a regular GRUB entry without systemd.mask for normal Touch ID service operation.");
            continue;
        }
        ensure!(load_state.trim() == "loaded", "Biometric service {unit} is not installed/loaded; no native operation");
        platform.execute(Invocation::new("systemctl", ["start".into(), unit.into()]).bounded(30))?;
        let condition = checked(platform, Invocation::new("systemctl",
            ["show".into(), unit.into(), "-p".into(), "ConditionResult".into(), "--value".into()]).captured().bounded(5))?;
        ensure!(!active(platform, unit)? && condition.trim() == "no", "Runtime activation guard did not hold; no native operation");
    }
    // Finish RemoteXPC discovery before native negotiation. Do not send an
    // SBIO/SKS operation before the initial EP7 capability exchange, and do
    // not rediscover the dynamic port while SEP DMA remains registered.
    let port = discover_biometric_service(platform, &args.bundle)
        .context("BiometricKit discovery did not finish before native SEP initialization; no native SEP request was sent")?;
    trial.report.biometric_service_port = Some(port);
    trial.checkpoint(platform, "guarded-discovery-ready")?;
    platform.quiet()?; // Sequencing precaution, not a DMA-drain acknowledgement.
    for unit in UNITS { ensure!(!active(platform, unit)?, "Preparation state changed; no native operation"); }
    native_absent(&paths)?;
    claim(&paths.state.join(format!("manual-attempt-{}", platform.boot())), format!("{}\n", platform.boot()).as_bytes())?;
    trial.report.native_attempted = true;
    trial.checkpoint(platform, "native-load-start")?;
    checked(platform, Invocation::new("insmod", std::iter::once(args.bundle.join("t2sep.ko").into_os_string()).chain(
        ["manual_unlock_trial=1", "register_ool=1", "probe_capabilities=1", "start_sep=0", "probe_testing=0",
         "probe_control=0", "discovery_window_ms=0"].map(OsString::from))).bounded(30))?;
    let capability_file = capability_path(&paths.driver)?;
    let capabilities = fs::read_to_string(&capability_file)
        .with_context(|| format!("Cannot read native capability status '{}'; no keybag or password operation", capability_file.display()))?;
    // Record an incomplete negotiation as well as a successful one. Reading
    // this sysfs attribute only inspects cached state; it sends no SEP request.
    trial.report.capabilities = Some(capabilities.trim().to_owned());
    let exchange_device_present = platform.character_device(&paths.device)
        .with_context(|| format!("Cannot inspect native exchange device '{}'; no keybag or password operation", paths.device.display()))?;
    trial.report.exchange_device_present = Some(exchange_device_present);
    ensure!(capabilities.starts_with("requested=1 complete=1 value=0x0000000000000002 ")
            && exchange_device_present,
            "Native capability negotiation was not confirmed: {}='{}'; exchange device '{}' {}. No keybag or password operation. Inspect the t2sep entries in 'journalctl -k -b' for the kernel error; do not unload, reset or retry this transport",
            capability_file.display(), capabilities.trim(), paths.device.display(),
            if exchange_device_present { "present" } else { "absent" });
    trial.checkpoint(platform, "native-capabilities-confirmed")?;
    // Only the private Rust AKS client owns the password TTY; this coordinator never reads it.
    let mut client = Invocation::new(args.bundle.join("t2-keybag-unlock"),
        [private.into_os_string(), args.uid.to_string().into()]);
    client.lock_fd = Some(lock.as_raw_fd());
    let outcome = platform.execute(client)?;
    if !outcome.success && outcome.exit_code == Some(crate::KEYBAG_INPUT_ABORTED) {
        trial.report.keybag_operation_not_sent = true;
        trial.checkpoint(platform, "password-input-aborted-before-keybag")?;
        anyhow::bail!("Password input aborted before any keybag operation; restoring only previously active biometric services. Native transport remains pinned; do not retry unlock in this boot");
    }
    ensure!(outcome.success, "Private keybag client failed; native outcome unconfirmed");
    trial.report.keybag_unlock_succeeded = true;
    trial.checkpoint(platform, "both-keybag-unlocks-confirmed")?;
    match sks(platform, &args.bundle, args.uid, port) {
        Ok(value) => trial.report.sks_after = Some(value),
        Err(error) => trial.report.post_query_error = Some(format!("{error:#}")),
    }
    Ok(())
}
fn finalize(platform: &mut impl Platform, paths: &Paths, trial: &mut Trial) -> Result<()> {
    let restore = !trial.report.native_attempted || trial.report.keybag_unlock_succeeded
        || trial.report.keybag_operation_not_sent;
    trial.report.service_restoration_allowed = restore;
    if let Some(private) = trial.private.take() { private.close()?; }
    if !restore {
        eprintln!("Native outcome unconfirmed. Runtime guards remain; no reload, retry or recovery assumed.");
        return Ok(());
    }
    // Before guard creation no service was changed, so a preflight/input
    // failure must not reload or start anything during cleanup.
    if !trial.marker_created && trial.guards.is_empty() {
        trial.report.service_restoration_completed = true;
        return Ok(());
    }
    // A failed activation condition may have started a previously inactive
    // unit. Restore that part of the snapshot before removing our guards.
    for (index, unit) in UNITS.into_iter().enumerate() {
        if !trial.prior[index] && active(platform, unit)? { system(platform, "stop", [unit])?; }
    }
    for path in &trial.guards { fs::remove_file(path)?; }
    if trial.marker_created { fs::remove_file(paths.state.join("manual-active"))?; }
    system(platform, "daemon-reload", [])?;
    for (index, unit) in UNITS.into_iter().enumerate() {
        if trial.prior[index] { system(platform, "start", [unit])?; }
    }
    trial.report.service_restoration_completed = true;
    Ok(())

}
