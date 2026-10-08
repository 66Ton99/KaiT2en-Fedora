// SPDX-License-Identifier: GPL-3.0-or-later
//! Private AKS client. Only the coordinated caller may open the native device.
//! Wire buffers and the terminal password stay in one locked, wiped mapping.
use anyhow::{Context, Result, bail, ensure};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use zeroize::{Zeroize, Zeroizing};

// Private-client exit status: capabilities were confirmed, but password input
// failed before load/alias/unlock. No other error may grant service restoration.
pub const KEYBAG_INPUT_ABORTED: i32 = 20;
#[derive(Debug)]
struct InputAbortedBeforeKeybag(anyhow::Error);
impl std::fmt::Display for InputAbortedBeforeKeybag {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:#}; no keybag operation was sent", self.0)
    }
}
impl std::error::Error for InputAbortedBeforeKeybag {}
pub fn keybag_exit_code(error: &anyhow::Error) -> i32 {
    if error.downcast_ref::<InputAbortedBeforeKeybag>().is_some() { KEYBAG_INPUT_ABORTED } else { 1 }
}
fn password_before_keybag(tty: &mut Terminal, memory: &mut SecretMemory, cancelled: fn() -> bool) -> Result<usize> {
    password(tty, memory, cancelled).map_err(|error| InputAbortedBeforeKeybag(error).into())
}

const CAPACITY: usize = 16384;
const HEADER: usize = 84;
const MAX_PASSWORD: usize = 1024;
const MEMORY_SIZE: usize = 2 * CAPACITY + MAX_PASSWORD + 1;
const LOCK_PATH: &str = "/run/kait2en-keybag/unlock.lock";
// Linux _IOWR(0xa7, 0, struct t2sep_exchange). Verified against the C UAPI in tests.
const EXCHANGE_IOCTL: libc::c_ulong = (3 << 30) | (32 << 16) | (0xa7 << 8);
#[repr(C)]
struct Exchange {
    endpoint: u8,
    operation: u8,
    reserved: [u8; 2],
    request_length: u32,
    response_capacity: u32,
    response_length: u32,
    request: u64,
    response: u64,
}
const _: () = assert!(std::mem::size_of::<Exchange>() == 32);

