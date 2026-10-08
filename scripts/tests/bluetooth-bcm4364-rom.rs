// SPDX-License-Identifier: GPL-3.0-or-later
// Compile the actual kernel C normalization; run all assertions in Rust.
use std::env;
use std::error::Error;
use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const NORMALIZATION: &str = "\t\tif (hdev->manufacturer == 15 && info->type > LE_ADV_SCAN_RSP)\n\t\t\tinfo->type &= 0x0f;";

fn normalization(source: &str) -> Result<&str, Box<dyn Error>> {
    let start = source.find("static void hci_le_adv_report_evt(")
        .ok_or("Legacy advertising handler is missing.")?;
    let rest = &source[start..];
    let end = rest.find("static u8 ext_evt_type_to_legacy(")
        .ok_or("Legacy advertising handler boundary is missing.")?;
    let handler = &rest[..end];
    let offset = handler.find(NORMALIZATION)
        .ok_or("Normalization is missing from the legacy advertising handler.")?;
    Ok(&handler[offset..offset + NORMALIZATION.len()])
}

struct WorkDir(PathBuf);
impl WorkDir {
    fn new() -> io::Result<Self> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?.as_nanos();
        for attempt in 0..100 {
            let path = env::temp_dir().join(format!(
                "bcm4364-rust-tests-{}-{stamp}-{attempt}", std::process::id()));
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(io::ErrorKind::AlreadyExists, "Cannot create private test directory."))
    }
}
impl Drop for WorkDir {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

fn checked(command: &mut Command) -> Result<(), Box<dyn Error>> {
    let status = command.status()?;
    if !status.success() {
        return Err(format!("Test command failed: {status}").into());
    }
    Ok(())
}

// No normalization logic is duplicated here: this calls the object compiled
// from the extracted C statements and asserts the required packet invariants.
const RUST_FIXTURE: &str = r#"
#[repr(C)]
struct HciDev { manufacturer: u16 }
#[repr(C)]
struct Advertisement { event_type: u8, data: [u8; 4], rssi: i8 }
unsafe extern "C" {
    fn normalize(hdev: *mut HciDev, info: *mut Advertisement);
}
fn check(manufacturer: u16, raw: u8, expected: u8) {
    let mut hdev = HciDev { manufacturer };
    let mut info = Advertisement { event_type: raw, data: [1, 2, 3, 4], rssi: -65 };
    // Both repr(C) values are live exclusive references with the C ABI layout.
    unsafe { normalize(&mut hdev, &mut info); }
    assert_eq!(info.event_type, expected, "manufacturer={manufacturer}, raw={raw:#04x}");
    assert_eq!(info.rssi, -65);
    assert_eq!(info.data, [1, 2, 3, 4]);
}
fn main() {
    // Every upper-nibble combination preserves valid types and leaves invalid
    // low nibbles invalid: cover all 256 Broadcom event bytes.
    for flags in (0u16..=0xf0).step_by(16) {
        for kind in 0u8..=4 { check(15, flags as u8 | kind, kind); }
        for invalid in 5u8..16 { check(15, flags as u8 | invalid, invalid); }
    }
    // All event bytes are unchanged for every other manufacturer.
    for manufacturer in 0..=u16::MAX {
        if manufacturer == 15 { continue; }
        for raw in 0..=u8::MAX { check(manufacturer, raw, raw); }
    }
    println!("BROADCOM_ROM_LE_REPORT_TESTS_PASS: all vendors/event bytes, valid/invalid types and payload preservation (Rust assertions; actual kernel C)");
}
"#;

fn exercise(source: &Path) -> Result<(), Box<dyn Error>> {
    let source = fs::read_to_string(source)?;
    let block = normalization(&source)?;
    let work = WorkDir::new()?;
    let c_source = work.0.join("normalize.c");
    let object = work.0.join("normalize.o");
    let fixture = work.0.join("fixture.rs");
    let executable = work.0.join("fixture");
    fs::write(&c_source, format!(
        "#include <stdint.h>\n#define LE_ADV_SCAN_RSP 4\n\
         struct hci_dev {{ uint16_t manufacturer; }};\n\
         struct advertisement {{ uint8_t type; uint8_t data[4]; int8_t rssi; }};\n\
         void normalize(struct hci_dev *hdev, struct advertisement *info) {{\n{block}\n}}\n"))?;
    fs::write(&fixture, RUST_FIXTURE)?;
    checked(Command::new("cc").args(["-O2", "-Wall", "-Wextra", "-Werror", "-c"])
        .arg(&c_source).arg("-o").arg(&object))?;
    checked(Command::new("rustc").args(["--edition=2024", "-O"])
        .arg(&fixture).arg("-C").arg(format!("link-arg={}", object.display()))
        .arg("-o").arg(&executable))?;
    checked(&mut Command::new(&executable))
}

fn main() {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--help" {
        println!("Usage: bluetooth-bcm4364-rom /path/to/kernel/net/bluetooth/hci_event.c\nRequires rustc and cc; does not access Bluetooth hardware.");
        return;
    }
    let result = if args.len() == 1 {
        exercise(Path::new(&args[0]))
    } else {
        Err("Provide exactly one kernel hci_event.c path; see --help.".into())
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
