// SPDX-License-Identifier: GPL-3.0-or-later
// All lifecycle operations use a model; native commands are never executed.
use super::*;
use crate::manual::{UnlockOptions, available, capability_path, unlock_with};
use crate::os::{Invocation, Outcome, Platform};
use crate::prepare::prepare_with;
use std::ffi::OsString;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

struct Model {
    paths: Paths,
    runner: PathBuf,
    boot: String,
    kernel: String,
    euid: u32,
    caller: (u32, u32),
    states: [bool; 2],
    failure: &'static str,
    commands: Vec<(String, Vec<String>)>,
    native: bool,
    unlocked: bool,
    reloads: usize,
    held: Option<File>,
    risk_response: bool,
    confirmations: usize,
}
impl Model {
    fn unit_index(unit: &str) -> usize { UNITS.iter().position(|value| *value == unit).unwrap() }
}
impl Platform for Model {
    fn kernel(&self) -> &str { &self.kernel }
    fn boot(&self) -> &str { &self.boot }
    fn runner(&self) -> &Path { &self.runner }
    fn paths(&self) -> &Paths { &self.paths }
    fn euid(&self) -> u32 { self.euid }
    fn root_uid(&self) -> u32 { unsafe { libc::getuid() } }
    fn caller(&self) -> Result<(u32, u32)> { Ok(self.caller) }
    fn owns(&self, path: &Path, uid: u32, gid: u32) -> Result<()> {
        assert_eq!((uid, gid), self.caller);
        assert_eq!(fs::metadata(path)?.uid(), uid);
        Ok(())
    }
    fn character_device(&self, path: &Path) -> Result<bool> {
        // The model's regular stand-in avoids creating any real device node.
        if ["capabilities-timeout", "exchange-device-missing"].contains(&self.failure) {
            // Exercise the production metadata handling on a genuinely missing
            // path instead of masking ENOENT with the model's is_file().
            return crate::os::character_device_present(path);
        }
        Ok(path == self.paths.device && path.is_file())
    }
    fn check_link(&mut self) -> Result<()> {
        ensure!(self.failure != "no-carrier", "T2 network preflight failed: no carrier");
        Ok(())
    }
    fn confirm_risk(&mut self) -> Result<()> {
        self.confirmations += 1;
        ensure!(self.risk_response, "Risk not confirmed; manual unlock cancelled");
        if self.failure == "confirmation-race" { fs::create_dir(&self.paths.module)?; }
        Ok(())
    }
    fn quiet(&mut self) -> Result<()> {
        if self.failure == "quiet" { anyhow::bail!("Simulated interruption before native operation"); }
        if self.failure == "quiet-race" { fs::create_dir(&self.paths.module)?; }
        Ok(())
    }
    fn execute(&mut self, call: Invocation) -> Result<Outcome> {
        let program = call.program.file_name().unwrap().to_string_lossy().to_string();
        let args: Vec<String> = call.args.iter().map(|value| value.to_string_lossy().to_string()).collect();
        self.commands.push((program.clone(), args.clone()));
        let mut success = true;
        let mut exit_code = None;
        let mut stdout = String::new();
        match program.as_str() {
            "modinfo" => {
                assert!(call.capture);
                assert_eq!(call.timeout, Some(std::time::Duration::from_secs(5)));
                assert_eq!(args[0], "-F");
                stdout = if args[1] == "signer" {
                    if self.failure == "unsigned" { String::new() } else { "fixture signer".into() }
                } else if self.failure == "wrong-kernel" { "different-kernel SMP".into() }
                else { format!("{} SMP", self.kernel) };
                if args[1] == "signer" && self.failure == "race" { fs::create_dir(&self.paths.module)?; }
                if args[1] == "signer" && self.failure == "race-lock" {
                    private_directory(&self.paths.state)?;
                    let lock = OpenOptions::new().read(true).write(true).create_new(true).mode(0o600)
                        .open(self.paths.state.join("unlock.lock"))?;
                    crate::os::flock(&lock)?;
                    self.held = Some(lock);
                }
            }
            "systemctl" => match args[0].as_str() {
                "is-active" => success = self.states[Self::unit_index(args.last().unwrap())],
                "daemon-reload" => {
                    self.reloads += 1;
                    if ["restoration", "input-abort-restore-fail"].contains(&self.failure) && self.reloads == 2 { success = false; }
                }
                "stop" => for unit in &args[1..] { self.states[Self::unit_index(unit)] = false; },
                "start" => for unit in &args[1..] {
                    let guard = self.paths.units.join(format!("{unit}.d")).join(GUARD);
                    self.states[Self::unit_index(unit)] = !(guard.exists() && self.paths.state.join("manual-active").exists());
                    if self.failure == "activation" { self.states[Self::unit_index(unit)] = true; }
                },
                "show" => stdout = if args.contains(&"LoadState".to_owned()) {
                    if self.failure == "masked" { "masked" } else { "loaded" }
                } else if self.states[Self::unit_index(&args[1])] { "yes" } else { "no" }.into(),
                _ => panic!("Unexpected system action"),
            },
            "sks-lock-state" => {
                assert!(call.capture);
                assert_eq!(call.timeout, Some(std::time::Duration::from_secs(25)));
                if args == ["--check-service"] {
                    assert!(!self.native);
                    assert_eq!(self.states, [false, false]);
                    if self.failure == "readiness" { success = false; }
                    stdout = if self.failure == "malformed-service" {
                        "BIOMETRIC_SERVICE_PORT=50000\nextra".into()
                    } else if self.failure == "invalid-service-port" {
                        "BIOMETRIC_SERVICE_PORT=12345\n".into()
                    } else { "BIOMETRIC_SERVICE_PORT=50000\n".into() };
                } else {
                    // An SKS command is allowed only after both native unlock
                    // acknowledgements, using the already discovered port.
                    assert!(self.unlocked);
                    assert_eq!(args, ["501", "50000"]);
                    if self.failure == "post-query" { success = false; }
                    stdout = "SKS_LOCK_STATE_RAW=0x00000208\n".into();
                }
            }
            "insmod" => {
                self.native = true;
                assert_eq!(call.timeout, Some(std::time::Duration::from_secs(30)));
                assert_eq!(&args[1..], &["manual_unlock_trial=1", "register_ool=1", "probe_capabilities=1",
                    "start_sep=0", "probe_testing=0", "probe_control=0", "discovery_window_ms=0"]);
                assert_eq!(self.states, [false, false]);
                let marker = self.paths.state.join("manual-active");
                assert!(marker.exists());
                for guard in self.paths.conditions() {
                    assert!(fs::read_to_string(guard)?.contains(&format!("ConditionPathExists=!{}", marker.display())));
                }
                if self.failure == "native" { success = false; }
                else {
                    fs::create_dir(&self.paths.module)?;
                    if !["capabilities-timeout", "exchange-device-missing"].contains(&self.failure) {
                        fs::write(&self.paths.device, b"fake device, never accessed by a real client")?;
                    }
                    let device = self.paths.driver.join("0000:07:00.2");
                    fs::create_dir_all(&device)?;
                    fs::write(device.join("vendor"), "0x106b\n")?;
                    fs::write(device.join("device"), "0x1802\n")?;
                    fs::write(device.join("capabilities"), if self.failure == "capabilities-timeout" {
                        "requested=1 complete=0\n"
                    } else if self.failure == "capabilities" {
                        "requested=1 complete=0 value=0x0 reply_length=0"
                    } else { "requested=1 complete=1 value=0x0000000000000002 reply_length=100" })?;
                }
            }
            "t2-keybag-unlock" if args[0] == "--check-inputs" => {
                assert!(!call.capture);
                assert!(call.lock_fd.is_none());
                let path = Path::new(&args[1]);
                assert_eq!(fs::read(path)?, b"fabricated encrypted keybag");
                assert_eq!(fs::metadata(path)?.mode() & 0o777, 0o600);
                if self.failure == "input" { success = false; }
            }
            "t2-keybag-unlock" => {
                assert!(self.native);
                assert_eq!(self.states, [false, false]);
                assert!(!call.capture);
                assert!(call.timeout.is_none()); // Private TTY input is deliberately interactive.
                let fd = call.lock_fd.expect("The client must inherit the execution lock");
                assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0);
                assert_eq!(unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) }, 0);
                assert_eq!(args.len(), 2); // No password in arguments or stdin.
                assert_eq!(args[1], "501");
                if self.failure == "client" { success = false; }
                else if ["input-abort", "input-abort-restore-fail"].contains(&self.failure) {
                    success = false;
                    exit_code = Some(crate::KEYBAG_INPUT_ABORTED);
                } else { self.unlocked = true; }
            }
            _ => panic!("Unexpected command: {program}"),
        }
        Ok(Outcome { success, exit_code: exit_code.or(Some(if success { 0 } else { 1 })), stdout })
    }
}
struct Fixture { directory: tempfile::TempDir, model: Model, options: UnlockOptions, preparation: PrepareOptions }
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path();
        let source = base.join("source");
        fs::create_dir(&source).unwrap();
        for name in ["t2sep.ko", "t2-keybag-unlock", "sks-lock-state", "runner"] {
            let path = source.join(name);
            fs::write(&path, format!("fabricated fixture: {name}")).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(if name.ends_with(".ko") { 0o600 } else { 0o700 })).unwrap();
        }
        let keybag = base.join("export.kb");
        fs::write(&keybag, b"fabricated encrypted keybag").unwrap();
        fs::set_permissions(&keybag, fs::Permissions::from_mode(0o600)).unwrap();
        let mut model = Model {
            paths: Paths { state: base.join("state"), units: base.join("units"), module: base.join("module"),
                device: base.join("device"), driver: base.join("pci") },
            runner: source.join("runner"), boot: "fixture-boot".into(), kernel: "fixture-kernel".into(),
            euid: 0, caller: (unsafe { libc::getuid() }, unsafe { libc::getgid() }),
            states: [true, false], failure: "", commands: Vec::new(), native: false, unlocked: false,
            reloads: 0, held: None, risk_response: true, confirmations: 0,
        };
        let preparation = PrepareOptions { module: source.join("t2sep.ko"), client: source.join("t2-keybag-unlock"),
            sks: source.join("sks-lock-state"), runner: model.runner.clone(), output: base.join("bundle") };
        prepare_with(&mut model, &preparation).unwrap();
        model.commands.clear();
        let options = UnlockOptions { check_only: true, accept_risk: false, uid: 501, keybag, bundle: preparation.output.clone() };
        Self { directory, model, options, preparation }
    }
    fn report(&self) -> serde_json::Value {
        let path = fs::read_dir(self.directory.path()).unwrap().map(|path| path.unwrap().path())
            .find(|path| path.file_name().unwrap().to_string_lossy().starts_with("manual-attempt-")).unwrap();
        serde_json::from_slice(&fs::read(path.join("summary.json")).unwrap()).unwrap()
    }
    fn state_dir(&self) { private_directory(&self.model.paths.state).unwrap(); }
    fn manifest(&self, mutate: impl FnOnce(&mut Manifest)) {
        let path = self.options.bundle.join("manifest.json");
        let mut manifest: Manifest = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        mutate(&mut manifest);
        fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    }
}
fn image(root: &Path) -> BTreeMap<PathBuf, (u32, Vec<u8>)> {
    fn visit(path: &Path, result: &mut BTreeMap<PathBuf, (u32, Vec<u8>)>) {
        for path in fs::read_dir(path).unwrap() {
            let path = path.unwrap().path();
            let info = fs::symlink_metadata(&path).unwrap();
            let bytes = if info.is_file() { fs::read(&path).unwrap() }
                else if info.file_type().is_symlink() { fs::read_link(&path).unwrap().as_os_str().as_encoded_bytes().to_vec() }
                else { Vec::new() };
            result.insert(path.clone(), (info.mode(), bytes));
            if info.is_dir() { visit(&path, result); }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, &mut result);
    result
}
fn precheck_case(case: &str) {
    let mut fixture = Fixture::new();
    let mut held = None;
    match case {
        "loaded" => fs::create_dir(&fixture.model.paths.module).unwrap(),
        "device" => std::os::unix::fs::symlink("missing-device", &fixture.model.paths.device).unwrap(),
        "attempt" | "marker" | "state-permissions" | "busy-lock" | "free-lock" => {
            fixture.state_dir();
            match case {
                "attempt" => fs::write(fixture.model.paths.state.join("manual-attempt-fixture-boot"), "already attempted").unwrap(),
                "marker" => std::os::unix::fs::symlink("missing-marker", fixture.model.paths.state.join("manual-active")).unwrap(),
                "state-permissions" => fs::set_permissions(&fixture.model.paths.state, fs::Permissions::from_mode(0o777)).unwrap(),
                _ => {
                    let lock = OpenOptions::new().read(true).write(true).create_new(true).mode(0o600)
                        .open(fixture.model.paths.state.join("unlock.lock")).unwrap();
                    if case == "busy-lock" { crate::os::flock(&lock).unwrap(); held = Some(lock); }
                }
            }
        }
        "guard" => {
            let guard = fixture.model.paths.conditions()[0].clone();
            fs::create_dir_all(guard.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink("missing-guard", guard).unwrap();
        }
        "state-symlink" => std::os::unix::fs::symlink(fixture.directory.path(), &fixture.model.paths.state).unwrap(),
        "unreadable" => fixture.model.paths.state = fixture.directory.path().join("x".repeat(300)),
        "old-boot" => fixture.manifest(|manifest| manifest.reviewed_boot = "other-boot".into()),
        "runner" => fixture.manifest(|manifest| manifest.runner_sha256 = "changed".into()),
        "artifact" => fs::write(fixture.options.bundle.join("t2sep.ko"), "changed").unwrap(),
        "bag-permissions" => fs::set_permissions(&fixture.options.keybag, fs::Permissions::from_mode(0o644)).unwrap(),
        "nonroot" => fixture.model.euid = 1000,
        "no-ack" => fixture.options.check_only = false,
        "valid" => {}
        _ => panic!("Unknown fixture"),
    }
    let before = image(fixture.directory.path());
    let result = unlock_with(&mut fixture.model, &fixture.options);
    if ["valid", "free-lock"].contains(&case) {
        result.unwrap();
        assert_eq!(fixture.model.commands.len(), 2);
    } else { assert!(result.is_err(), "case={case}"); }
    assert_eq!(before, image(fixture.directory.path()), "Read-only preflight changed files: {case}");
    assert!(fixture.model.commands.iter().all(|(program, _)| program == "modinfo"));
    assert!(!fixture.model.native);
    drop(held);
}
macro_rules! precheck_tests {
    ($($name:ident: $case:literal),* $(,)?) => { $(#[test] fn $name() { precheck_case($case); })* };
}
precheck_tests! {
    precheck_no_writes_or_commands_to_hardware: "valid",
    precheck_loaded_module: "loaded", precheck_dangling_device: "device",
    precheck_attempt_marker: "attempt", precheck_dangling_active_marker: "marker",
    precheck_dangling_runtime_guard: "guard", precheck_bad_directory_permissions: "state-permissions",
    precheck_state_symlink: "state-symlink", precheck_metadata_error_fails_closed: "unreadable",
    precheck_concurrent_lock: "busy-lock", precheck_free_lock_unchanged: "free-lock",
    precheck_old_boot: "old-boot", precheck_changed_runner: "runner",
    precheck_changed_artifact: "artifact", precheck_keybag_permissions: "bag-permissions",
    precheck_nonroot_cannot_pass: "nonroot", native_still_requires_acknowledgement: "no-ack",
}
fn lifecycle_case(case: &'static str) {
    let mut fixture = Fixture::new();
    fixture.options.check_only = false;
    fixture.options.accept_risk = true;
    fixture.model.failure = case;
    let result = unlock_with(&mut fixture.model, &fixture.options);
    let report = fixture.report();
    let native = !["input", "readiness", "malformed-service", "invalid-service-port", "quiet", "quiet-race", "activation"].contains(&case);
    let unlocked = ["", "restoration", "post-query"].contains(&case);
    assert_eq!(report["native_attempted"], native);
    assert_eq!(fixture.model.native, native);
    assert_eq!(report["keybag_unlock_succeeded"], unlocked);
    let clients = fixture.model.commands.iter().filter(|(name, args)| name == "t2-keybag-unlock" && args[0] != "--check-inputs").count();
    assert_eq!(clients, usize::from(unlocked || ["client", "input-abort", "input-abort-restore-fail"].contains(&case)));
    assert!(!fs::read_dir(&fixture.model.paths.state).unwrap().any(|path| path.unwrap().file_name().to_string_lossy().starts_with("bag-")));
    if ["input-abort", "input-abort-restore-fail"].contains(&case) {
        assert!(result.is_err());
        assert_eq!(report["keybag_operation_not_sent"], true);
        assert_eq!(report["keybag_unlock_succeeded"], false);
        assert_eq!(report["service_restoration_allowed"], true);
        assert!(fixture.model.paths.state.join("manual-attempt-fixture-boot").exists());
        assert!(!fixture.model.paths.state.join("manual-active").exists());
        assert_eq!(fixture.model.states, [case != "input-abort-restore-fail", false]);
        assert_eq!(report["service_restoration_completed"], case != "input-abort-restore-fail");
        assert!(!fixture.model.commands.iter().any(|(name, args)| name == "sks-lock-state" && args != &["--check-service"]));
        let commands = fixture.model.commands.len();
        assert!(unlock_with(&mut fixture.model, &fixture.options).is_err());
        assert_eq!(fixture.model.commands.len(), commands);
    } else if ["native", "capabilities", "capabilities-timeout", "exchange-device-missing", "client"].contains(&case) {
        assert!(result.is_err());
        assert!(fixture.model.paths.state.join("manual-active").exists());
        assert_eq!(report["service_restoration_allowed"], false);
        assert_eq!(fixture.model.states, [false, false]);
    } else {
        assert!(!fixture.model.paths.state.join("manual-active").exists());
        assert_eq!(fixture.model.states, [case != "restoration", false]);
        assert_eq!(report["service_restoration_allowed"], true);
        assert_eq!(report["service_restoration_completed"], case != "restoration");
        if case == "restoration" {
            assert!(result.is_err());
            assert!(report["finalization_error"].is_string());
            assert_eq!(report["keybag_unlock_succeeded"], true);
        } else if ["", "post-query"].contains(&case) { result.as_ref().unwrap(); }
        else {
            assert!(result.is_err());
            assert!(!fixture.model.paths.state.join("manual-attempt-fixture-boot").exists());
        }
    }
    assert_eq!(report["stage"], "finished");
    assert!(report.get("sks_before").is_none());
    if native { assert_eq!(report["biometric_service_port"], 50000); }
    // The parent's execution lock is released on every return, including errors.
    let file = File::open(fixture.model.paths.state.join("unlock.lock")).unwrap();
    crate::os::flock(&file).unwrap();
    if case == "" { assert_eq!(report["sks_after"], "0x00000208"); }
    if ["capabilities", "capabilities-timeout", "exchange-device-missing"].contains(&case) {
        let expected = fs::read_to_string(capability_path(&fixture.model.paths.driver).unwrap()).unwrap();
        assert_eq!(report["capabilities"], expected.trim());
        assert_eq!(report["exchange_device_present"], case == "capabilities");
        let message = format!("{:#}", result.unwrap_err());
        assert!(message.contains(expected.trim()));
        assert!(message.contains(&fixture.model.paths.device.display().to_string()));
        assert!(message.contains("journalctl -k -b"));
        assert!(message.contains("do not unload, reset or retry"));
        // A second invocation must not issue a second query or start services.
        let commands = fixture.model.commands.len();
        assert!(unlock_with(&mut fixture.model, &fixture.options).is_err());
        assert_eq!(fixture.model.commands.len(), commands);
    }
    if case == "post-query" { assert!(report["post_query_error"].is_string()); }
}
macro_rules! lifecycle_tests {
    ($($name:ident: $case:literal),* $(,)?) => { $(#[test] fn $name() { lifecycle_case($case); })* };
}
lifecycle_tests! {
    restores_only_previous_services: "", readiness_failure_restores_without_native: "readiness",
    malformed_discovery_prevents_native_load: "malformed-service",
    non_dynamic_service_port_prevents_native_load: "invalid-service-port", native_failure_keeps_guards: "native",
    capabilities_failure_never_prompts_password: "capabilities",
    capabilities_timeout_records_incomplete_status_and_missing_device: "capabilities-timeout",
    successful_capability_without_device_never_prompts_password: "exchange-device-missing",
    client_failure_keeps_guards: "client",
    password_abort_restores_services_without_retry: "input-abort",
    password_abort_restoration_failure_retains_no_keybag_outcome: "input-abort-restore-fail",
    restoration_failure_keeps_native_result: "restoration", post_query_failure_does_not_undo_acks: "post-query",
    interruption_before_native_restores: "quiet", state_change_before_native_stops: "quiet-race",
    dbus_activation_guard_must_hold: "activation", input_failure_does_not_change_services: "input",
}
#[test]
fn repeat_refusal_cannot_change_services_after_ambiguous_native_result() {
    let mut fixture = Fixture::new();
    fixture.options.check_only = false; fixture.options.accept_risk = true;
    fixture.model.failure = "native";
    assert!(unlock_with(&mut fixture.model, &fixture.options).is_err());
    let before = image(fixture.directory.path());
    let count = fixture.model.commands.len();
    assert!(unlock_with(&mut fixture.model, &fixture.options).is_err());
    assert_eq!(before, image(fixture.directory.path()));
    assert_eq!(count, fixture.model.commands.len());
    assert_eq!(fixture.model.states, [false, false]);
}
#[test]
fn state_rechecked_after_execution_lock() {
    let mut fixture = Fixture::new();
    fixture.model.failure = "race";
    fixture.options.check_only = false; fixture.options.accept_risk = true;
    assert!(unlock_with(&mut fixture.model, &fixture.options).is_err());
    assert!(fixture.model.commands.iter().all(|(name, _)| name == "modinfo"));
    assert!(!fixture.model.paths.units.exists());
    assert!(!fixture.model.paths.state.join("manual-active").exists());
    crate::os::flock(&File::open(fixture.model.paths.state.join("unlock.lock")).unwrap()).unwrap();
}
#[test]
fn concurrent_execution_lock_race_refused_before_services() {
    let mut fixture = Fixture::new();
    fixture.model.failure = "race-lock";
    fixture.options.check_only = false; fixture.options.accept_risk = true;
    assert!(unlock_with(&mut fixture.model, &fixture.options).is_err());
    assert!(fixture.model.commands.iter().all(|(name, _)| name == "modinfo"));
    assert!(!fixture.model.paths.units.exists());
    fixture.model.held.take();
    crate::os::flock(&File::open(fixture.model.paths.state.join("unlock.lock")).unwrap()).unwrap();
}
fn prepare_case(case: &str) {
    let mut fixture = Fixture::new();
    fixture.preparation.output = fixture.directory.path().join("prepared");
    match case {
        "unsigned" | "wrong-kernel" => fixture.model.failure = if case == "unsigned" { "unsigned" } else { "wrong-kernel" },
        "symlink" => {
            fs::remove_file(&fixture.preparation.module).unwrap();
            std::os::unix::fs::symlink(&fixture.preparation.runner, &fixture.preparation.module).unwrap();
        }
        "existing" => {
            private_directory(&fixture.preparation.output).unwrap();
            fs::write(fixture.preparation.output.join("keep"), b"reviewed data").unwrap();
        }
        "no-executable" => fs::set_permissions(&fixture.preparation.client, fs::Permissions::from_mode(0o600)).unwrap(),
        "valid" => {}
        _ => panic!("Unknown preparation fixture"),
    }
    let result = prepare_with(&mut fixture.model, &fixture.preparation);
    if case == "valid" {
        let manifest = result.unwrap();
        assert_eq!(manifest.reviewed_boot, fixture.model.boot);
        assert_eq!(manifest.kernel, fixture.model.kernel);
        assert_eq!(manifest.runner_sha256, digest(&fixture.preparation.runner).unwrap());
        assert_eq!(fs::metadata(&fixture.preparation.output).unwrap().mode() & 0o777, 0o700);
        for name in FILES {
            assert_eq!(manifest.sha256[name], digest(&fixture.preparation.output.join(name)).unwrap());
            assert_eq!(fs::metadata(fixture.preparation.output.join(name)).unwrap().mode() & 0o777,
                       if name.ends_with(".ko") { 0o600 } else { 0o700 });
        }
        assert_eq!(fs::metadata(fixture.preparation.output.join("manifest.json")).unwrap().mode() & 0o777, 0o600);
    } else {
        assert!(result.is_err());
        if case == "existing" { assert_eq!(fs::read(fixture.preparation.output.join("keep")).unwrap(), b"reviewed data"); }
        else { assert!(!fixture.preparation.output.exists()); }
    }
    assert!(fixture.model.commands.iter().all(|(name, _)| name == "modinfo"));
}
macro_rules! prepare_tests {
    ($($name:ident: $case:literal),* $(,)?) => { $(#[test] fn $name() { prepare_case($case); })* };
}
prepare_tests! {
    preparation_matching_artifacts: "valid", preparation_unsigned_refused: "unsigned",
    preparation_wrong_kernel_refused: "wrong-kernel", preparation_existing_directory_preserved: "existing",
    preparation_symlink_refused: "symlink", preparation_nonexecutable_refused: "no-executable",
}
#[test]
fn failed_checkpoint_keeps_previous_complete_private_record() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("summary.json");
    private_write(&path, b"previous complete record").unwrap();
    assert!(publish_checkpoint(&path, b"partial new record", |_, _| Err(std::io::Error::other("simulated publication failure"))).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"previous complete record");
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}
#[test]
fn complete_checkpoint_replaces_record_without_temporary_files() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("summary.json");
    private_write(&path, b"old").unwrap(); private_write(&path, b"complete new").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"complete new");
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}
#[test]
fn pci_selection_uses_bound_device_and_rejects_ambiguity() {
    let directory = tempfile::tempdir().unwrap();
    let device = directory.path().join("0000:07:00.2");
    fs::create_dir(&device).unwrap(); fs::write(device.join("vendor"), "0x106b\n").unwrap();
    fs::write(device.join("device"), "0x1802\n").unwrap(); fs::write(directory.path().join("bind"), "control").unwrap();
    assert_eq!(capability_path(directory.path()).unwrap(), device.join("capabilities"));
    fs::write(device.join("device"), "0x1803\n").unwrap(); assert!(capability_path(directory.path()).is_err());
    fs::write(device.join("device"), "0x1802\n").unwrap();
    let other = directory.path().join("0001:01:00.0"); fs::create_dir(&other).unwrap();
    fs::write(other.join("vendor"), "0x106b\n").unwrap(); fs::write(other.join("device"), "0x1802\n").unwrap();
    assert!(capability_path(directory.path()).is_err());
}
#[test]
fn caller_identity_accepts_sudo_and_polkit_without_password_data() {
    let uid = unsafe { libc::getuid() }; let gid = unsafe { libc::getgid() };
    assert_eq!(crate::os::caller_ids(Some(uid.to_string().into()), None, Some(gid.to_string().into())).unwrap(), (uid, gid));
    assert_eq!(crate::os::caller_ids(None, Some(uid.to_string().into()), None).unwrap().0, uid);
    assert!(crate::os::caller_ids(Some("-1".into()), None, None).is_err());
}
#[test]
fn invalid_cli_and_private_password_arguments_refused() {
    let args = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();
    assert!(DefaultOptions::parse(args(&["--help"])).unwrap().is_none());
    for values in [vec!["--uid", "501"], vec!["--password", "secret"], vec!["--check-only", "--check-only"], vec!["--run"], vec!["--accept-risk", "--accept-risk"], vec!["--accept-risk", "--check-only"], vec!["--check-only", "--accept-risk"]] {
        assert!(DefaultOptions::parse(args(&values)).is_err());
    }
    assert!(!DefaultOptions::parse(args(&[])).unwrap().unwrap().check_only);
    assert!(DefaultOptions::parse(args(&["--check-only"])).unwrap().unwrap().check_only);
    assert!(!DefaultOptions::parse(args(&[])).unwrap().unwrap().accept_risk);
    assert!(DefaultOptions::parse(args(&["--accept-risk"])).unwrap().unwrap().accept_risk);
    assert!(valid_uid(500).is_err()); assert!(valid_uid(i32::MAX as u32 + 1).is_err());
}
#[test]
fn legacy_script_manifest_rejected_without_services_or_native_calls() {
    let mut fixture = Fixture::new();
    fs::write(fixture.options.bundle.join("manifest.json"), r#"{"kernel":"fixture-kernel","reviewed_boot":"fixture-boot","signer":"test","runner_sha256":"script hash","sha256":{}}"#).unwrap();
    let before = image(fixture.directory.path());
    assert!(unlock_with(&mut fixture.model, &fixture.options).is_err());
    assert_eq!(before, image(fixture.directory.path())); assert!(fixture.model.commands.is_empty());
}
#[test]
fn read_only_state_check_does_not_create_missing_lock_directory() {
    let fixture = Fixture::new();
    available(&fixture.model, false).unwrap(); assert!(!fixture.model.paths.state.exists());
}

// Default installation fixtures use fabricated files and the lifecycle model.
fn default_layout(fixture: &Fixture) -> crate::defaults::Layout {
    let base = fixture.directory.path();
    let layout = crate::defaults::Layout {
        config: base.join("etc/touchid-unlock.json"), assets: base.join("assets"), data: base.join("data"),
    };
    fs::create_dir(layout.config.parent().unwrap()).unwrap();
    fs::create_dir(&layout.assets).unwrap();
    private_directory(&layout.data).unwrap();
    private_directory(&layout.data.join("reports")).unwrap();
    let assets = layout.assets.join(&fixture.model.kernel);
    fs::create_dir(&assets).unwrap();
    for name in FILES { fs::copy(fixture.options.bundle.join(name), assets.join(name)).unwrap(); }
    fs::copy(&fixture.options.keybag, layout.data.join("user.kb")).unwrap();
    private_write(&layout.config, br#"{"macos_uid":501}"#).unwrap();
    layout
}
fn default_case(case: &str) {
    let mut fixture = Fixture::new();
    let layout = default_layout(&fixture);
    let options = DefaultOptions { check_only: case != "no-ack" && case != "no-reports", accept_risk: false };
    let assets = layout.assets.join(&fixture.model.kernel);
    match case {
        "valid" => {},
        "custom-keybag" => private_write(&layout.config, &serde_json::to_vec(&serde_json::json!({
            "macos_uid":501,"keybag":fixture.options.keybag
        })).unwrap()).unwrap(),
        "no-ack" => fixture.model.risk_response = false,
        "check-without-ack" => fixture.model.risk_response = false,
        "legacy-true" => private_write(&layout.config, br#"{"macos_uid":501,"accept_prior_shutdown_risk":true}"#).unwrap(),
        "legacy-false" => private_write(&layout.config, br#"{"macos_uid":501,"accept_prior_shutdown_risk":false}"#).unwrap(),
        "missing-config" => fs::remove_file(&layout.config).unwrap(),
        "missing-config-parent" => fs::remove_dir_all(layout.config.parent().unwrap()).unwrap(),
        "invalid-json" => private_write(&layout.config, b"{").unwrap(),
        "missing-asset-parent" => fs::remove_dir_all(&layout.assets).unwrap(),
        "missing-kernel-assets" => fs::remove_dir_all(&assets).unwrap(),
        "missing-data" => fs::remove_dir_all(&layout.data).unwrap(),
        "missing-keybag" => fs::remove_file(layout.data.join("user.kb")).unwrap(),
        "config-sharing" => fs::set_permissions(&layout.config, fs::Permissions::from_mode(0o644)).unwrap(),
        "config-parent" => fs::set_permissions(layout.config.parent().unwrap(), fs::Permissions::from_mode(0o777)).unwrap(),
        "config-symlink" => {
            fs::remove_file(&layout.config).unwrap();
            std::os::unix::fs::symlink(&fixture.options.keybag, &layout.config).unwrap();
        },
        "config-hardlink" => fs::hard_link(&layout.config, fixture.directory.path().join("config-link")).unwrap(),
        "config-size" => private_write(&layout.config, &[b' ';8193]).unwrap(),
        "invalid-uid" => private_write(&layout.config, br#"{"macos_uid":500}"#).unwrap(),
        "unknown-field" => private_write(&layout.config, br#"{"macos_uid":501,"password":"never-accepted"}"#).unwrap(),
        "relative-keybag" => private_write(&layout.config, br#"{"macos_uid":501,"keybag":"user.kb"}"#).unwrap(),
        "missing-asset" => fs::remove_file(assets.join("sks-lock-state")).unwrap(),
        "asset-sharing" => fs::set_permissions(assets.join("t2-keybag-unlock"), fs::Permissions::from_mode(0o777)).unwrap(),
        "asset-hardlink" => fs::hard_link(assets.join("t2sep.ko"), fixture.directory.path().join("module-link")).unwrap(),
        "asset-parent" => fs::set_permissions(&layout.assets, fs::Permissions::from_mode(0o777)).unwrap(),
        "asset-symlink" => {
            fs::remove_file(assets.join("t2sep.ko")).unwrap();
            std::os::unix::fs::symlink(&fixture.preparation.module, assets.join("t2sep.ko")).unwrap();
        },
        "kernel-traversal" => fixture.model.kernel = "../source".into(),
        "data-sharing" => fs::set_permissions(&layout.data, fs::Permissions::from_mode(0o755)).unwrap(),
        "bag-sharing" => fs::set_permissions(layout.data.join("user.kb"), fs::Permissions::from_mode(0o644)).unwrap(),
        "no-reports" => fs::remove_dir(layout.data.join("reports")).unwrap(),
        "unsigned" => fixture.model.failure = "unsigned",
        "wrong-kernel" => fixture.model.failure = "wrong-kernel",
        "loaded-before-missing-config" => {
            fs::create_dir(&fixture.model.paths.module).unwrap();
            fs::remove_file(&layout.config).unwrap();
        },
        "attempt" => {
            fixture.state_dir();
            private_write(&fixture.model.paths.state.join("manual-attempt-fixture-boot"), b"claimed").unwrap();
        },
        "guard" => {
            let path = fixture.model.paths.conditions()[0].clone();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            private_write(&path, b"existing guard").unwrap();
        },
        _ => panic!("Unknown default-layout fixture: {case}"),
    }
    let before = image(fixture.directory.path());
    let result = crate::defaults::default_with(&mut fixture.model, &layout, &options);
    if ["valid", "custom-keybag", "check-without-ack", "legacy-true", "legacy-false"].contains(&case) { result.unwrap(); }
    else {
        let error = result.unwrap_err();
        let message = format!("{error:#}");
        let expected = match case {
            "missing-config" => Some((layout.config.clone(), "install-unlock")),
            "missing-config-parent" => Some((layout.config.parent().unwrap().to_owned(), "DESTDIR")),
            "invalid-json" => Some((layout.config.clone(), "Invalid JSON")),
            "missing-asset-parent" => Some((layout.assets.clone(), "install-unlock")),
            "missing-kernel-assets" => Some((assets.clone(), fixture.model.kernel.as_str())),
            "missing-asset" => Some((assets.join("sks-lock-state"), "reinstall")),
            "missing-data" => Some((layout.data.clone(), "install-unlock")),
            "missing-keybag" => Some((layout.data.join("user.kb"), "Privately import")),
            "bag-sharing" => Some((layout.data.join("user.kb"), "private")),
            "no-reports" => Some((layout.data.join("reports"), "install-unlock")),
            _ => None,
        };
        if let Some((path, hint)) = expected {
            assert!(message.contains(&path.display().to_string()), "Missing failed path: {message}");
            assert!(message.contains(hint), "Missing setup hint: {message}");
        }
        if case == "loaded-before-missing-config" {
            assert!(format!("{error:#}").contains("Native transport already present"));
            assert!(fixture.model.commands.is_empty());
        }
    }
    assert_eq!(before, image(fixture.directory.path()), "Preparation refusal/check-only wrote files: {case}");
    assert!(fixture.model.commands.iter().all(|(name, _)| name == "modinfo"));
    assert!(!fixture.model.native);
    assert_eq!(fixture.model.confirmations, usize::from(case == "no-ack"));
}
macro_rules! default_tests {
    ($($name:ident: $case:literal),* $(,)?) => { $(#[test] fn $name() { default_case($case); })* };
}
default_tests! {
    default_check_only_reads_installed_files_without_writes: "valid",
    default_configured_keybag_supported: "custom-keybag",
    default_check_only_does_not_require_risk_ack: "check-without-ack",
    default_declined_runtime_confirmation_stops_without_writes: "no-ack",
    default_legacy_true_config_can_be_read_without_authorizing: "legacy-true",
    default_legacy_false_config_can_be_read_without_authorizing: "legacy-false",
    default_missing_config_refused: "missing-config",
    default_missing_config_parent_explained_without_writes: "missing-config-parent",
    default_invalid_json_explained_without_writes: "invalid-json",
    default_missing_asset_parent_explained_without_writes: "missing-asset-parent",
    default_missing_kernel_assets_explained_without_writes: "missing-kernel-assets",
    default_missing_data_explained_without_writes: "missing-data",
    default_missing_keybag_explained_without_writes: "missing-keybag",
    default_shared_config_refused: "config-sharing",
    default_writable_config_parent_refused: "config-parent",
    default_symlink_config_refused: "config-symlink",
    default_hardlink_config_refused: "config-hardlink",
    default_oversized_config_refused: "config-size",
    default_invalid_uid_refused: "invalid-uid",
    default_unknown_config_field_refused: "unknown-field",
    default_relative_keybag_refused: "relative-keybag",
    default_missing_asset_refused: "missing-asset",
    default_writable_executable_refused: "asset-sharing",
    default_hardlink_asset_refused: "asset-hardlink",
    default_writable_asset_parent_refused: "asset-parent",
    default_symlink_asset_refused: "asset-symlink",
    default_kernel_path_traversal_refused: "kernel-traversal",
    default_shared_data_directory_refused: "data-sharing",
    default_shared_keybag_refused: "bag-sharing",
    default_native_without_private_report_directory_refused: "no-reports",
    default_unsigned_module_refused: "unsigned",
    default_wrong_kernel_module_refused: "wrong-kernel",
    default_loaded_transport_refused_before_configuration: "loaded-before-missing-config",
    default_previous_attempt_refused_before_preparation: "attempt",
    default_previous_service_guard_refused_before_preparation: "guard",
}
#[test]
fn default_native_prepares_current_boot_automatically_then_uses_guarded_lifecycle() {
    let mut fixture = Fixture::new();
    let layout = default_layout(&fixture);
    let before = image(&layout.assets);
    crate::defaults::default_with(&mut fixture.model, &layout, &DefaultOptions { check_only: false, accept_risk: false }).unwrap();
    assert_eq!(before, image(&layout.assets));
    assert!(fixture.model.unlocked);
    assert_eq!(fixture.model.states, [true, false]);
    let trial = fs::read_dir(layout.data.join("reports")).unwrap().next().unwrap().unwrap().path();
    assert_eq!(fs::metadata(&trial).unwrap().mode() & 0o777, 0o700);
    let manifest: Manifest = serde_json::from_slice(&fs::read(trial.join("bundle/manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest.reviewed_boot, fixture.model.boot);
    assert_eq!(manifest.runner_sha256, digest(&fixture.model.runner).unwrap());
    for name in FILES {
        assert_eq!(manifest.sha256[name], digest(&layout.assets.join(&fixture.model.kernel).join(name)).unwrap());
    }
    let report = fs::read_dir(&trial).unwrap().map(|path| path.unwrap().path())
        .find(|path| path.file_name().unwrap().to_string_lossy().starts_with("manual-attempt-")).unwrap();
    let report: serde_json::Value = serde_json::from_slice(&fs::read(report.join("summary.json")).unwrap()).unwrap();
    assert_eq!(report["keybag_unlock_succeeded"], true);
    assert_eq!(report["service_restoration_completed"], true);
    assert!(fixture.model.paths.state.join("manual-attempt-fixture-boot").exists());
}
#[test]
fn default_native_race_during_review_refused_before_snapshot_or_services() {
    let mut fixture = Fixture::new();
    let layout = default_layout(&fixture);
    fixture.model.failure = "race";
    assert!(crate::defaults::default_with(&mut fixture.model, &layout, &DefaultOptions { check_only: false, accept_risk: false }).is_err());
    assert!(fixture.model.paths.module.exists());
    assert!(!fixture.model.native);
    assert!(!fixture.model.paths.state.exists());
    assert_eq!(fs::read_dir(layout.data.join("reports")).unwrap().count(), 0);
    assert!(fixture.model.commands.iter().all(|(name, _)| name == "modinfo"));
}

#[test]
fn obsolete_config_cannot_automatically_confirm_native_unlock() {
    for legacy in [true, false] {
        let mut fixture = Fixture::new();
        let layout = default_layout(&fixture);
        private_write(&layout.config, &serde_json::to_vec(&serde_json::json!({"macos_uid":501,"accept_prior_shutdown_risk":legacy})).unwrap()).unwrap();
        fixture.model.risk_response = false;
        let before = image(fixture.directory.path());
        assert!(crate::defaults::default_with(&mut fixture.model, &layout, &DefaultOptions { check_only: false, accept_risk: false }).is_err());
        assert_eq!(fixture.model.confirmations, 1);
        assert!(!fixture.model.native);
        assert_eq!(before, image(fixture.directory.path()));
    }
}
#[test]
fn explicit_risk_flag_skips_only_confirmation_and_completes_modeled_unlock() {
    let mut fixture = Fixture::new();
    let layout = default_layout(&fixture);
    fixture.model.risk_response = false;
    crate::defaults::default_with(&mut fixture.model, &layout, &DefaultOptions { check_only: false, accept_risk: true }).unwrap();
    assert_eq!(fixture.model.confirmations, 0);
    assert!(fixture.model.unlocked);
}
#[test]
fn explicit_risk_flag_cannot_bypass_missing_keybag() {
    let mut fixture = Fixture::new();
    let layout = default_layout(&fixture);
    fs::remove_file(layout.data.join("user.kb")).unwrap();
    let before = image(fixture.directory.path());
    assert!(crate::defaults::default_with(&mut fixture.model, &layout, &DefaultOptions { check_only: false, accept_risk: true }).is_err());
    assert_eq!(fixture.model.confirmations, 0);
    assert!(!fixture.model.native);
    assert_eq!(before, image(fixture.directory.path()));
}
#[test]
fn state_change_while_confirming_refused_before_snapshot_or_services() {
    let mut fixture = Fixture::new();
    let layout = default_layout(&fixture);
    fixture.model.failure = "confirmation-race";
    let before = image(&layout.data);
    assert!(crate::defaults::default_with(&mut fixture.model, &layout, &DefaultOptions { check_only: false, accept_risk: false }).is_err());
    assert_eq!(fixture.model.confirmations, 1);
    assert_eq!(before, image(&layout.data));
    assert!(fixture.model.commands.iter().all(|(name, _)| name == "modinfo"));
    assert!(!fixture.model.native);
}

#[test]
fn unavailable_link_refused_before_confirmation_snapshot_and_services() {
    for check_only in [true, false] {
        for accept_risk in [true, false] {
            let mut fixture = Fixture::new();
            let layout = default_layout(&fixture);
            fixture.model.failure = "no-carrier";
            let before = image(fixture.directory.path());
            let error = crate::defaults::default_with(&mut fixture.model, &layout, &DefaultOptions { check_only, accept_risk }).unwrap_err();
            assert!(error.to_string().contains("no carrier"));
            assert_eq!(fixture.model.confirmations, 0);
            assert_eq!(before, image(fixture.directory.path()));
            assert!(!fixture.model.native);
            assert!(fixture.model.commands.iter().all(|(name, _)| name == "modinfo"));
        }
    }
}
#[test]
fn existing_masks_are_preserved_without_attempting_to_start_masked_services() {
    let mut fixture = Fixture::new();
    fixture.model.failure = "masked";
    fixture.model.states = [false, false];
    fixture.options.check_only = false;
    fixture.options.accept_risk = true;
    unlock_with(&mut fixture.model, &fixture.options).unwrap();
    assert!(fixture.model.unlocked);
    assert_eq!(fixture.model.states, [false, false]);
    assert!(!fixture.model.commands.iter().any(|(name, args)| name == "systemctl" && ["start", "unmask"].contains(&args[0].as_str())));
}
