// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::Context;
use std::os::unix::process::CommandExt;
use t2_manual_unlock::{DefaultOptions, Host, default_unlock, install_signal_handlers, refuse_existing_transport};
fn main() {
    let result = (|| -> anyhow::Result<()> {
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        let Some(options) = DefaultOptions::parse(args.clone())? else {
            println!("Usage: t2-touchid-unlock [--check-only | --accept-risk]\nNo arguments: validate installed files, warn and ask for risk confirmation, then perform one guarded manual unlock.\n--accept-risk: acknowledge the warning for this invocation without the confirmation prompt.\n--check-only: read-only checks; no risk prompt or native exchange.\nConfiguration: /etc/kait2en/touchid-unlock.json\nRoot authorization is requested automatically. Passwords are never command-line arguments.");
            return Ok(());
        };
        let mut host = Host::new()?;
        refuse_existing_transport(&host)?;
        if unsafe { libc::geteuid() } != 0 {
            let executable = std::env::current_exe().context("Cannot locate the running command for sudo authorization; install t2-touchid-unlock in /usr/local/bin")?;
            let error = std::process::Command::new("/usr/bin/sudo")
                .arg(&executable).args(args)
                .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin").exec();
            return Err(anyhow::Error::new(error).context(format!("Cannot start '/usr/bin/sudo' to authorize '{}'; install sudo or run the installed command from a root terminal", executable.display())));
        }
        let _signals = install_signal_handlers()?;
        default_unlock(&mut host, &options)
    })();
    if let Err(error) = result { eprintln!("t2-touchid-unlock: {error:#}"); std::process::exit(1); }
}
