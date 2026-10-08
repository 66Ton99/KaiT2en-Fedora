// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

mod manual;
mod keybag;
pub use keybag::{KEYBAG_INPUT_ABORTED, KeybagOptions, keybag_exit_code, run_keybag};
mod os;
mod prepare;
mod defaults;
mod diagnostics;
mod confirmation;
mod link_ready;
pub use link_ready::run_link_ready;
use diagnostics::io_error;
pub use defaults::{DefaultOptions, default_unlock, refuse_existing_transport};
pub use os::{Host, install_signal_handlers};
use prepare::PrepareOptions;

const FILES: [&str; 3] = ["sks-lock-state", "t2-keybag-unlock", "t2sep.ko"];
const UNITS: [&str; 2] = ["kait2en-t2-touchid.service", "fprintd.service"];
const GUARD: &str = "99-kait2en-manual-trial.conf";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format_version: u32,
    kernel: String,
    reviewed_boot: String,
    signer: String,
    // Hash the built Rust coordinator, not a script or the preparation tool.
    runner_sha256: String,
    sha256: BTreeMap<String, String>,
}

#[derive(Clone)]
struct Paths {
    state: PathBuf,
    units: PathBuf,
    module: PathBuf,
    device: PathBuf,
    driver: PathBuf,
}
impl Default for Paths {
    fn default() -> Self {
        Self {
            state: "/run/kait2en-keybag".into(),
            units: "/run/systemd/system".into(),
            module: "/sys/module/t2sep".into(),
            device: "/dev/t2sep".into(),
            driver: "/sys/bus/pci/drivers/t2sep".into(),
        }
    }
}
impl Paths {
    fn conditions(&self) -> [PathBuf; 2] {
        UNITS.map(|unit| self.units.join(format!("{unit}.d")).join(GUARD))
    }
}

fn entry(path: &Path) -> Result<Option<Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(info) => Ok(Some(info)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("Cannot inspect {}", path.display())),
    }
}
fn digest(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|error| io_error(error, "Cannot open file for integrity verification", path, "Check that the installed file is readable."))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer).map_err(|error| io_error(error, "Cannot read file for integrity verification", path, "Check storage and file readability."))?;
        if count == 0 { break; }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn regular(path: &Path) -> Result<Metadata> {
    let info = fs::symlink_metadata(path).map_err(|error| io_error(error, "Cannot inspect required file", path, "Install the required file before running the command."))?;
    ensure!(info.is_file(), "Use a regular file without symlinks: {}", path.display());
    Ok(info)
}
fn private_directory(path: &Path) -> Result<()> {
    fs::DirBuilder::new().mode(0o700).create(path)?;
    Ok(())
}
fn sync_directory(path: &Path) -> Result<()> {
    OpenOptions::new().read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)?.sync_all()?;
    Ok(())
}
fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    publish_checkpoint(path, bytes, |pending, target| fs::rename(pending, target))
}
fn publish_checkpoint(path: &Path, bytes: &[u8], publish: impl FnOnce(&Path, &Path) -> std::io::Result<()>) -> Result<()> {
    let parent = path.parent().context("Missing checkpoint parent")?;
    let mut pending = tempfile::Builder::new().prefix(".pending-").tempfile_in(parent)?;
    pending.as_file().set_permissions(fs::Permissions::from_mode(0o600))?;
    pending.write_all(bytes)?;
    pending.as_file().sync_all()?;
    publish(pending.path(), path)?;
    sync_directory(parent)
}
fn claim(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600)
        .custom_flags(libc::O_NOFOLLOW).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    sync_directory(path.parent().context("Missing state parent")?)
}
fn valid_uid(uid: u32) -> Result<()> {
    ensure!((501..=i32::MAX as u32).contains(&uid), "Use the exported, explicit macOS UID (501..2147483647).");
    Ok(())
}
fn decimal_id(value: &std::ffi::OsStr) -> Result<u32> {
    let value = value.to_str().context("UID must be UTF-8 decimal")?;
    ensure!(!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()), "UID must be decimal");
    Ok(value.parse()?)
}

#[cfg(test)]
mod tests;
