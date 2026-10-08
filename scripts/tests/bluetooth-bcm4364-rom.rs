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

const CANONICAL_NORMALIZATION: &str = "\tif (hdev->manufacturer == 15 && info->type > LE_ADV_SCAN_RSP)\n\t\tinfo->type &= 0x0f;";

fn extract_normalization(source: &str) -> Result<String, Box<dyn Error>> {
    // If it's a C file with hci_le_adv_report_evt:
    // verify the function is present and find its boundary.
    let search_scope = if let Some(start) = source.find("static void hci_le_adv_report_evt(") {
        let rest = &source[start..];
        let end = rest
            .find("static u8 ext_evt_type_to_legacy(")
            .or_else(|| {
                // Find column-0 closing brace after the function header
                let open_brace = rest.find('{')?;
                rest[open_brace..].find("\n}").map(|p| open_brace + p + 2)
            })
            .ok_or("Legacy advertising handler boundary is missing.")?;
        &rest[..end]
    } else if source.contains("hci_event.c") || source.contains("diff --git") || source.contains("--- a/") {
        // It's a patch file modifying hci_event.c
        source
    } else {
        return Err("Input must be a kernel hci_event.c source or patch file.".into());
    };

    // Find the normalization statements within search_scope regardless of indentation / plus signs
    let lines: Vec<&str> = search_scope.lines().collect();
    let mut cond_idx = None;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim().trim_start_matches('+').trim();
        if trimmed.contains("hdev->manufacturer == 15") && trimmed.contains("info->type > LE_ADV_SCAN_RSP") {
            cond_idx = Some(i);
            break;
        }
    }

    let idx = cond_idx.ok_or("Normalization condition (hdev->manufacturer == 15 && info->type > LE_ADV_SCAN_RSP) is missing.")?;

    let first_line = lines[idx].trim_start_matches('+').trim();
    if first_line.contains("info->type &= 0x0f") {
        Ok(format!("\t{first_line}"))
    } else {
        let next_line = lines.get(idx + 1)
            .ok_or("Expected statement after normalization condition.")?
            .trim_start_matches('+')
            .trim();
        if !next_line.contains("info->type &= 0x0f") {
            return Err("Normalization assignment (info->type &= 0x0f) is missing.".into());
        }
        Ok(format!("\t{first_line}\n\t\t{next_line}"))
    }
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
fn check(manufacturer: u16, raw: u8, expected: u8, rssi: i8, payload: [u8; 4]) {
    let mut hdev = HciDev { manufacturer };
    let mut info = Advertisement { event_type: raw, data: payload, rssi };
    // Both repr(C) values are live exclusive references with the C ABI layout.
    unsafe { normalize(&mut hdev, &mut info); }
    assert_eq!(info.event_type, expected, "manufacturer={manufacturer}, raw={raw:#04x}");
    assert_eq!(info.rssi, rssi, "RSSI modified for raw={raw:#04x}");
    assert_eq!(info.data, payload, "Payload modified for raw={raw:#04x}");
}

fn test_known_bcm4364_reports() {
    // Specific real-world patterns emitted by Broadcom ROM firmware:
    // 0x81 / 0x91 / 0xa1: ADV_DIRECT_IND (0x01) with vendor flags 0x80 / (0x80|0x10) / (0x80|0x20)
    check(15, 0x81, 0x01, -70, [10, 20, 30, 40]);
    check(15, 0x91, 0x01, -65, [10, 20, 30, 40]);
    check(15, 0xa1, 0x01, -60, [10, 20, 30, 40]);

    // 0x13: ADV_NONCONN_IND (0x03) with vendor bit 0x10
    check(15, 0x13, 0x03, -80, [1, 2, 3, 4]);

    // 0x14 / 0x24: LE_ADV_SCAN_RSP (0x04) with vendor bits 0x10 / 0x20
    check(15, 0x14, 0x04, -55, [5, 6, 7, 8]);
    check(15, 0x24, 0x04, -50, [5, 6, 7, 8]);

    // 0x20: ADV_IND (0x00) with vendor bit 0x20
    check(15, 0x20, 0x00, -75, [0, 0, 0, 0]);
}

