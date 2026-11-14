// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::{Context, Result, bail, ensure};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;

pub(crate) const RISK_WARNING: &str = "WARNING: Manual SEP unlock is experimental. Previous native tests caused whole-machine shutdowns and a reported SEPD/MDMA panic. Save open work. The transport retains DMA until reboot; do not force-unload, reset or retry after an unconfirmed result.";

pub(crate) fn confirm_on_terminal() -> Result<()> {
    let mut tty = OpenOptions::new().read(true).write(true).open("/dev/tty")
        .context("Risk confirmation requires an interactive /dev/tty. Run from a terminal, or explicitly acknowledge the warning with --accept-risk")?;
    confirm_with(&mut tty, crate::os::interrupted_now)
}
fn confirm_with(tty: &mut File, cancelled: fn() -> bool) -> Result<()> {
    ensure!(unsafe { libc::isatty(tty.as_raw_fd()) } == 1,
        "Risk confirmation requires a terminal; use --accept-risk only to explicitly acknowledge the warning");
    tty.write_all(b"Continue with one manual SEP unlock? Type 'yes' to confirm [default: no]: ")
        .context("Cannot display risk confirmation prompt")?;
    tty.flush().context("Cannot flush risk confirmation prompt")?;
    let mut answer = Vec::new();
    loop {
        ensure!(!cancelled(), "Risk confirmation interrupted; manual unlock cancelled");
        let mut ready = libc::pollfd { fd: tty.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        let result = unsafe { libc::poll(&mut ready, 1, 100) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted { continue; }
            return Err(error).context("Cannot wait for risk confirmation; manual unlock cancelled");
        }
        if result == 0 { continue; }
        let mut byte = [0u8; 1];
        match tty.read(&mut byte) {
            Ok(0) => bail!("Risk confirmation ended without consent; manual unlock cancelled"),
            Ok(_) if byte[0] == b'\n' || byte[0] == b'\r' => break,
            Ok(_) => {
                ensure!(answer.len() < 32, "Risk confirmation is too long; manual unlock cancelled");
                answer.push(byte[0]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("Cannot read risk confirmation; manual unlock cancelled"),
        }
    }
    ensure!(answer == b"yes", "Risk not confirmed; manual unlock cancelled. Nothing was sent to SEP");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::FromRawFd;
    fn running() -> bool { false }
    fn cancelled() -> bool { true }
    fn terminal() -> (File, File) {
        let (mut master, mut slave) = (-1, -1);
        assert_eq!(unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(), std::ptr::null()) }, 0);
        unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) }
    }
    #[test]
    fn terminal_confirmation_accepts_explicit_yes() {
        let (mut master, mut slave) = terminal();
        master.write_all(b"yes\n").unwrap();
        confirm_with(&mut slave, running).unwrap();
    }
    #[test]
    fn terminal_confirmation_refuses_default_no_and_oversized_input() {
        for answer in [b"\n".as_slice(), b"no\n", b"y\n", b"YES\n", b"yes please\n", &[b'x'; 33]] {
            let (mut master, mut slave) = terminal();
            master.write_all(answer).unwrap();
            if !answer.ends_with(b"\n") { master.write_all(b"\n").unwrap(); }
            assert!(confirm_with(&mut slave, running).is_err());
        }
    }
    #[test]
    fn terminal_confirmation_handles_eof_and_interruption() {
        let (mut master, mut slave) = terminal();
        master.write_all(&[4]).unwrap();
        assert!(confirm_with(&mut slave, running).unwrap_err().to_string().contains("without consent"));
        let (_master, mut slave) = terminal();
        assert!(confirm_with(&mut slave, cancelled).unwrap_err().to_string().contains("interrupted"));
    }
    #[test]
    fn regular_file_cannot_confirm_risk() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"yes\n").unwrap();
        assert!(confirm_with(&mut file, running).is_err());
    }
}
