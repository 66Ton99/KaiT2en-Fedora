// SPDX-License-Identifier: GPL-3.0-or-later
use t2_manual_unlock::{KeybagOptions, keybag_exit_code, run_keybag};
fn main() {
    let result = (|| -> anyhow::Result<()> {
        let Some(options) = KeybagOptions::parse(std::env::args_os().skip(1))? else {
            println!("Usage: t2-keybag-unlock [--check-inputs] PRIVATE_KEYBAG MACOS_UID\nRequires a prepared /dev/t2sep and inherited coordinator lock. Never accepts a password in arguments or stdin.");
            return Ok(());
        };
        run_keybag(&options)
    })();
    if let Err(error) = result { eprintln!("{error:#}"); std::process::exit(keybag_exit_code(&error)); }
}
