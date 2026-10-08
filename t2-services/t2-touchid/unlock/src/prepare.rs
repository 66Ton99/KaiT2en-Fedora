// SPDX-License-Identifier: GPL-3.0-or-later
use crate::os::{Invocation, Platform, checked};
use crate::{FILES, Manifest, digest, private_directory, private_write, regular};
use anyhow::{Result, ensure};
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::PathBuf;

pub(crate) struct PrepareOptions {
    pub module: PathBuf,
    pub client: PathBuf,
    pub sks: PathBuf,
    pub runner: PathBuf,
    pub output: PathBuf,
}
pub(crate) fn prepare_with(platform: &mut impl Platform, options: &PrepareOptions) -> Result<Manifest> {
    let sources = [&options.sks, &options.client, &options.module];
    let mut manifest = review_with(platform, options)?;
    // Exclusive creation never replaces an earlier reviewed artifact directory.
    private_directory(&options.output)?;
    let result = (|| {
        for (name, source) in FILES.into_iter().zip(sources) {
            let mut input = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(source)?;
            ensure!(input.metadata()?.is_file(), "Source changed during preparation");
            let mut output = OpenOptions::new().write(true).create_new(true)
                .mode(if name.ends_with(".ko") { 0o600 } else { 0o700 })
                .open(options.output.join(name))?;
            std::io::copy(&mut input, &mut output)?;
            output.sync_all()?;
        }
        manifest.sha256 = FILES.into_iter().map(|name| Ok((name.to_owned(), digest(&options.output.join(name))?)))
            .collect::<Result<_>>()?;
        private_write(&options.output.join("manifest.json"), &serde_json::to_vec_pretty(&manifest)?)?;
        Ok(manifest)
    })();
    if result.is_err() { fs::remove_dir_all(&options.output)?; }
    result
}
pub(crate) fn review_with(platform: &mut impl Platform, options: &PrepareOptions) -> Result<Manifest> {
    let sources = [&options.sks, &options.client, &options.module];
    for source in sources { regular(source)?; }
    // /proc/self/exe is the trusted live executable, not a user-supplied link.
    if options.runner != std::path::Path::new("/proc/self/exe") { regular(&options.runner)?; }
    for source in [&options.client, &options.sks, &options.runner] {
        ensure!(fs::metadata(source)?.mode() & 0o111 != 0, "Executable permission is missing: {}", source.display());
    }
    let vermagic = field(platform, "vermagic", &options.module)?;
    let signer = field(platform, "signer", &options.module)?;
    ensure!(vermagic.split_whitespace().next() == Some(platform.kernel()) && !signer.is_empty(),
            "Sign the module for the running kernel before preparing a bundle.");
    Ok(Manifest {
        format_version: 1, kernel: platform.kernel().to_owned(), reviewed_boot: platform.boot().to_owned(),
        signer, runner_sha256: digest(&options.runner)?,
        sha256: FILES.into_iter().zip(sources).map(|(name, source)| Ok((name.to_owned(), digest(source)?)))
            .collect::<Result<_>>()?,
    })
}
pub(crate) fn field(platform: &mut impl Platform, name: &str, module: &std::path::Path) -> Result<String> {
    Ok(checked(platform, Invocation::new("modinfo", ["-F".into(), name.into(), module.as_os_str().to_owned()])
        .captured().bounded(5))?.trim().to_owned())
}