fn test_edge_cases_and_immutability() {
    // Test boundary conditions and varying signal strength / payload boundaries
    let rssi_values = [-128i8, -100, -65, -1, 0, 1, 64, 127];
    let payloads = [
        [0u8, 0, 0, 0],
        [0xff, 0xff, 0xff, 0xff],
        [0xde, 0xad, 0xbe, 0xef],
    ];

    for &rssi in &rssi_values {
        for &payload in &payloads {
            // Clean valid types 0..=4 must be completely untouched
            for kind in 0u8..=4 {
                check(15, kind, kind, rssi, payload);
            }
            // Invalid low nibbles 5..15 must remain > 4
            check(15, 0x05, 0x05, rssi, payload);
            check(15, 0x15, 0x05, rssi, payload);
            check(15, 0x85, 0x05, rssi, payload);
        }
    }
}

fn main() {
    test_known_bcm4364_reports();
    test_edge_cases_and_immutability();

    // Every upper-nibble combination preserves valid types and leaves invalid
    // low nibbles invalid: cover all 256 Broadcom event bytes.
    for flags in (0u16..=0xf0).step_by(16) {
        for kind in 0u8..=4 { check(15, flags as u8 | kind, kind, -65, [1, 2, 3, 4]); }
        for invalid in 5u8..16 { check(15, flags as u8 | invalid, invalid, -65, [1, 2, 3, 4]); }
    }

    // All event bytes are unchanged for every other manufacturer.
    for manufacturer in 0..=u16::MAX {
        if manufacturer == 15 { continue; }
        for raw in 0..=u8::MAX { check(manufacturer, raw, raw, -65, [1, 2, 3, 4]); }
    }

    println!("BROADCOM_ROM_LE_REPORT_TESTS_PASS: all vendors/event bytes, valid/invalid types and payload preservation (Rust assertions; actual kernel C)");
}
"#;

fn exercise_c_block(block: &str) -> Result<(), Box<dyn Error>> {
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
    checked(Command::new("rustc").args(["--edition=2021", "-O"])
        .arg(&fixture).arg("-C").arg(format!("link-arg={}", object.display()))
        .arg("-o").arg(&executable))?;
    checked(&mut Command::new(&executable))
}

fn exercise_file(source_path: &Path) -> Result<(), Box<dyn Error>> {
    let source = fs::read_to_string(source_path)
        .map_err(|e| format!("Cannot read '{}': {}", source_path.display(), e))?;
    let block = extract_normalization(&source)?;
    exercise_c_block(&block)
}

fn find_repo_patch() -> Option<PathBuf> {
    let candidates = [
        "patches/runtime/bluetooth-bcm4364-rom/0001-Bluetooth-hci_event-normalize-Broadcom-ROM-advertising.patch",
        "../../patches/runtime/bluetooth-bcm4364-rom/0001-Bluetooth-hci_event-normalize-Broadcom-ROM-advertising.patch",
    ];
    for candidate in candidates {
        let p = PathBuf::from(candidate);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn main() {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--help" {
        println!("Usage: bluetooth-bcm4364-rom [PATH_TO_HCI_EVENT_C_OR_PATCH]\n\n\
                  Options:\n  \
                    --self-test    Run verification against canonical C block directly\n  \
                    --help         Show this help message\n\n\
                  If no path is provided, automatically discovers the repository patch.\n\
                  Requires rustc and cc; does not access Bluetooth hardware.");
        return;
    }

    let result = if args.is_empty() {
        if let Some(patch_path) = find_repo_patch() {
            println!("Testing repository patch: {}", patch_path.display());
            exercise_file(&patch_path)
        } else {
            Err("No file path provided and repository patch not found. See --help.".into())
        }
    } else if args.len() == 1 {
        if args[0] == "--self-test" {
            println!("Running self-test with canonical normalization block...");
            exercise_c_block(CANONICAL_NORMALIZATION)
        } else {
            exercise_file(Path::new(&args[0]))
        }
    } else {
        Err("Too many arguments; provide exactly one path. See --help.".into())
    };

    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
