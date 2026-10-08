// SPDX-License-Identifier: GPL-3.0-or-later
use crate::manual::{UnlockOptions, available, unlock_with, validate_manifest};
use crate::os::Platform;
use crate::prepare::{prepare_with, review_with};
use crate::{FILES, Host, PrepareOptions, io_error, regular, valid_uid};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

pub struct DefaultOptions { pub check_only: bool, pub accept_risk: bool }
impl DefaultOptions {
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Option<Self>> {
        let args: Vec<_> = args.into_iter().collect();
        if args == ["--help"] { return Ok(None); }
        let mut check_only = false;
        let mut accept_risk = false;
        for arg in args {
            if arg == "--check-only" && !check_only { check_only = true; }
            else if arg == "--accept-risk" && !accept_risk { accept_risk = true; }
            else { anyhow::bail!("Usage: t2-touchid-unlock [--check-only | --accept-risk]; duplicate or unknown arguments are refused. Paths and UID belong in the root-owned configuration."); }
        }
        ensure!(!(check_only && accept_risk), "--check-only and --accept-risk cannot be combined; read-only checks do not need risk confirmation.");
        Ok(Some(Self { check_only, accept_risk }))
    }
}

pub(crate) struct Layout { pub config: PathBuf, pub assets: PathBuf, pub data: PathBuf }
impl Default for Layout {
    fn default() -> Self {
        Self {
            config: option_env!("T2_UNLOCK_CONFIG").unwrap_or("/etc/kait2en/touchid-unlock.json").into(),
            assets: option_env!("T2_UNLOCK_ASSET_DIR").unwrap_or("/usr/local/libexec/kait2en/touchid-unlock").into(),
            data: option_env!("T2_UNLOCK_DATA_DIR").unwrap_or("/var/lib/kait2en/touchid-unlock").into(),
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    macos_uid: u32,
    #[serde(default)]
    keybag: Option<PathBuf>,
    // Compatibility only: the old field cannot authorize a native operation.
    #[serde(default, rename = "accept_prior_shutdown_risk")]
    legacy_risk: Option<bool>,
}
fn settings(path: &Path, root: u32) -> Result<Settings> {
    let file = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path)
        .map_err(|error| io_error(error, "Cannot open manual-unlock configuration", path,
            "Run 'sudo make -C t2-services/t2-touchid install-unlock' from the repository root, then configure this file."))?;
    let info = file.metadata().with_context(|| format!("Cannot inspect configuration '{}'", path.display()))?;
    ensure!(info.is_file() && info.uid() == root && info.mode() & 0o077 == 0
            && info.nlink() == 1 && (1..=8192).contains(&info.len()),
            "Configuration must be a root-owned private regular file without links (maximum 8192 bytes).");
    let mut bytes = Vec::new();
    file.take(8193).read_to_end(&mut bytes).with_context(|| format!("Cannot read configuration '{}'", path.display()))?;
    ensure!(bytes.len() <= 8192, "Configuration exceeded its bound");
    let settings: Settings = serde_json::from_slice(&bytes).with_context(|| format!("Invalid JSON configuration '{}'; correct the field names and values using touchid-unlock.example.json", path.display()))?;
    if settings.legacy_risk.is_some() {
        eprintln!("Obsolete 'accept_prior_shutdown_risk' in '{}': ignored. Remove it; risk is confirmed at each launch or with --accept-risk.", path.display());
    }
    valid_uid(settings.macos_uid)?;
    if let Some(keybag) = &settings.keybag { ensure!(keybag.is_absolute(), "Configured keybag path must be absolute"); }
    Ok(settings)
}
fn directory(path: &Path, root: u32, private: bool) -> Result<()> {
    let info = fs::symlink_metadata(path).map_err(|error| io_error(error, "Cannot inspect installed directory", path,
        "Run 'sudo make -C t2-services/t2-touchid install-unlock' from the repository root. A DESTDIR staging package does not install system files."))?;
    ensure!(info.is_dir() && info.uid() == root && info.mode() & (if private { 0o077 } else { 0o022 }) == 0,
            "Installed directory must be root-owned, protected and without symlinks: {}", path.display());
    Ok(())
}
fn assets(layout: &Layout, platform: &impl Platform) -> Result<PathBuf> {
    let mut components = Path::new(platform.kernel()).components();
    ensure!(matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none(), "Invalid kernel directory component");
    let directory = layout.assets.join(platform.kernel());
    self::directory(&directory, platform.root_uid(), false)
        .with_context(|| format!("Missing or unsafe artifacts for running kernel '{}'; install the signed matching t2sep.ko and helpers into '{}'. Do not use another kernel's module", platform.kernel(), directory.display()))?;
    for name in FILES {
        let path = directory.join(name);
        let info = regular(&path).with_context(|| format!("Required kernel artifact '{}' is unavailable; reinstall the signed module and helpers for kernel '{}'", path.display(), platform.kernel()))?;
        ensure!(info.uid() == platform.root_uid() && info.mode() & 0o022 == 0 && info.nlink() == 1,
                "Installed artifact must be root-owned without writable sharing or links: {name}");
    }
    Ok(directory)
}

pub fn default_unlock(host: &mut Host, options: &DefaultOptions) -> Result<()> {
    default_with(host, &Layout::default(), options)
}
pub(crate) fn default_with(platform: &mut impl Platform, layout: &Layout, options: &DefaultOptions) -> Result<()> {
    // Refuse existing native state before reading configuration or making a snapshot.
    available(platform, false).context("PRECHECK_REFUSED")?;
    ensure!(platform.euid() == 0, "Run t2-touchid-unlock as root to inspect protected state");
    directory(layout.config.parent().context("Configuration has no parent")?, platform.root_uid(), false)?;
    let settings = settings(&layout.config, platform.root_uid())?;
    directory(&layout.assets, platform.root_uid(), false)?;
    directory(&layout.data, platform.root_uid(), true)?;
    let source = assets(layout, platform)?;
    let mut args = UnlockOptions {
        // Review inputs without treating them as an authorized native request.
        check_only: true, accept_risk: false,
        uid: settings.macos_uid, keybag: settings.keybag.unwrap_or_else(|| layout.data.join("user.kb")),
        bundle: source.clone(),
    };
    let mut preparation = PrepareOptions {
        module: source.join("t2sep.ko"), client: source.join("t2-keybag-unlock"),
        sks: source.join("sks-lock-state"), runner: platform.runner().to_owned(), output: PathBuf::new(),
    };
    let manifest = review_with(platform, &preparation)?;
    validate_manifest(platform, &args, manifest)?;
    available(platform, false).context("PRECHECK_REFUSED")?;
    platform.check_link()?;
    if options.check_only {
        println!("PRECHECK_PASS: configured UID/keybag, installed artifacts and local trial state verified.");
        println!("No files, services, password or hardware operations. Native risk remains.");
        return Ok(());
    }
    let reports = layout.data.join("reports");
    directory(&reports, platform.root_uid(), true)?;
    eprintln!("{}", crate::confirmation::RISK_WARNING);
    if options.accept_risk {
        eprintln!("Risk confirmed for this invocation by --accept-risk.");
    } else {
        platform.confirm_risk()?;
    }
    // Confirmation can take time: recheck state before making any snapshot.
    available(platform, false).context("PRECHECK_REFUSED")?;
    args.check_only = false;
    args.accept_risk = true;
    let directory = tempfile::Builder::new().prefix("manual-").tempdir_in(&reports)
        .map_err(|error| io_error(error, "Cannot create private snapshot directory in", &reports, "Check free space and root-only directory permissions."))?;
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
    let directory = directory.keep();
    preparation.output = directory.join("bundle");
    println!("Trial directory: {}", directory.display());
    prepare_with(platform, &preparation)?;
    args.bundle = preparation.output;
    unlock_with(platform, &args)
}

// Keep this check narrow before sudo: unreadable protected state requires root,
// while a present native module/device already proves that another load is forbidden.
pub fn refuse_existing_transport(host: &Host) -> Result<()> {
    crate::manual::native_absent(host.paths()).context("PRECHECK_REFUSED")
}
