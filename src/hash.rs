use std::borrow::Cow;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

use crate::builtins::builtin;
use crate::shell::{Flow, Shell, diagnostic, output, show};
use crate::sys;

pub fn run(shell: &mut Shell, args: &[Vec<u8>]) -> Flow {
    let mut operands = args;
    let mut clear = false;
    while let Some((first, remaining)) = operands.split_first() {
        if first == b"--" {
            operands = remaining;
            break;
        }
        if first.len() < 2 || !first.starts_with(b"-") {
            break;
        }
        if first[1..].iter().any(|option| *option != b'r') {
            return shell.error(2, "usage: hash [-r | utility ...]", false);
        }
        clear = true;
        operands = remaining;
    }
    if clear {
        if !operands.is_empty() {
            return shell.error(2, "usage: hash [-r | utility ...]", false);
        }
        shell.hashed_commands.clear();
        return Flow::Status(0);
    }
    if operands.is_empty() {
        let mut report = Vec::new();
        for (name, path) in &shell.hashed_commands {
            if !is_intrinsic(shell, name) {
                report.extend_from_slice(path);
                report.push(b'\n');
            }
        }
        return output(&report);
    }
    let mut status = 0;
    for name in operands {
        if is_intrinsic(shell, name) {
            continue;
        }
        let path = shell
            .search_paths(name)
            .find(|path| {
                sys::access(path, libc::X_OK)
                    && std::fs::metadata(OsStr::from_bytes(path)).is_ok_and(|info| info.is_file())
            })
            .map(Cow::into_owned);
        if let Some(path) = path {
            if !name.contains(&b'/') {
                shell.hashed_commands.insert(name.clone(), path);
            }
        } else {
            shell.hashed_commands.remove(name);
            diagnostic(&format!("hash: {}: not found", show(name)));
            status = 1;
        }
    }
    Flow::Status(status)
}

pub fn search<'a>(shell: &'a Shell, name: &'a [u8]) -> impl Iterator<Item = Cow<'a, [u8]>> {
    let cached = shell.hashed_commands.get(name).map(Vec::as_slice);
    let mut duplicate = cached;
    cached
        .map(Cow::Borrowed)
        .into_iter()
        .chain(shell.search_paths(name).filter(move |path| {
            if duplicate == Some(path.as_ref()) {
                duplicate = None;
                false
            } else {
                true
            }
        }))
}

fn is_intrinsic(shell: &Shell, name: &[u8]) -> bool {
    builtin(name) || shell.functions.contains_key(name) || name == b"nsh" && shell.config
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell() -> Shell {
        let mut shell = Shell::new(
            b"nsh".to_vec(),
            Vec::new(),
            Default::default(),
            false,
            false,
            false,
        );
        shell
            .assign(b"PATH", b"/usr/bin:/bin".to_vec(), false)
            .unwrap();
        shell
    }

    #[test]
    fn remembers_external_location_and_forgets_it() {
        let mut shell = shell();
        assert_eq!(run(&mut shell, &[b"cat".to_vec()]).code(), 0);
        assert_eq!(shell.hashed_commands[b"cat".as_slice()], b"/usr/bin/cat");
        assert_eq!(run(&mut shell, &[b"-r".to_vec(), b"--".to_vec()]).code(), 0);
        assert!(shell.hashed_commands.is_empty());
    }

    #[test]
    fn omits_builtins_and_functions() {
        let mut shell = shell();
        assert_eq!(shell.code(b"cat() { :; }\n", false).code(), 0);
        assert_eq!(
            run(&mut shell, &[b"pwd".to_vec(), b"cat".to_vec()]).code(),
            0
        );
        assert!(shell.hashed_commands.is_empty());
    }

    #[test]
    fn readonly_path_assignment_preserves_the_table() {
        let mut shell = shell();
        assert_eq!(run(&mut shell, &[b"cat".to_vec()]).code(), 0);
        shell.interactive = true;
        assert_eq!(shell.code(b"readonly PATH", false).code(), 0);
        assert_eq!(shell.code(b"PATH=/invalid hash cat", false).code(), 1);
        assert_eq!(shell.get(b"PATH"), Some(b"/usr/bin:/bin".as_slice()));
        assert_eq!(shell.hashed_commands[b"cat".as_slice()], b"/usr/bin/cat");
    }

    #[test]
    fn path_assignment_clears_the_table() {
        let mut shell = shell();
        assert_eq!(run(&mut shell, &[b"cat".to_vec()]).code(), 0);
        assert_eq!(shell.code(br#"PATH="$PATH""#, false).code(), 0);
        assert!(shell.hashed_commands.is_empty());
    }

    #[test]
    fn rejects_invalid_options_and_missing_ordinary_utility() {
        let mut shell = shell();
        assert_eq!(run(&mut shell, &[b"-q".to_vec()]).code(), 2);
        assert_eq!(
            run(&mut shell, &[b"nsh-hash-fixture-missing-28471".to_vec()]).code(),
            1
        );
        assert!(shell.hashed_commands.is_empty());
    }
}
