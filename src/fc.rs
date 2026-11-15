//! POSIX-style editing and replay of the interactive command history.
use crate::shell::{Flow, Shell, diagnostic, output};
use std::ffi::CStr;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::{
    ffi::OsStrExt,
    fs::{MetadataExt, OpenOptionsExt},
};
use std::path::PathBuf;

const LIMIT: u64 = 32 * 1024 * 1024;

pub fn run(shell: &mut Shell, args: &[Vec<u8>], tested: bool) -> Flow {
    let invocation = match Invocation::parse(args) {
        Ok(invocation) => invocation,
        Err(message) => return shell.error(2, &format!("fc: {message}"), false),
    };
    if !invocation.list {
        shell.suppress_history = true;
    }
    let history = shell.session.history();
    if history.is_empty() {
        return shell.error(1, "fc: no previous commands", false);
    }
    let selected = match invocation.select(history) {
        Ok(selected) => selected,
        Err(message) => return shell.error(1, &format!("fc: {message}"), false),
    };
    if invocation.list {
        let mut listing = Vec::new();
        for index in selected {
            for (part, line) in history[index].split(|byte| *byte == b'\n').enumerate() {
                if part == 0 && !invocation.no_numbers {
                    listing.extend_from_slice((index + 1).to_string().as_bytes());
                }
                listing.push(b'\t');
                listing.extend_from_slice(line);
                listing.push(b'\n');
            }
        }
        return output(&listing);
    }
    if shell.call_depth >= 64 {
        return shell.error(2, "fc: too many nested command replays", false);
    }
    let source = if invocation.repeat {
        let mut command = history[selected[0]].clone();
        if let Some((old, new)) = invocation.replacement {
            let Some(index) = find(&command, old) else {
                return shell.error(1, "fc: old string not found in selected command", false);
            };
            command.splice(index..index + old.len(), new.iter().copied());
        }
        command
    } else {
        let mut text = Vec::new();
        for index in selected {
            text.extend_from_slice(&history[index]);
            text.push(b'\n');
        }
        let editor = invocation
            .editor
            .or_else(|| shell.get(b"FCEDIT").filter(|name| !name.is_empty()))
            .unwrap_or(b"ed")
            .to_vec();
        let file = match EditFile::new(&text) {
            Ok(file) => file,
            Err(error) => return shell.error(1, &format!("fc: temporary file: {error}"), false),
        };
        let args = [editor, file.path.as_os_str().as_bytes().to_vec()];
        let status = shell.spawn_external(&args, false);
        if !matches!(status, Flow::Status(0)) {
            return status;
        }
        match file.read() {
            Ok(edited) => edited,
            Err(error) => {
                return shell.error(1, &format!("fc: reading edited file: {error}"), false);
            }
        }
    };
    let entry = source.strip_suffix(b"\n").unwrap_or(&source);
    if let Err(error) = shell.session.record(entry) {
        diagnostic(&format!("fc: history: {error}"));
    }
    shell.call_depth += 1;
    let status = shell.code(&source, tested);
    shell.call_depth -= 1;
    status
}

#[derive(Default)]
struct Invocation<'a> {
    list: bool,
    no_numbers: bool,
    reverse: bool,
    repeat: bool,
    editor: Option<&'a [u8]>,
    replacement: Option<(&'a [u8], &'a [u8])>,
    first: Option<&'a [u8]>,
    last: Option<&'a [u8]>,
}

impl<'a> Invocation<'a> {
    fn parse(args: &'a [Vec<u8>]) -> Result<Self, &'static str> {
        let mut options = Self::default();
        let mut args = args.iter().map(Vec::as_slice).peekable();
        while let Some(arg) = args.peek().copied() {
            if arg == b"--" {
                args.next();
                break;
            }
            let Some(flags) = arg.strip_prefix(b"-") else {
                break;
            };
            if flags.is_empty() || flags.iter().all(u8::is_ascii_digit) {
                break;
            }
            args.next();
            for (index, flag) in flags.iter().enumerate() {
                match flag {
                    b'l' => options.list = true,
                    b'n' => options.no_numbers = true,
                    b'r' => options.reverse = true,
                    b's' => options.repeat = true,
                    b'e' => {
                        let inline = &flags[index + 1..];
                        options.editor = Some(if inline.is_empty() {
                            args.next().ok_or("-e requires an editor name")?
                        } else {
                            inline
                        });
                        break;
                    }
                    _ => return Err("unknown option"),
                }
            }
        }
        if (options.editor.is_some() && (options.list || options.repeat))
            || (options.repeat && (options.list || options.reverse))
            || (options.no_numbers && !options.list)
        {
            return Err("incompatible options");
        }
        if options.repeat {
            if let Some(arg) = args.peek().copied() {
                if let Some(index) = arg.iter().position(|byte| *byte == b'=') {
                    options.replacement = Some((&arg[..index], &arg[index + 1..]));
                    args.next();
                }
            }
        }
        options.first = args.next();
        options.last = args.next();
        if (options.repeat && options.last.is_some()) || args.next().is_some() {
            return Err("too many operands");
        }
        Ok(options)
    }

    fn select(&self, history: &[Vec<u8>]) -> Result<Vec<usize>, &'static str> {
        let newest = history.len() - 1;
        let is_range = self.list || self.last.is_some();
        let first = match self.first {
            Some(first) => resolve(history, first, is_range.then_some(0))?,
            None if self.list => history.len().saturating_sub(16),
            None => newest,
        };
        let last = match self.last {
            Some(last) => resolve(history, last, Some(newest))?,
            None if self.list => newest,
            None => first,
        };
        let mut indices = (first.min(last)..=first.max(last)).collect::<Vec<_>>();
        if (first > last) ^ self.reverse {
            indices.reverse();
        }
        Ok(indices)
    }
}

