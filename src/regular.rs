//! Search for PATH-associated regular builtins without overriding an earlier executable.
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

use crate::{
    shell::{Flow, Shell},
    sys,
};

fn system_directory(mut path: &[u8]) -> bool {
    while let Some(directory) = path.strip_suffix(b"/") {
        path = directory;
    }
    matches!(path, b"/usr/bin" | b"/bin")
}

pub(crate) fn associated_path(path: &[u8]) -> bool {
    path.iter()
        .rposition(|&byte| byte == b'/')
        .is_some_and(|slash| system_directory(&path[..slash]))
}

pub(crate) fn run(shell: &Shell, name: &[u8], args: &[Vec<u8>]) -> Option<Flow> {
    match name {
        b"printf" => Some(crate::printf::run(shell, args)),
        b"test" | b"[" => crate::test_builtin::run(shell, name, args),
        _ => None,
    }
}

fn executable(path: &[u8]) -> bool {
    sys::access(path, libc::X_OK)
        && std::fs::metadata(OsStr::from_bytes(path)).is_ok_and(|metadata| metadata.is_file())
}

pub(crate) fn selected(shell: &Shell, name: &[u8]) -> bool {
    if name.contains(&b'/') {
        return false;
    }
    if let Some(cached) = shell.hashed_commands.get(name) {
        if associated_path(cached) {
            return true;
        }
        if executable(cached) {
            return false;
        }
    }
    let path = shell.get(b"PATH").unwrap_or(b"/usr/bin:/bin");
    let mut candidate = Vec::new();
    for directory in path.split(|&byte| byte == b':') {
        if system_directory(directory) {
            return true;
        }
        candidate.clear();
        if !directory.is_empty() {
            candidate.extend_from_slice(directory);
            candidate.push(b'/');
        }
        candidate.extend_from_slice(name);
        if executable(&candidate) {
            return false;
        }
    }
    false
}
