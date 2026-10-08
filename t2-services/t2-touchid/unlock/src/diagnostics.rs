// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::Error;
use std::io;
use std::path::Path;

// Keep the original OS error in the chain, but always identify the operation,
// path and next setup step. Never include file contents or secret input.
pub(crate) fn io_error(error: io::Error, action: &str, path: &Path, remedy: &str) -> Error {
    let reason = match error.kind() {
        io::ErrorKind::NotFound => "required file or directory is missing",
        io::ErrorKind::PermissionDenied => "access denied; check ownership, permissions and security policy",
        io::ErrorKind::AlreadyExists => "path already exists; it will not be overwritten",
        io::ErrorKind::NotADirectory => "a path component is not a directory",
        io::ErrorKind::IsADirectory => "expected a file, but found a directory",
        _ => "operating-system operation failed",
    };
    Error::new(error).context(format!("{action} '{}': {reason}. {remedy}", path.display()))
}
