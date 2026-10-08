// SPDX-License-Identifier: GPL-3.0-or-later
use crate::{Paths, io_error};
use anyhow::{Context, Result, ensure};
use std::ffi::{CString, OsString};
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
extern "C" fn interrupted(_: libc::c_int) { INTERRUPTED.store(true, Ordering::Relaxed); }

pub struct SignalHandlers(Vec<(libc::c_int, libc::sigaction)>);
impl Drop for SignalHandlers {
    fn drop(&mut self) {
        for (signal, action) in self.0.iter().rev() {
            // Restore the handlers installed by this process only.
            unsafe { libc::sigaction(*signal, action, std::ptr::null_mut()); }
        }
    }
}
pub fn install_signal_handlers() -> Result<SignalHandlers> {
    install_handlers(&[libc::SIGINT, libc::SIGTERM, libc::SIGHUP])
}
pub(crate) fn install_client_signal_handlers() -> Result<SignalHandlers> {
    install_handlers(&[libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT, libc::SIGTSTP])
}
pub(crate) fn interrupted_now() -> bool { INTERRUPTED.load(Ordering::Relaxed) }
fn install_handlers(signals: &[libc::c_int]) -> Result<SignalHandlers> {
    let mut saved = SignalHandlers(Vec::new());
    for &signal in signals {
        // sigaction is a plain C record; an empty signal mask is initialized below.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = interrupted as usize;
        unsafe { libc::sigemptyset(&mut action.sa_mask); }
        ensure!(unsafe { libc::sigaction(signal, &action, &mut previous) } == 0,
                "Cannot install interrupt handler: {}", std::io::Error::last_os_error());
        saved.0.push((signal, previous));
    }
    Ok(saved)
}

pub(crate) struct Invocation {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub capture: bool,
    pub timeout: Option<Duration>,
    pub lock_fd: Option<RawFd>,
}
impl Invocation {
    pub fn new(program: impl AsRef<Path>, args: impl IntoIterator<Item = OsString>) -> Self {
        Self { program: program.as_ref().to_owned(), args: args.into_iter().collect(),
               capture: false, timeout: None, lock_fd: None }
    }
    pub fn captured(mut self) -> Self { self.capture = true; self }
    pub fn bounded(mut self, seconds: u64) -> Self { self.timeout = Some(Duration::from_secs(seconds)); self }
}
pub(crate) struct Outcome { pub success: bool, pub exit_code: Option<i32>, pub stdout: String }
pub(crate) trait Platform {
    fn kernel(&self) -> &str;
    fn boot(&self) -> &str;
    fn runner(&self) -> &Path;
    fn paths(&self) -> &Paths;
    fn euid(&self) -> u32;
    fn root_uid(&self) -> u32 { 0 }
    fn caller(&self) -> Result<(u32, u32)>;
    fn execute(&mut self, call: Invocation) -> Result<Outcome>;
    fn quiet(&mut self) -> Result<()>;
    fn confirm_risk(&mut self) -> Result<()>;
    fn check_link(&mut self) -> Result<()>;
    fn begin_cleanup(&mut self) {}
    fn character_device(&self, path: &Path) -> Result<bool> {
        character_device_present(path)
    }
    fn owns(&self, path: &Path, uid: u32, gid: u32) -> Result<()>;
}