pub struct KeybagOptions { check: bool, path: PathBuf, uid: u32 }
impl KeybagOptions {
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Option<Self>> {
        let mut args: Vec<_> = args.into_iter().collect();
        if args == ["--help"] { return Ok(None); }
        let check = args.first().is_some_and(|arg| arg == "--check-inputs");
        if check { args.remove(0); }
        ensure!(args.len() == 2 && !args[0].as_encoded_bytes().starts_with(b"--"),
                "Usage: t2-keybag-unlock [--check-inputs] PRIVATE_KEYBAG MACOS_UID");
        let uid = crate::decimal_id(&args[1])?;
        crate::valid_uid(uid)?;
        Ok(Some(Self { check, path: args.remove(0).into(), uid }))
    }
}
fn open_bag(path: &Path) -> Result<File> {
    let file = OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path)
        .map_err(|error| crate::io_error(error, "Cannot open encrypted keybag", path, "Use the coordinated t2-touchid-unlock command with a private exported user.kb."))?;
    bag_size(&file)?;
    Ok(file)
}
fn bag_size(file: &File) -> Result<usize> {
    private_bag_size(&file.metadata()?, 0)
}
fn private_bag_size(info: &std::fs::Metadata, owner: u32) -> Result<usize> {
    ensure!(info.is_file() && info.uid() == owner && info.mode() & 0o077 == 0
            && info.nlink() == 1 && (1..=16000).contains(&info.len()),
            "Keybag must be a root-owned private regular file (mode 0600), 1..16000 bytes, without links.");
    Ok(info.len() as usize)
}
fn coordinated() -> Result<()> {
    // Do not take ownership of the inherited descriptor or close its shared lock.
    let mut inherited: libc::stat = unsafe { std::mem::zeroed() };
    ensure!(unsafe { libc::fstat(9, &mut inherited) } == 0,
            "Use the coordinated t2-touchid-unlock command, not this private helper.");
    let named = std::fs::symlink_metadata(LOCK_PATH)?;
    ensure!(named.is_file() && named.uid() == 0 && named.mode() & 0o077 == 0
            && inherited.st_mode & libc::S_IFMT == libc::S_IFREG && inherited.st_uid == 0
            && inherited.st_mode & 0o077 == 0 && inherited.st_dev == named.dev()
            && inherited.st_ino == named.ino()
            && unsafe { libc::flock(9, libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "Use the coordinated t2-touchid-unlock command, not this private helper.");
    Ok(())
}
fn disable_dumps() -> Result<()> {
    let limits = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    ensure!(unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limits) } == 0
            && unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) } == 0,
            "Cannot disable process dumps.");
    Ok(())
}
struct SecretMemory { pointer: NonNull<u8>, locked: bool }
impl SecretMemory {
    fn new() -> Result<Self> { Self::allocate(true) }
    fn allocate(lock: bool) -> Result<Self> {
        let raw = unsafe { libc::mmap(std::ptr::null_mut(), MEMORY_SIZE,
            libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0) };
        ensure!(raw != libc::MAP_FAILED, "Cannot allocate protected memory.");
        let mut memory = Self { pointer: NonNull::new(raw.cast()).context("Null mapping")?, locked: false };
        // Anonymous mmap is initialized to zero. No secret ever occupies a Vec or String.
        ensure!(unsafe { libc::madvise(raw, MEMORY_SIZE, libc::MADV_DONTDUMP) } == 0,
                "Cannot exclude protected memory from dumps.");
        if lock {
            ensure!(unsafe { libc::mlock(raw, MEMORY_SIZE) } == 0, "Cannot lock password memory.");
            memory.locked = true;
        }
        Ok(memory)
    }
    fn bytes(&mut self) -> &mut [u8] {
        // The uniquely owned mapping is live until Drop; all borrows are exclusive.
        unsafe { std::slice::from_raw_parts_mut(self.pointer.as_ptr(), MEMORY_SIZE) }
    }
    fn parts(&mut self) -> (&mut [u8], &mut [u8], &mut [u8]) {
        let (request, rest) = self.bytes().split_at_mut(CAPACITY);
        let (response, password) = rest.split_at_mut(CAPACITY);
        (request, response, password)
    }
}
impl Drop for SecretMemory {
    fn drop(&mut self) {
        self.bytes().zeroize();
        unsafe {
            if self.locked { libc::munlock(self.pointer.as_ptr().cast(), MEMORY_SIZE); }
            libc::munmap(self.pointer.as_ptr().cast(), MEMORY_SIZE);
        }
    }
}
fn le32(bytes: &[u8]) -> u32 { u32::from_le_bytes(bytes[..4].try_into().expect("bounded wire word")) }
fn put32(bytes: &mut [u8], value: u32) { bytes[..4].copy_from_slice(&value.to_le_bytes()); }
fn put64(bytes: &mut [u8], value: u64) { bytes[..8].copy_from_slice(&value.to_le_bytes()); }
fn owned_fd(raw: libc::c_int) -> Result<OwnedFd> {
    ensure!(raw >= 0, "Kernel SHA-256 socket: {}", std::io::Error::last_os_error());
    // A successful socket/accept creates a new uniquely owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}
fn send_hash(fd: &OwnedFd, bytes: &[u8], flags: libc::c_int) -> Result<()> {
    ensure!(unsafe { libc::send(fd.as_raw_fd(), bytes.as_ptr().cast(), bytes.len(), flags) } == bytes.len() as isize,
            "Kernel SHA-256 send failed.");
    Ok(())
}
fn wire_digest(wire: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    ensure!((HEADER..=CAPACITY).contains(&wire.len()) && le32(wire) == (HEADER - 4) as u32,
            "Invalid AKS envelope length");
    let version = le32(&wire[20..]);
    ensure!([1, 2].contains(&version), "Unsupported AKS envelope version");
    // Preserve the C client's AF_ALG hashing: secret request bytes are not copied
    // into a userspace hash object's buffering or allocator.
    let algorithm = owned_fd(unsafe { libc::socket(libc::AF_ALG, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0) })?;
    let mut address: libc::sockaddr_alg = unsafe { std::mem::zeroed() };
    address.salg_family = libc::AF_ALG as libc::sa_family_t;
    address.salg_type[..4].copy_from_slice(b"hash");
    address.salg_name[..6].copy_from_slice(b"sha256");
    ensure!(unsafe { libc::bind(algorithm.as_raw_fd(), (&address as *const libc::sockaddr_alg).cast(),
            std::mem::size_of_val(&address) as libc::socklen_t) } == 0, "Kernel SHA-256 bind failed.");
    let operation = owned_fd(unsafe { libc::accept4(algorithm.as_raw_fd(), std::ptr::null_mut(),
                                                  std::ptr::null_mut(), libc::SOCK_CLOEXEC) })?;
    if version == 1 {
        // The observed mixed 0x50/v1 envelope excludes calendar bytes 76..83.
        send_hash(&operation, &wire[20..76], libc::MSG_MORE)?;
        send_hash(&operation, &wire[HEADER..], 0)?;
    } else { send_hash(&operation, &wire[20..], 0)?; }
    let mut hash = Zeroizing::new([0u8; 32]);
    ensure!(unsafe { libc::read(operation.as_raw_fd(), hash.as_mut_ptr().cast(), hash.len()) } == 32,
            "Kernel SHA-256 read failed.");
    Ok(hash)
}
fn seal(wire: &mut [u8]) -> Result<()> {
    let hash = wire_digest(wire)?;
    wire[4..20].copy_from_slice(&hash[..16]);
    Ok(())
}
fn valid_reply(wire: &[u8], minimum: usize) -> Result<()> {
    ensure!(wire.len() >= HEADER && wire.len() - HEADER >= minimum, "Short AKS reply");
    let hash = wire_digest(wire)?;
    let difference = hash[..16].iter().zip(&wire[4..20]).fold(0u8, |result, (a, b)| result | (a ^ b));
    ensure!(difference == 0, "Invalid AKS integrity digest");
    Ok(())
}
trait Transport {
    fn transact(&mut self, operation: u8, request: &[u8], response: &mut [u8]) -> Result<usize>;
}
struct NativeTransport(File);
impl Transport for NativeTransport {
    fn transact(&mut self, operation: u8, request: &[u8], response: &mut [u8]) -> Result<usize> {
        let mut call = Exchange { endpoint: 7, operation, reserved: [0; 2],
            request_length: request.len() as u32, response_capacity: response.len() as u32,
            response_length: 0, request: request.as_ptr() as u64, response: response.as_mut_ptr() as u64 };
        ensure!(unsafe { libc::ioctl(self.0.as_raw_fd(), EXCHANGE_IOCTL, &mut call) } == 0,
                "SEP exchange failed ({}); outcome may be unknown. Do not retry: safe SEP recovery across reboot is not established.",
                std::io::Error::last_os_error());
        Ok(call.response_length as usize)
    }
}
fn clocks() -> Result<(u64, u64)> {
    let mut now: libc::timespec = unsafe { std::mem::zeroed() };
    ensure!(unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut now) } == 0, "Cannot read the boot clock.");
    let boot = (now.tv_sec as u64).checked_mul(1_000_000).and_then(|value| value.checked_add(now.tv_nsec as u64 / 1000))
        .context("Boot clock overflow")?;
    ensure!(unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut now) } == 0, "Cannot read the calendar clock.");
    Ok((boot, now.tv_sec as u64))
}
struct Client<'a, T> {
    memory: &'a mut SecretMemory,
    transport: &'a mut T,
    now: fn() -> Result<(u64, u64)>,
    cancelled: fn() -> bool,
}
impl<T: Transport> Client<'_, T> {
    fn body(&mut self, length: usize) -> Result<&mut [u8]> {
        ensure!(length <= CAPACITY - HEADER, "Request exceeds the transport capacity.");
        let (boot, calendar) = (self.now)()?;
        let (request, _, _) = self.memory.parts();
        request.zeroize();
        put32(request, (HEADER - 4) as u32);
        put32(&mut request[20..], 2);
        put64(&mut request[24..], boot);
        put64(&mut request[76..], calendar);
        Ok(&mut request[HEADER..HEADER + length])
    }
    fn exchange(&mut self, operation: u8, length: usize, minimum: usize) -> Result<&[u8]> {
        let (request, response, _) = self.memory.parts();
        let result = (|| {
            ensure!(!(self.cancelled)(), "Unlock cancelled; no further SEP requests will be sent.");
            ensure!(length <= CAPACITY - HEADER && minimum >= 4, "Invalid exchange bounds");
            seal(&mut request[..HEADER + length])?;
            response.zeroize();
            self.transport.transact(operation, &request[..HEADER + length], response)
        })();
        request.zeroize(); // Also wipe on timeout, digest failure or interruption.
        let size = result?;
        ensure!(size <= response.len(), "SEP reply exceeds transport capacity; no success is assumed.");
        valid_reply(&response[..size], minimum)
            .context("Invalid SEP reply; no success is assumed. Further SEP operations require investigation.")?;
        let reply = &response[HEADER..size];
        ensure!(le32(reply) == 0, "AppleKeyStore operation 0x{operation:02x} rejected (status 0x{:08x}); Touch ID remains unconfirmed.", le32(reply));
        Ok(reply)
    }
    fn capabilities(&mut self) -> Result<()> {
        put64(&mut self.body(16)?[4..], 1);
        let reply = self.exchange(0x4d, 16, 16)?;
        ensure!(le32(&reply[4..]) == 2 && le32(&reply[8..]) == 0, "Unsupported AppleKeyStore capabilities.");
        Ok(())
    }
    fn load_and_unlock(&mut self, bag: &mut impl Read, bag_size: usize, uid: u32, secret_size: usize) -> Result<()> {
        crate::valid_uid(uid)?;
        ensure!((1..=16000).contains(&bag_size) && (1..=MAX_PASSWORD).contains(&secret_size), "Invalid keybag/password size");
        let padded = (bag_size + 3) & !3;
        let data = self.body(16 + padded)?;
        put64(&mut data[4..], 1);
        put32(&mut data[12..], bag_size as u32);
        bag.read_exact(&mut data[16..16 + bag_size]).context("Cannot read the complete keybag.")?;
        let handle = le32(&self.exchange(0x03, 16 + padded, 8)?[4..]);
        ensure!((1..=i32::MAX as u32).contains(&handle), "Invalid loaded keybag handle.");
        let alias = 0u32.wrapping_sub(uid);
        let data = self.body(24)?;
        put64(&mut data[4..], 1);
        put32(&mut data[12..], handle);
        put32(&mut data[16..], alias);
        self.exchange(0x0d, 24, 4)?;
        for handle in [handle, alias] {
            let length = 24 + ((secret_size + 3) & !3);
            let data = self.body(length)?;
            put64(&mut data[4..], 1);
            put32(&mut data[12..], handle);
            put32(&mut data[20..], secret_size as u32);
            let (request, _, password) = self.memory.parts();
            request[HEADER + 24..HEADER + 24 + secret_size].copy_from_slice(&password[..secret_size]);
            self.exchange(0x04, length, 4)?;
        }
        Ok(())
    }
}
struct Terminal { file: File, original: libc::termios, changed: bool }
impl Terminal {
    fn open() -> Result<Self> {
        Self::hide(OpenOptions::new().read(true).write(true).custom_flags(libc::O_NOCTTY).open("/dev/tty")?)
    }
    fn hide(file: File) -> Result<Self> {
        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        ensure!(unsafe { libc::tcgetattr(file.as_raw_fd(), &mut original) } == 0,
                "An interactive controlling terminal is required.");
        let mut tty = Self { file, original, changed: false };
        let mut hidden = tty.original;
        hidden.c_lflag &= !(libc::ECHO | libc::ECHONL | libc::ICANON);
        hidden.c_cc[libc::VMIN] = 1;
        hidden.c_cc[libc::VTIME] = 0;
        ensure!(unsafe { libc::tcsetattr(tty.file.as_raw_fd(), libc::TCSAFLUSH, &hidden) } == 0,
                "Cannot hide terminal input.");
        tty.changed = true;
        Ok(tty)
    }
    fn restore(&mut self) -> Result<()> {
        if self.changed {
            ensure!(unsafe { libc::tcsetattr(self.file.as_raw_fd(), libc::TCSANOW, &self.original) } == 0,
                    "Cannot restore terminal settings.");
            self.changed = false;
        }
        Ok(())
    }
}
impl Drop for Terminal { fn drop(&mut self) { let _ = self.restore(); } }
fn password_byte(secret: &mut [u8], size: &mut usize, byte: u8) -> Result<bool> {
    match byte {
        b'\n' | b'\r' => { ensure!(*size != 0, "Empty password; nothing was unlocked."); return Ok(true); }
        4 => bail!("Password entry cancelled."),
        0x7f | 8 => {
            while *size > 0 {
                *size -= 1;
                let removed = secret[*size];
                secret[*size] = 0;
                if removed & 0xc0 != 0x80 { break; }
            }
        }
        _ => {
            ensure!(byte != 0 && *size < MAX_PASSWORD, "Password is invalid or too long.");
            secret[*size] = byte;
            *size += 1;
        }
    }
    Ok(false)
}
fn password(tty: &mut Terminal, memory: &mut SecretMemory, cancelled: fn() -> bool) -> Result<usize> {
    tty.file.write_all(b"macOS login password (not saved): ")?;
    let (_, _, secret) = memory.parts();
    let mut size = 0;
    let mut byte = Zeroizing::new([0u8; 1]);
    loop {
        ensure!(!cancelled(), "Password input cancelled; no keybag operation was sent.");
        // A libc read deliberately does not retry EINTR like Read::read_exact.
        ensure!(unsafe { libc::read(tty.file.as_raw_fd(), byte.as_mut_ptr().cast(), 1) } == 1,
                "Password input cancelled; no keybag operation was sent.");
        if password_byte(secret, &mut size, byte[0])? {
            ensure!(!cancelled(), "Password input cancelled; no keybag operation was sent.");
            tty.restore()?;
            tty.file.write_all(b"\n")?;
            return Ok(size);
        }
        byte.zeroize();
    }
}

