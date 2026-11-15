// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use std::io::Cursor;
use std::process::Command;

fn fixed_clocks() -> Result<(u64, u64)> { Ok((123456, 123)) }
fn running() -> bool { false }
fn cancelled() -> bool { true }
fn envelope(version: u32, size: usize) -> Vec<u8> {
    let mut wire = vec![0; HEADER + size];
    put32(&mut wire, (HEADER - 4) as u32);
    put32(&mut wire[20..], version);
    wire
}
#[test]
fn v2_digest_matches_original_c_golden_and_covers_every_byte() {
    let mut wire = envelope(2, 16);
    put64(&mut wire[HEADER + 4..], 2);
    seal(&mut wire).unwrap();
    assert_eq!(&wire[4..20], &[0x9d,0x82,0x58,0xba,0x98,0x43,0xb7,0x84,0xa8,0x34,0x76,0x16,0xe0,0x4b,0x00,0xd5]);
    valid_reply(&wire, 16).unwrap();
    for index in 0..wire.len() {
        wire[index] ^= 1;
        assert!(valid_reply(&wire, 16).is_err(), "unprotected byte {index}");
        wire[index] ^= 1;
    }
}
#[test]
fn mixed_v1_digest_matches_c_golden_excluding_only_calendar() {
    let mut wire = envelope(1, 16);
    put64(&mut wire[HEADER + 4..], 2);
    put64(&mut wire[76..], 0x0102030405060708);
    seal(&mut wire).unwrap();
    assert_eq!(&wire[4..20], &[0x2b,0xfa,0x94,0xed,0x3f,0xd2,0xa5,0x0c,0x71,0x68,0xb5,0x43,0x3b,0x5b,0x51,0x9d]);
    for index in 0..wire.len() {
        wire[index] ^= 1;
        assert_eq!(valid_reply(&wire, 16).is_ok(), (76..84).contains(&index), "byte {index}");
        wire[index] ^= 1;
    }
}
#[test]
fn reply_and_request_bounds_and_versions_fail_closed() {
    for size in [0, HEADER - 1, CAPACITY + 1] {
        assert!(valid_reply(&vec![0; size], 4).is_err());
    }
    let mut wire = envelope(2, 16);
    seal(&mut wire).unwrap();
    assert!(valid_reply(&wire, 17).is_err());
    for version in [0, 3, u32::MAX] {
        put32(&mut wire[20..], version);
        assert!(seal(&mut wire).is_err());
        assert!(valid_reply(&wire, 4).is_err());
    }
    let mut memory = SecretMemory::allocate(false).unwrap();
    let mut peer = Peer::default();
    let mut client = Client { memory: &mut memory, transport: &mut peer, now: fixed_clocks, cancelled: running };
    assert!(client.body(CAPACITY - HEADER + 1).is_err());
    assert!(client.exchange(3, usize::MAX, 4).is_err());
    assert!(peer.operations.is_empty());
}
#[derive(Clone, Copy, Default)]
enum Fault { #[default] None, Timeout, Rejected, Digest, Oversize, Short, Version, Handle(u32), Capability(u64) }
#[derive(Default)]
struct Peer { operations: Vec<u8>, fault: Fault, fail_at: usize, mixed: bool, workflow: bool }
impl Transport for Peer {
    fn transact(&mut self, operation: u8, request: &[u8], response: &mut [u8]) -> Result<usize> {
        valid_reply(request, 0).unwrap();
        assert_eq!(le32(&request[20..]), 2);
        assert_eq!(u64::from_le_bytes(request[24..32].try_into().unwrap()), 123456);
        assert_eq!(u64::from_le_bytes(request[76..84].try_into().unwrap()), 123);
        if self.workflow {
            // Original C fixtures: load, alias, unlock loaded handle, unlock UID alias.
            const LOADED: &[u8] = &[0,0,0,0,1,0,0,0,0,0,0,0,3,0,0,0,0xa1,0xb2,0xc3,0];
            const ALIAS: &[u8] = &[0,0,0,0,1,0,0,0,0,0,0,0,7,0,0,0,0x0b,0xfe,0xff,0xff,0,0,0,0];
            const NORMAL: &[u8] = &[0,0,0,0,1,0,0,0,0,0,0,0,7,0,0,0,0,0,0,0,3,0,0,0,0x61,0x62,0x63,0];
            const SPECIAL: &[u8] = &[0,0,0,0,1,0,0,0,0,0,0,0,0x0b,0xfe,0xff,0xff,0,0,0,0,3,0,0,0,0x61,0x62,0x63,0];
            let index = self.operations.len();
            assert_eq!(operation, [3, 0x0d, 4, 4][index]);
            assert_eq!(&request[HEADER..], [LOADED, ALIAS, NORMAL, SPECIAL][index]);
        }
        let fault = if self.operations.len() == self.fail_at { self.fault } else { Fault::None };
        self.operations.push(operation);
        if matches!(fault, Fault::Timeout) { bail!("fixture timeout; unconfirmed"); }
        let size = HEADER + match operation { 0x4d => 16, 3 => 8, _ => 4 };
        response[..size].fill(0);
        put32(response, (HEADER - 4) as u32);
        put32(&mut response[20..], if self.mixed { 1 } else { 2 });
        if operation == 0x4d { put64(&mut response[HEADER + 4..], if let Fault::Capability(value) = fault { value } else { 2 }); }
        if operation == 3 { put32(&mut response[HEADER + 4..], if let Fault::Handle(value) = fault { value } else { 7 }); }
        if matches!(fault, Fault::Rejected) { put32(&mut response[HEADER..], 1); }
        seal(&mut response[..size]).unwrap();
        if matches!(fault, Fault::Digest) { response[4] ^= 1; }
        if matches!(fault, Fault::Version) { put32(&mut response[20..], 3); }
        Ok(match fault { Fault::Oversize => CAPACITY + 1, Fault::Short => HEADER + 3, _ => size })
    }
}
fn workflow(peer: &mut Peer, memory: &mut SecretMemory) -> Result<()> {
    memory.parts().2[..3].copy_from_slice(b"abc");
    Client { memory, transport: peer, now: fixed_clocks, cancelled: running }
        .load_and_unlock(&mut Cursor::new([0xa1, 0xb2, 0xc3]), 3, 501, 3)
}
#[test]
fn exact_load_alias_two_unlock_sequence_matches_c_for_v1_and_v2() {
    for mixed in [false, true] {
        let mut memory = SecretMemory::allocate(false).unwrap();
        let mut peer = Peer { workflow: true, mixed, ..Peer::default() };
        workflow(&mut peer, &mut memory).unwrap();
        assert_eq!(peer.operations, [3, 0x0d, 4, 4]);
        assert!(memory.parts().0.iter().all(|byte| *byte == 0));
        assert_eq!(&memory.parts().2[..3], b"abc");
        memory.bytes().zeroize();
        assert!(memory.bytes().iter().all(|byte| *byte == 0));
    }
}
#[test]
fn all_invalid_exchanges_stop_without_retry_and_wipe_request() {
    for fault in [Fault::Timeout, Fault::Rejected, Fault::Digest, Fault::Oversize, Fault::Short, Fault::Version] {
        for fail_at in 0..4 {
            let mut peer = Peer { fault, fail_at, workflow: true, ..Peer::default() };
            let mut memory = SecretMemory::allocate(false).unwrap();
            assert!(workflow(&mut peer, &mut memory).is_err());
            assert_eq!(peer.operations.len(), fail_at + 1);
            assert!(memory.parts().0.iter().all(|byte| *byte == 0));
        }
    }
}
#[test]
fn invalid_handles_stop_before_alias_or_password_unlock() {
    for handle in [0, i32::MAX as u32 + 1, u32::MAX] {
        let mut peer = Peer { fault: Fault::Handle(handle), ..Peer::default() };
        let mut memory = SecretMemory::allocate(false).unwrap();
        assert!(workflow(&mut peer, &mut memory).is_err());
        assert_eq!(peer.operations, [3]);
    }
}
#[test]
fn capabilities_require_exact_version_two_before_password() {
    for value in [0, 1, 3, 0x100000002] {
        let mut memory = SecretMemory::allocate(false).unwrap();
        let mut peer = Peer { fault: Fault::Capability(value), ..Peer::default() };
        assert!(Client { memory: &mut memory, transport: &mut peer, now: fixed_clocks, cancelled: running }.capabilities().is_err());
        assert_eq!(peer.operations, [0x4d]);
    }
    let mut memory = SecretMemory::allocate(false).unwrap();
    let mut peer = Peer::default();
    Client { memory: &mut memory, transport: &mut peer, now: fixed_clocks, cancelled: running }.capabilities().unwrap();
}
#[test]
fn interrupted_client_sends_no_native_request() {
    let mut memory = SecretMemory::allocate(false).unwrap();
    let mut peer = Peer::default();
    assert!(Client { memory: &mut memory, transport: &mut peer, now: fixed_clocks, cancelled }.capabilities().is_err());
    assert!(peer.operations.is_empty());
    assert!(memory.parts().0.iter().all(|byte| *byte == 0));
}
#[test]
fn truncated_keybag_and_invalid_input_sizes_send_nothing() {
    for (bag_size, password_size) in [(0, 3), (16001, 3), (3, 0), (3, 1025), (4, 3)] {
        let mut memory = SecretMemory::allocate(false).unwrap();
        let mut peer = Peer::default();
        assert!(Client { memory: &mut memory, transport: &mut peer, now: fixed_clocks, cancelled: running }
            .load_and_unlock(&mut Cursor::new([1,2,3]), bag_size, 501, password_size).is_err());
        assert!(peer.operations.is_empty());
    }
}
#[test]
fn utf8_backspace_and_password_limits_match_hidden_input_contract() {
    let mut secret = [0; MAX_PASSWORD + 1];
    let mut size = 0;
    for byte in "aї🙂".bytes() { assert!(!password_byte(&mut secret, &mut size, byte).unwrap()); }
    password_byte(&mut secret, &mut size, 0x7f).unwrap();
    assert_eq!(&secret[..size], "aї".as_bytes());
    assert!(secret[size..].iter().all(|byte| *byte == 0));
    password_byte(&mut secret, &mut size, 8).unwrap();
    assert_eq!(&secret[..size], b"a");
    assert!(password_byte(&mut secret, &mut size, b'\n').unwrap());
    size = 0;
    assert!(password_byte(&mut secret, &mut size, b'\n').is_err());
    assert!(password_byte(&mut secret, &mut size, 0).is_err());
    assert!(password_byte(&mut secret, &mut size, 4).is_err());
    for _ in 0..MAX_PASSWORD { password_byte(&mut secret, &mut size, b'x').unwrap(); }
    assert!(password_byte(&mut secret, &mut size, b'y').is_err());
    assert!(password_byte(&mut secret, &mut size, b'\n').unwrap());
}
fn pty() -> (File, File) {
    let (mut master, mut slave) = (-1, -1);
    assert_eq!(unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(), std::ptr::null()) }, 0);
    unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) }
}
fn flags(file: &File) -> libc::tcflag_t {
    let mut settings: libc::termios = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::tcgetattr(file.as_raw_fd(), &mut settings) }, 0);
    settings.c_lflag
}
#[test]
fn terminal_hides_echo_and_restores_on_normal_error_and_cancellation() {
    for cancel in [false, true] {
        let (mut master, slave) = pty();
        let original = flags(&slave);
        let probe = slave.try_clone().unwrap();
        let mut tty = Terminal::hide(slave).unwrap();
        assert_eq!(flags(&probe) & (libc::ECHO | libc::ECHONL | libc::ICANON), 0);
        let writer = std::thread::spawn(move || {
            let mut prompt = vec![0; b"macOS login password (not saved): ".len()];
            master.read_exact(&mut prompt).unwrap();
            if !cancel { master.write_all(b"abc\n").unwrap(); }
            // Keep the PTY open while the reader consumes its input.
            master
        });
        let mut memory = SecretMemory::allocate(false).unwrap();
        let result = password(&mut tty, &mut memory, if cancel { cancelled } else { running });
        if cancel { assert!(result.is_err()); } else {
            assert_eq!(result.unwrap(), 3);
            assert_eq!(&memory.parts().2[..3], b"abc");
        }
        drop(tty);
        assert_eq!(flags(&probe), original);
        let master = writer.join().unwrap();
        drop(master);
    }
    let (_master, slave) = pty();
    let original = flags(&slave);
    let probe = slave.try_clone().unwrap();
    { let _tty = Terminal::hide(slave).unwrap(); } // Error unwinding path.
    assert_eq!(flags(&probe), original);
}
#[test]
fn no_nonterminal_password_input_or_secret_cli_arguments() {
    let temporary = tempfile::tempfile().unwrap();
    assert!(Terminal::hide(temporary).is_err());
    for args in [vec!["user.kb", "501", "password"], vec!["--password", "secret"],
                 vec!["user.kb", "auto"], vec!["user.kb", "500"], vec!["user.kb", "-501"],
                 vec!["user.kb", "2147483648"], vec!["user.kb", "501x"]] {
        assert!(KeybagOptions::parse(args.into_iter().map(OsString::from)).is_err());
    }
    for uid in ["501", "2147483647"] {
        assert!(KeybagOptions::parse(["user.kb", uid].map(OsString::from)).is_ok());
    }
    assert!(KeybagOptions::parse(["--check-inputs", "user.kb", "501"].map(OsString::from)).unwrap().unwrap().check);
}
#[test]
fn ioctl_layout_matches_the_actual_kernel_uapi() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("abi.c");
    let binary = temporary.path().join("abi");
    let header = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../modules/t2sep/t2sep_uapi.h");
    std::fs::write(&source, format!("#include <stdio.h>\n#include <stddef.h>\n#include \"{}\"\nint main(void) {{ printf(\"%zu %lu %zu %zu %zu %zu\\n\", sizeof(struct t2sep_exchange), (unsigned long)T2SEP_IOC_EXCHANGE, offsetof(struct t2sep_exchange, request_length), offsetof(struct t2sep_exchange, response_length), offsetof(struct t2sep_exchange, request), offsetof(struct t2sep_exchange, response)); }}\n", header.display())).unwrap();
    assert!(Command::new("cc").args(["-Wall", "-Werror"]).arg(&source).arg("-o").arg(&binary).status().unwrap().success());
    let output = Command::new(&binary).output().unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), format!("{} {} {} {} {} {}", std::mem::size_of::<Exchange>(), EXCHANGE_IOCTL,
        std::mem::offset_of!(Exchange, request_length), std::mem::offset_of!(Exchange, response_length),
        std::mem::offset_of!(Exchange, request), std::mem::offset_of!(Exchange, response)));
}