// ENOENT is an observed absent device, not an inspection failure. Preserve
// other errors: an unreadable or invalid path is not evidence of absence.
pub(crate) fn character_device_present(path: &Path) -> Result<bool> {
    match std::fs::metadata(path) {
        Ok(info) => Ok(info.file_type().is_char_device()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub struct Host { kernel: String, boot: String, runner: PathBuf, paths: Paths }
impl Host {
    pub fn new() -> Result<Self> {
        Ok(Self {
            kernel: std::fs::read_to_string("/proc/sys/kernel/osrelease").context("Cannot read running kernel from /proc/sys/kernel/osrelease; run on Linux with /proc mounted")?.trim().to_owned(),
            boot: std::fs::read_to_string("/proc/sys/kernel/random/boot_id").context("Cannot read boot ID from /proc/sys/kernel/random/boot_id; run on Linux with /proc mounted")?.trim().to_owned(),
            runner: "/proc/self/exe".into(), paths: Paths::default(),
        })
    }
}
fn primary_group(uid: u32) -> Result<u32> {
    let mut size = 16384;
    loop {
        let mut buffer = vec![0u8; size];
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result = std::ptr::null_mut();
        let error = unsafe { libc::getpwuid_r(uid, &mut entry, buffer.as_mut_ptr().cast(), buffer.len(), &mut result) };
        if error == libc::ERANGE && size < 1 << 20 { size *= 2; continue; }
        ensure!(error == 0 && !result.is_null(), "Cannot resolve caller's primary group");
        return Ok(entry.pw_gid);
    }
}
pub(crate) fn caller_ids(sudo: Option<OsString>, polkit: Option<OsString>, gid: Option<OsString>) -> Result<(u32, u32)> {
    let uid = sudo.or(polkit).map(|value| crate::decimal_id(&value)).transpose()?.unwrap_or(0);
    let gid = match gid { Some(value) => crate::decimal_id(&value)?, None => primary_group(uid)? };
    Ok((uid, gid))
}
impl Platform for Host {
    fn kernel(&self) -> &str { &self.kernel }
    fn boot(&self) -> &str { &self.boot }
    fn runner(&self) -> &Path { &self.runner }
    fn paths(&self) -> &Paths { &self.paths }
    fn euid(&self) -> u32 { unsafe { libc::geteuid() } }
    fn caller(&self) -> Result<(u32, u32)> {
        caller_ids(std::env::var_os("SUDO_UID"), std::env::var_os("PKEXEC_UID"), std::env::var_os("SUDO_GID"))
    }
    fn execute(&mut self, call: Invocation) -> Result<Outcome> { execute(call) }
    fn confirm_risk(&mut self) -> Result<()> { crate::confirmation::confirm_on_terminal() }
    fn check_link(&mut self) -> Result<()> {
        t2_bridgexpc::discovery::ready_interface().context("T2 network preflight failed; native SEP unlock was not attempted")?;
        Ok(())
    }
    fn begin_cleanup(&mut self) { INTERRUPTED.store(false, Ordering::Relaxed); }
    fn quiet(&mut self) -> Result<()> {
        for _ in 0..50 {
            ensure!(!INTERRUPTED.load(Ordering::Relaxed), "Interrupted before native operation");
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }
    fn owns(&self, path: &Path, uid: u32, gid: u32) -> Result<()> {
        let path = CString::new(path.as_os_str().as_bytes())?;
        ensure!(unsafe { libc::chown(path.as_ptr(), uid, gid) } == 0,
                "Cannot assign report ownership: {}", std::io::Error::last_os_error());
        Ok(())
    }
}
pub(crate) fn checked(platform: &mut impl Platform, call: Invocation) -> Result<String> {
    let name = call.program.display().to_string();
    let outcome = platform.execute(call)?;
    ensure!(outcome.success, "Command failed: {name}");
    Ok(outcome.stdout)
}
pub(crate) fn flock(file: &File) -> Result<()> {
    ensure!(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "Another manual unlock is running or its lock cannot be acquired: {}", std::io::Error::last_os_error());
    Ok(())
}
struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().map(|status| status.is_none()).unwrap_or(true) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}
fn execute(call: Invocation) -> Result<Outcome> {
    execute_with(call, interrupted_now)
}
fn execute_with(call: Invocation, cancelled: impl Fn() -> bool) -> Result<Outcome> {
    ensure!(!cancelled(), "Interrupted");
    let mut command = Command::new(&call.program);
    command.args(&call.args).env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin");
    if call.capture { command.stdout(Stdio::piped()); }
    if let Some(fd) = call.lock_fd {
        // Only the child inherits fd 9, keeping parent descriptors and parallel
        // tests untouched. dup2/fcntl are async-signal-safe before exec.
        unsafe { command.pre_exec(move || {
            if fd != 9 && libc::dup2(fd, 9) < 0 { return Err(std::io::Error::last_os_error()); }
            if libc::fcntl(9, libc::F_SETFD, 0) < 0 { return Err(std::io::Error::last_os_error()); }
            Ok(())
        }); }
    }
    let mut process = ChildGuard(command.spawn().map_err(|error| io_error(error, "Cannot start required command", &call.program,
        "Check that the executable and its interpreter or loader are installed and executable; commands use /usr/sbin:/usr/bin:/sbin:/bin."))?);
    let child = &mut process.0;
    let mut output = child.stdout.take();
    if let Some(output) = &output {
        let fd = output.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        ensure!(flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == 0,
                "Cannot bound command output reads");
    }
    let mut bytes = Vec::new();
    let start = Instant::now();
    let result = loop {
        drain_output(output.as_mut(), &mut bytes)?;
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Err(error) => { let _ = child.kill(); let _ = child.wait(); break Err(error.into()); }
            Ok(None) => {}
        }
        let interrupted = cancelled();
        if interrupted || call.timeout.is_some_and(|timeout| start.elapsed() >= timeout) {
            // Give the private client its signal handler a chance to wipe its password.
            let mut completed = None;
            if interrupted {
                unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM); }
                let grace = Instant::now();
                while grace.elapsed() < Duration::from_secs(2) {
                    if let Some(status) = child.try_wait()? { completed = Some(status); break; }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            // Preserve a reaped child's explicit result. In particular, only
            // the private client's abort-before-keybag status permits cleanup;
            // a killed child or unconfirmed timeout never supplies that proof.
            if let Some(status) = completed { break Ok(status); }
            let _ = child.kill();
            let _ = child.wait();
            break Err(anyhow::anyhow!(if interrupted { "Interrupted" } else { "Command timed out; outcome is unconfirmed" }));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // A descendant may retain stdout after the direct child exits. Never wait
    // for pipe EOF beyond the command deadline or after its observed exit.
    drain_output(output.as_mut(), &mut bytes)?;
    let status = result?;
    Ok(Outcome { success: status.success(), exit_code: status.code(), stdout: String::from_utf8(bytes)? })
}

fn drain_output(output: Option<&mut std::process::ChildStdout>, bytes: &mut Vec<u8>) -> Result<()> {
    let Some(output) = output else { return Ok(()); };
    let mut buffer = [0u8; 4096];
    loop {
        match output.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(count) => {
                ensure!(bytes.len() + count <= 131072, "Command output exceeded its bound");
                bytes.extend_from_slice(&buffer[..count]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt;
    #[test]
    fn missing_character_device_is_absent_without_an_io_failure() {
        let directory = tempfile::tempdir().unwrap();
        assert!(!character_device_present(&directory.path().join("missing-device")).unwrap());
    }
    #[test]
    fn regular_file_is_not_a_character_device() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file");
        std::fs::write(&path, b"fixture").unwrap();
        assert!(!character_device_present(&path).unwrap());
        // Metadata only: no read/write/open operation on this device.
        assert!(character_device_present(Path::new("/dev/null")).unwrap());
    }
    #[test]
    fn invalid_device_parent_is_not_misreported_as_absent() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file");
        std::fs::write(&path, b"fixture").unwrap();
        let error = character_device_present(&path.join("device")).unwrap_err();
        assert_eq!(error.downcast_ref::<std::io::Error>().unwrap().raw_os_error(), Some(libc::ENOTDIR));
    }

    #[test]
    fn commands_capture_output_and_preserve_failure_status() {
        let output = execute(Invocation::new("/bin/sh", ["-c".into(), "printf fixture".into()]).captured().bounded(5)).unwrap();
        assert!(output.success); assert_eq!(output.stdout, "fixture");
        let output = execute(Invocation::new("/bin/sh", ["-c".into(), "exit 3".into()]).bounded(5)).unwrap();
        assert!(!output.success);
    }
    #[test]
    fn missing_command_reports_executable_and_remedy() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("missing-helper");
        let error = execute(Invocation::new(&path, []).bounded(5)).err().unwrap();
        let message = format!("{error:#}");
        assert!(message.contains(&path.display().to_string()));
        assert!(message.contains("required file or directory is missing"));
        assert!(message.contains("interpreter or loader"));
        assert!(message.contains("os error 2"));
    }
    #[test]
    fn non_executable_command_reports_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("non-executable-helper");
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        let error = execute(Invocation::new(&path, []).bounded(5)).err().unwrap();
        let message = format!("{error:#}");
        assert!(message.contains(&path.display().to_string()));
        assert!(message.contains("access denied"));
        assert!(message.contains("permissions"));
    }
    #[test]
    fn interrupted_parent_preserves_confirmed_child_exit_status() {
        let directory = tempfile::tempdir().unwrap();
        let ready = directory.path().join("ready");
        let call = Invocation::new("/bin/sh", ["-c".into(),
            "trap 'exit 20' TERM; : > \"$1\"; while :; do sleep 0.05; done".into(),
            "fixture".into(), ready.clone().into_os_string()]).bounded(5);
        let output = execute_with(call, || ready.exists()).unwrap();
        assert!(!output.success);
        assert_eq!(output.exit_code, Some(crate::KEYBAG_INPUT_ABORTED));
    }
    #[test]
    fn signal_termination_has_no_client_abort_proof() {
        let output = execute(Invocation::new("/bin/sh", ["-c".into(), "kill -TERM $$".into()]).bounded(5)).unwrap();
        assert!(!output.success);
        assert_eq!(output.exit_code, None);
    }
    #[test]
    fn command_timeout_kills_and_reaps_child() {
        let start = Instant::now();
        assert!(execute(Invocation::new("/bin/sleep", ["5".into()]).captured().bounded(0)).is_err());
        assert!(start.elapsed() < Duration::from_secs(5));
    }
    #[test]
    fn captured_output_has_a_hard_bound() {
        assert!(execute(Invocation::new("/usr/bin/head", ["-c".into(), "131073".into(), "/dev/zero".into()]).captured().bounded(5)).is_err());
    }
    #[test]
    fn descendant_stdout_cannot_extend_command_lifetime() {
        let start = Instant::now();
        let output = execute(Invocation::new("/bin/sh", ["-c".into(), "sleep 5 & printf '%s' \"$!\"".into()]).captured().bounded(1)).unwrap();
        assert!(output.success);
        let descendant: libc::pid_t = output.stdout.parse().unwrap();
        unsafe { libc::kill(descendant, libc::SIGTERM); }
        assert!(start.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn child_inherits_actual_execution_lock_as_fd_nine() {
        let directory = tempfile::tempdir().unwrap(); let path = directory.path().join("lock");
        let lock = OpenOptions::new().read(true).write(true).create_new(true).mode(0o600).open(&path).unwrap();
        flock(&lock).unwrap();
        let mut call = Invocation::new("/bin/sh", ["-c".into(), "test /proc/self/fd/9 -ef \"$1\"".into(), "fixture".into(), path.into_os_string()]).bounded(5);
        call.lock_fd = Some(lock.as_raw_fd());
        assert!(execute(call).unwrap().success);
        flock(&lock).unwrap();
    }
}