pub fn run_keybag(options: &KeybagOptions) -> Result<()> {
    ensure!(unsafe { libc::geteuid() } == 0, "Run this private helper as root; see --help.");
    let mut bag = open_bag(&options.path)?;
    if options.check { return Ok(()); }
    coordinated()?;
    disable_dumps()?;
    let _signals = crate::os::install_client_signal_handlers()?;
    let mut memory = SecretMemory::new()?;
    let device = OpenOptions::new().read(true).write(true).custom_flags(libc::O_NOFOLLOW).open("/dev/t2sep").context("Cannot open /dev/t2sep; the coordinator must prepare the native transport before this private helper is used")?;
    ensure!(device.metadata()?.file_type().is_char_device() && device.metadata()?.uid() == 0,
            "Prepared root-owned /dev/t2sep is unavailable.");
    let mut transport = NativeTransport(device);
    {
        let mut client = Client { memory: &mut memory, transport: &mut transport, now: clocks, cancelled: crate::os::interrupted_now };
        // No password or keybag operation before this read-only protocol check.
        client.capabilities()?;
    }
    let secret_size = password_before_keybag(&mut Terminal::open()?, &mut memory, crate::os::interrupted_now)?;
    let size = bag_size(&bag)?;
    Client { memory: &mut memory, transport: &mut transport, now: clocks, cancelled: crate::os::interrupted_now }
        .load_and_unlock(&mut bag, size, options.uid, secret_size)?;
    drop(memory);
    println!("Both keybag unlock requests succeeded. Verify Touch ID separately with fprintd-verify.");
    Ok(())
}

#[cfg(test)]
#[path = "keybag_tests.rs"]
mod tests;