fn resolve(
    history: &[Vec<u8>],
    selector: &[u8],
    fallback: Option<usize>,
) -> Result<usize, &'static str> {
    let number = selector
        .strip_prefix(b"+")
        .or_else(|| selector.strip_prefix(b"-"))
        .unwrap_or(selector);
    if !number.is_empty() && number.iter().all(u8::is_ascii_digit) {
        let count = number.iter().fold(0usize, |acc, digit| {
            acc.saturating_mul(10)
                .saturating_add((digit - b'0') as usize)
        });
        let negative = selector.starts_with(b"-");
        let position = if negative {
            history.len().checked_sub(count)
        } else {
            count.checked_sub(1)
        };
        return match position {
            Some(index) if index < history.len() => Ok(index),
            _ if fallback.is_none() => Err("command number out of range"),
            _ if negative || count == 0 => Ok(0),
            _ => Ok(history.len() - 1),
        };
    }
    history
        .iter()
        .rposition(|line| line.starts_with(selector))
        .or(fallback)
        .ok_or("no previous command starts with the operand")
}

fn find(text: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        Some(0)
    } else {
        text.windows(needle.len())
            .position(|window| window == needle)
    }
}

struct EditFile {
    directory: PathBuf,
    path: PathBuf,
}

impl EditFile {
    fn new(source: &[u8]) -> io::Result<Self> {
        let mut template = *b"/tmp/nsh-fc-XXXXXXXX\0";
        // SAFETY: the mutable buffer contains a NUL-terminated template. On
        // success mkdtemp writes a private directory name into the same buffer.
        let created = unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) };
        if created.is_null() {
            return Err(io::Error::last_os_error());
        }
        let directory = PathBuf::from(std::ffi::OsStr::from_bytes(
            CStr::from_bytes_until_nul(&template)
                .expect("mkdtemp leaves the terminator")
                .to_bytes(),
        ));
        let edit = Self {
            path: directory.join("commands"),
            directory,
        };
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&edit.path)?;
        file.write_all(source)?;
        Ok(edit)
    }

    fn read(&self) -> io::Result<Vec<u8>> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&self.path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != crate::sys::uid()
            || metadata.mode() & 0o022 != 0
        {
            return Err(io::Error::other(
                "edited path does not name a trusted regular file",
            ));
        }
        let mut source = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut source)?;
        if source.len() as u64 > LIMIT {
            return Err(io::Error::other("edited file exceeds the size limit"));
        }
        Ok(source)
    }
}

impl Drop for EditFile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_ranges_and_reverses_relative_order() {
        let history = [
            b"echo one".to_vec(),
            b"printf two".to_vec(),
            b"echo three".to_vec(),
        ];
        let flags = [b"-lnr".to_vec(), b"-1".to_vec(), b"1".to_vec()];
        let action = Invocation::parse(&flags).unwrap();
        assert!(action.no_numbers);
        assert_eq!(action.select(&history).unwrap(), [0, 1, 2]);
        let flags = [b"-l".to_vec(), b"+2".to_vec(), b"99".to_vec()];
        assert_eq!(
            Invocation::parse(&flags).unwrap().select(&history).unwrap(),
            [1, 2]
        );
    }

    #[test]
    fn parses_repeat_operands_as_bytes_and_uses_latest_prefix_match() {
        let history = [b"echo one".to_vec(), b"echo \xff".to_vec()];
        let flags = [b"-s".to_vec(), b"\xff=two".to_vec(), b"echo".to_vec()];
        let action = Invocation::parse(&flags).unwrap();
        assert_eq!(action.replacement, Some((&b"\xff"[..], &b"two"[..])));
        assert_eq!(action.select(&history).unwrap(), [1]);
        assert_eq!(find(&history[1], b"\xff"), Some(5));
    }
}
