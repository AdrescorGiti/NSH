//! Directory snapshots for editor providers. A short-lived helper performs
//! filesystem I/O while NSH's single-threaded editor remains responsive.
use std::cell::RefCell;
use std::fs;
use std::io::{self, BufWriter, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

const HEADER: &[u8] = b"NSHFS1";
const MAX_ENTRIES: usize = 8192;
const MAX_BYTES: usize = 512 * 1024;
const MAX_RETIRED: usize = 8;
const CACHE_TIME: Duration = Duration::from_secs(2);
const IO_TIMEOUT: Duration = Duration::from_secs(3);

thread_local! {
    static RETIRED: RefCell<Vec<(u32, Child)>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone)]
pub struct FsEntry {
    pub name: Vec<u8>,
    pub is_dir: bool,
}

type EndResult = Result<Vec<FsEntry>, (io::ErrorKind, String)>;

struct Cache {
    path: PathBuf,
    time: Instant,
    result: EndResult,
}

#[derive(Default)]
pub struct Fs {
    running: Option<Worker>,
    cache: Option<Cache>,
}

impl Fs {
    pub fn pending(&self) -> bool {
        self.running
            .as_ref()
            .is_some_and(|worker| worker.owner == std::process::id())
    }

    pub fn request(&mut self, path: &Path) -> io::Result<Option<Vec<FsEntry>>> {
        self.request_matching(path, b"")
    }

    pub fn request_matching(
        &mut self,
        path: &Path,
        prefix: &[u8],
    ) -> io::Result<Option<Vec<FsEntry>>> {
        reap_retired();
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        if self
            .running
            .as_ref()
            .is_some_and(|worker| worker.path != path || worker.owner != std::process::id())
        {
            self.running.take();
        }
        self.poll();
        if let Some(cache) = self
            .cache
            .as_ref()
            .filter(|cache| cache.path == path && cache.time.elapsed() < CACHE_TIME)
        {
            return match &cache.result {
                Ok(entries) => Ok(Some(
                    entries
                        .iter()
                        .filter(|entry| entry.name.starts_with(prefix))
                        .cloned()
                        .collect(),
                )),
                Err((kind, message)) => Err(io::Error::new(*kind, message.clone())),
            };
        }
        if self.running.is_none() && RETIRED.with(|retired| retired.borrow().len()) < MAX_RETIRED {
            self.running = Some(Worker::start(path)?);
        }
        Ok(None)
    }

    pub fn poll(&mut self) {
        let Some(worker) = self.running.as_mut() else {
            return;
        };
        let result = match worker.poll() {
            Ok(Some(entries)) => Ok(entries),
            Ok(None) => return,
            Err(error) => Err((error.kind(), error.to_string())),
        };
        self.cache = Some(Cache {
            path: worker.path.clone(),
            time: Instant::now(),
            result,
        });
        self.running.take();
    }
}

struct Worker {
    path: PathBuf,
    child: Option<Child>,
    pipe: ChildStdout,
    bytes: Vec<u8>,
    started: Instant,
    owner: u32,
}

impl Worker {
    fn start(path: PathBuf) -> io::Result<Self> {
        let mut child = Command::new("/proc/self/exe")
            .arg("--plugin-fs")
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let pipe = child.stdout.take().expect("requested piped output");
        let worker = Self {
            path,
            child: Some(child),
            pipe,
            bytes: Vec::new(),
            started: Instant::now(),
            owner: std::process::id(),
        };
        // SAFETY: pipe owns this descriptor. Changing its file status flags
        // affects only this read endpoint.
        let result =
            unsafe { libc::fcntl(worker.pipe.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(worker)
    }

    fn poll(&mut self) -> io::Result<Option<Vec<FsEntry>>> {
        if self.owner != std::process::id() {
            return Err(io::Error::other(
                "directory request belongs to the parent NSH",
            ));
        }
        if self.started.elapsed() > IO_TIMEOUT {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "directory scan timed out",
            ));
        }
        let mut input = [0; 8192];
        loop {
            match self.pipe.read(&mut input) {
                Ok(0) => break,
                Ok(n) if self.bytes.len() + n <= MAX_BYTES => self.bytes.extend(&input[..n]),
                Ok(_) => return Err(io::Error::other("directory scan exceeded its output limit")),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        match self.child.as_mut().expect("running worker").try_wait() {
            Ok(None) => return Ok(None),
            Ok(Some(status)) if !status.success() => {
                return Err(io::Error::other("directory could not be read"));
            }
            Err(error) if error.raw_os_error() != Some(libc::ECHILD) => return Err(error),
            _ => {}
        }
        self.child.take();
        decode(&self.bytes).map(Some)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            if self.owner == std::process::id() && matches!(child.try_wait(), Ok(None)) {
                let _ = child.kill();
                RETIRED.with(|retired| retired.borrow_mut().push((self.owner, child)));
            }
        }
    }
}

pub(crate) fn has_retired() -> bool {
    RETIRED.with(|retired| !retired.borrow().is_empty())
}

pub(crate) fn reap_retired() {
    RETIRED.with(|retired| {
        retired.borrow_mut().retain_mut(|(owner, child)| {
            *owner == std::process::id() && matches!(child.try_wait(), Ok(None))
        });
    });
}

fn decode(bytes: &[u8]) -> io::Result<Vec<FsEntry>> {
    let invalid = || io::Error::other("invalid directory worker response");
    let mut rest = bytes.strip_prefix(HEADER).ok_or_else(invalid)?;
    let mut entries = Vec::new();
    while let [kind @ (0 | 1), a, b, name_and_rest @ ..] = rest {
        let len = usize::from(u16::from_le_bytes([*a, *b]));
        let (name, next) = name_and_rest.split_at_checked(len).ok_or_else(invalid)?;
        entries.push(FsEntry {
            is_dir: *kind != 0,
            name: name.to_vec(),
        });
        rest = next;
    }
    if rest != [0xff] {
        return Err(invalid());
    }
    Ok(entries)
}

/// Write a bounded, byte-preserving snapshot to the private worker pipe.
pub fn worker(path: &Path) -> i32 {
    let Ok(entries) = fs::read_dir(path) else {
        return 1;
    };
    let mut output = BufWriter::new(io::stdout().lock());
    if output.write_all(HEADER).is_err() {
        return 1;
    }
    let mut written = HEADER.len();
    for entry in entries.take(MAX_ENTRIES).flatten() {
        let name = entry.file_name();
        let name = name.as_bytes();
        let Ok(length) = u16::try_from(name.len()) else {
            continue;
        };
        if written + 3 + name.len() + 1 > MAX_BYTES {
            break;
        }
        let is_dir = entry
            .file_type()
            .is_ok_and(|kind| kind.is_dir() || kind.is_symlink() && entry.path().is_dir());
        let [low, high] = length.to_le_bytes();
        if output
            .write_all(&[u8::from(is_dir), low, high])
            .and_then(|()| output.write_all(name))
            .is_err()
        {
            return 1;
        }
        written += name.len() + 3;
    }
    i32::from(
        output
            .write_all(&[0xff])
            .and_then(|()| output.flush())
            .is_err(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_wire_format_preserves_unix_bytes() {
        let entries = decode(b"NSHFS1\x01\x02\x00\xffx\x00\x01\x00y\xff").unwrap();
        assert!(entries[0].is_dir);
        assert_eq!(entries[0].name, b"\xffx");
        assert!(!entries[1].is_dir);
        assert!(decode(b"NSHFS1\x01\x02\x00x").is_err());
    }
}
