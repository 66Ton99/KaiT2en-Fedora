// SPDX-License-Identifier: GPL-2.0-only
// Compile the unmodified production mailbox with host-only kernel stand-ins.
use std::error::Error;
use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
struct WorkDir(PathBuf);
impl WorkDir {
    fn new(target: &std::path::Path) -> io::Result<Self> {
        fs::create_dir_all(target)?;
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_err(io::Error::other)?.as_nanos();
        for attempt in 0..100 {
            let path = target.join(format!("mailbox-test-{}-{stamp}-{attempt}", std::process::id()));
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::other("Cannot create private mailbox fixture directory"))
    }
}
impl Drop for WorkDir { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn exercise() -> Result<(), Box<dyn Error>> {
    let root = std::env::current_dir()?;
    let work = WorkDir::new(&root.join("target"))?;
    fs::create_dir(work.0.join("linux"))?;
    fs::copy(root.join("tests/mailbox-shim.h"), work.0.join("mailbox-shim.h"))?;
    for header in ["bitops", "delay", "errno", "io", "ktime", "types"] {
        fs::write(work.0.join("linux").join(format!("{header}.h")), "#include \"../mailbox-shim.h\"\n")?;
    }
    let mut compiler: Vec<_> = std::env::args_os().skip(1).collect();
    if compiler.is_empty() { compiler.push("cc".into()); }
    let binary = work.0.join("test-mailbox");
    let status = Command::new(&compiler[0]).args(&compiler[1..])
        .args(["-std=gnu11", "-O2", "-Wall", "-Wextra", "-Werror"])
        .arg("-I").arg(&work.0).arg("-I").arg(&root)
        .arg(root.join("test-mailbox.c")).arg(root.join("t2sep_mailbox.c"))
        .arg("-o").arg(&binary).status()?;
    if !status.success() { return Err(format!("Mailbox fixture compilation failed: {status}").into()); }
    let mut child = Command::new(binary).spawn()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("Mailbox fixture failed: {status}").into()),
            Err(error) => { let _ = child.kill(); let _ = child.wait(); return Err(error.into()); }
            Ok(None) => {}
        }
        if start.elapsed() >= Duration::from_secs(10) {
            let _ = child.kill(); let _ = child.wait();
            return Err("Mailbox fixture timed out".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn main() {
    if let Err(error) = exercise() { eprintln!("{error}"); std::process::exit(1); }
}
