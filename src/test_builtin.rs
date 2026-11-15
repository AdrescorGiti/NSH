//! POSIX.1-2024 test primaries and argument-count rules.
use std::cmp::Ordering;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};

use crate::{
    shell::{Flow, Shell, decimal, show},
    sys,
};

/// Undefined argument forms and extensions of the selected external utility
/// stay on that utility's normal dispatch path.
pub fn run(shell: &Shell, name: &[u8], args: &[Vec<u8>]) -> Option<Flow> {
    let args = if name == b"[" {
        let (last, args) = args.split_last()?;
        if last != b"]" {
            return None;
        }
        args
    } else {
        args
    };
    Some(match evaluate(shell, args)? {
        Ok(truth) => Flow::Status(i32::from(!truth)),
        Err(value) => shell.error(
            2,
            &format!("{}: invalid integer '{}'", show(name), show(value)),
            false,
        ),
    })
}

type Result<'a> = std::result::Result<bool, &'a [u8]>;

fn evaluate<'a>(shell: &Shell, args: &'a [Vec<u8>]) -> Option<Result<'a>> {
    match args {
        [] => Some(Ok(false)),
        [word] => Some(Ok(!word.is_empty())),
        [op, word] if op == b"!" => Some(Ok(word.is_empty())),
        [op, word] => unary(op, word).map(Ok),
        [lhs, op, rhs] => binary(shell, lhs, op, rhs)
            .or_else(|| (lhs == b"!").then(|| negate(shell, &args[1..])).flatten()),
        [first, ..] if args.len() == 4 && first == b"!" => negate(shell, &args[1..]),
        _ => None,
    }
}

fn negate<'a>(shell: &Shell, args: &'a [Vec<u8>]) -> Option<Result<'a>> {
    evaluate(shell, args).map(|value| value.map(|truth| !truth))
}

fn unary(op: &[u8], word: &[u8]) -> Option<bool> {
    let path = OsStr::from_bytes(word);
    Some(match op {
        b"-n" => !word.is_empty(),
        b"-z" => word.is_empty(),
        b"-t" => decimal(word).is_some_and(sys::tty),
        b"-h" | b"-L" => fs::symlink_metadata(path).is_ok_and(|m| m.is_symlink()),
        b"-r" | b"-w" | b"-x" if sys::privilege_matches() => {
            let permission = match op[1] {
                b'r' => libc::R_OK,
                b'w' => libc::W_OK,
                _ => libc::X_OK,
            };
            sys::access(word, permission)
        }
        b"-b" | b"-c" | b"-d" | b"-e" | b"-f" | b"-g" | b"-p" | b"-S" | b"-s" | b"-u" => {
            fs::metadata(path).is_ok_and(|m| match op[1] {
                b'b' => m.file_type().is_block_device(),
                b'c' => m.file_type().is_char_device(),
                b'd' => m.is_dir(),
                b'e' => true,
                b'f' => m.is_file(),
                b'g' => m.mode() & 0o2000 != 0,
                b'p' => m.file_type().is_fifo(),
                b'S' => m.file_type().is_socket(),
                b's' => m.len() != 0,
                b'u' => m.mode() & 0o4000 != 0,
                _ => unreachable!(),
            })
        }
        _ => return None,
    })
}

fn binary<'a>(shell: &Shell, lhs: &'a [u8], op: &[u8], rhs: &'a [u8]) -> Option<Result<'a>> {
    match op {
        b"=" | b"==" => Some(Ok(lhs == rhs)),
        b"!=" => Some(Ok(lhs != rhs)),
        b"<" | b">" if byte_collation(shell) => {
            Some(Ok(if op == b"<" { lhs < rhs } else { lhs > rhs }))
        }
        b"-ef" | b"-nt" | b"-ot" => {
            let left = fs::metadata(OsStr::from_bytes(lhs)).ok();
            let right = fs::metadata(OsStr::from_bytes(rhs)).ok();
            Some(Ok(match (left, right) {
                (Some(a), Some(b)) if op == b"-ef" => (a.dev(), a.ino()) == (b.dev(), b.ino()),
                (Some(a), Some(b)) => {
                    let order = (a.mtime(), a.mtime_nsec()).cmp(&(b.mtime(), b.mtime_nsec()));
                    if op == b"-nt" {
                        order.is_gt()
                    } else {
                        order.is_lt()
                    }
                }
                (Some(_), None) => op == b"-nt",
                (None, Some(_)) => op == b"-ot",
                (None, None) => false,
            }))
        }
        b"-eq" | b"-ne" | b"-lt" | b"-le" | b"-gt" | b"-ge" => {
            Some(compare_integer(lhs, rhs).map(|order| match op {
                b"-eq" => order.is_eq(),
                b"-ne" => !order.is_eq(),
                b"-lt" => order.is_lt(),
                b"-le" => !order.is_gt(),
                b"-gt" => order.is_gt(),
                _ => !order.is_lt(),
            }))
        }
        _ => None,
    }
}

fn byte_collation(shell: &Shell) -> bool {
    let locale = [b"LC_ALL".as_slice(), b"LC_COLLATE", b"LANG"]
        .iter()
        .find_map(|name| {
            let var = shell.vars.get(*name)?;
            var.value
                .as_deref()
                .filter(|s| var.exported && !s.is_empty())
        });
    locale.is_none_or(|locale| matches!(locale, b"C" | b"POSIX"))
}

fn compare_integer<'a>(lhs: &'a [u8], rhs: &'a [u8]) -> std::result::Result<Ordering, &'a [u8]> {
    let (a_sign, a) = signed_digits(lhs).ok_or(lhs)?;
    let (b_sign, b) = signed_digits(rhs).ok_or(rhs)?;
    if a_sign != b_sign {
        return Ok(b_sign.cmp(&a_sign));
    }
    let order = a.len().cmp(&b.len()).then_with(|| a.cmp(b));
    Ok(if a_sign { order.reverse() } else { order })
}

fn signed_digits(mut raw: &[u8]) -> Option<(bool, &[u8])> {
    let whitespace = |b| b" \t\r\n\x0b\x0c".contains(b);
    while raw.first().is_some_and(whitespace) {
        raw = &raw[1..];
    }
    while raw.last().is_some_and(whitespace) {
        raw = &raw[..raw.len() - 1];
    }
    let negative = raw.starts_with(b"-");
    if negative || raw.starts_with(b"+") {
        raw = &raw[1..];
    }
    if raw.is_empty() || !raw.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let start = raw.iter().position(|ch| *ch != b'0').unwrap_or(raw.len());
    Some((negative && start != raw.len(), &raw[start..]))
}