#[test]
fn private_keybag_metadata_and_no_follow_rules_fail_closed() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("user.kb");
    std::fs::write(&path, b"fabricated").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let info = std::fs::metadata(&path).unwrap();
    let owner = info.uid();
    assert_eq!(private_bag_size(&info, owner).unwrap(), 10);
    assert!(private_bag_size(&info, owner.wrapping_add(1)).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    assert!(private_bag_size(&std::fs::metadata(&path).unwrap(), owner).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let link = directory.path().join("hardlink");
    std::fs::hard_link(&path, &link).unwrap();
    assert!(private_bag_size(&std::fs::metadata(&path).unwrap(), owner).is_err());
    std::fs::remove_file(link).unwrap();
    let link = directory.path().join("symlink");
    symlink(&path, &link).unwrap();
    let error = open_bag(&link).unwrap_err();
    assert_eq!(error.downcast_ref::<std::io::Error>().unwrap().raw_os_error(), Some(libc::ELOOP));
    for size in [0, 16001] {
        File::options().write(true).open(&path).unwrap().set_len(size).unwrap();
        assert!(private_bag_size(&std::fs::metadata(&path).unwrap(), owner).is_err());
    }
}

#[test]
fn password_cancellation_has_explicit_before_keybag_status_without_an_unlock_request() {
    let (_master, slave) = pty();
    let mut tty = Terminal::hide(slave).unwrap();
    let mut memory = SecretMemory::allocate(false).unwrap();
    let mut peer = Peer::default();
    Client { memory: &mut memory, transport: &mut peer, now: fixed_clocks, cancelled: running }
        .capabilities().unwrap();
    let error = password_before_keybag(&mut tty, &mut memory, cancelled).unwrap_err();
    assert_eq!(keybag_exit_code(&error), KEYBAG_INPUT_ABORTED);
    assert_eq!(peer.operations, [0x4d]);
    assert!(memory.parts().2.iter().all(|byte| *byte == 0));
    assert_eq!(keybag_exit_code(&anyhow::anyhow!("unconfirmed native error")), 1);
}
