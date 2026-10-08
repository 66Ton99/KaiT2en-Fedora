// SPDX-License-Identifier: GPL-3.0-or-later
fn main() {
    let result = (|| -> anyhow::Result<()> {
        anyhow::ensure!(std::env::args_os().len() == 1, "The private link-readiness helper accepts no arguments");
        let _signals = t2_manual_unlock::install_signal_handlers()?;
        t2_manual_unlock::run_link_ready()
    })();
    if let Err(error) = result { eprintln!("t2-touchid-link-ready: {error:#}"); std::process::exit(1); }
}
