//! File access for NSH-owned state. Every directory component is opened relative
//! to its verified parent, and symbolic links are rejected throughout.
use std::env;
use std::ffi::{CStr, CString, OsStr};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) fn safe_ids() -> bool {
    // SAFETY: These Linux identity queries take no arguments.
    unsafe { libc::getuid() == libc::geteuid() && libc::getgid() == libc::getegid() }
}

pub(crate) fn uid() -> u32 {
    // SAFETY: getuid takes no arguments and does not modify memory.
    unsafe { libc::getuid() }
}

pub(crate) fn base(variable: &str, fallback: &str) -> Option<PathBuf> {
    env::var_os(variable)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(fallback))
        })
        .map(|path| path.join("nsh"))
}

fn c_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains NUL"))
}

fn open(parent: &File, name: &CStr, flags: i32, mode: u32) -> io::Result<File> {
    // SAFETY: The parent fd and the NUL-terminated name outlive openat. A
    // successful call transfers a new descriptor to File exactly once.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode,
        )
    };
    if fd == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub(crate) struct Dir(File);

impl Dir {
    pub(crate) fn identity(&self) -> io::Result<(u64, u64)> {
        let metadata = self.0.metadata()?;
        Ok((metadata.dev(), metadata.ino()))
    }

    pub(crate) fn open(path: &Path, create: bool, private: bool) -> io::Result<Self> {
        let mut parts = path.components().peekable();
        if parts.next() != Some(Component::RootDir) || !safe_ids() {
            return Err(io::Error::other(
                "unsafe XDG path or mismatched real/effective IDs",
            ));
        }
        let mut current = Self(File::open("/")?);
        while let Some(component) = parts.next() {
            let Component::Normal(name) = component else {
                return Err(io::Error::other("parent traversal is invalid in XDG paths"));
            };
            current = current.child(name, create, private && parts.peek().is_none())?;
        }
        Ok(current)
    }

    pub(crate) fn child(&self, name: &OsStr, create: bool, private: bool) -> io::Result<Self> {
        let name = c_name(name)?;
        let try_open = || open(&self.0, &name, libc::O_RDONLY | libc::O_DIRECTORY, 0);
        let file = match try_open() {
            Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                // SAFETY: Parent fd is open and name is a live C string.
                if unsafe { libc::mkdirat(self.0.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::AlreadyExists {
                        return Err(error);
                    }
                }
                try_open()?
            }
            result => result?,
        };
        let meta = file.metadata()?;
        let mode = meta.mode();
        // A root-owned sticky directory, for example /tmp, cannot have an
        // existing user-owned name replaced by a different ordinary user.
        let sticky_system = meta.uid() == 0 && mode & 0o1000 != 0;
        let trusted =
            (meta.uid() == uid() || meta.uid() == 0) && (mode & 0o022 == 0 || sticky_system);
        if !trusted || (private && (meta.uid() != uid() || mode & 0o077 != 0)) {
            return Err(io::Error::other(format!(
                "untrusted directory {}",
                name.to_string_lossy()
            )));
        }
        Ok(Self(file))
    }

    pub(crate) fn read(&self, name: &OsStr, limit: u64, private: bool) -> io::Result<Vec<u8>> {
        let file = open(
            &self.0,
            &c_name(name)?,
            libc::O_RDONLY | libc::O_NONBLOCK,
            0,
        )?;
        let meta = file.metadata()?;
        let forbidden = if private { 0o177 } else { 0o022 };
        if !meta.is_file()
            || meta.uid() != uid()
            || meta.mode() & forbidden != 0
            || (private && meta.nlink() != 1)
        {
            return Err(io::Error::other(format!(
                "untrusted file {}",
                name.to_string_lossy()
            )));
        }
        let mut bytes = Vec::new();
        file.take(limit + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err(io::Error::other(format!(
                "{} exceeds the size limit",
                name.to_string_lossy()
            )));
        }
        Ok(bytes)
    }

    pub(crate) fn list(&self) -> io::Result<Vec<String>> {
        // This path resolves to the directory already pinned by the live fd.
        fs::read_dir(format!("/proc/self/fd/{}", self.0.as_raw_fd()))?
            .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .collect()
    }

    pub(crate) fn lock(&self) -> io::Result<DirLock<'_>> {
        // flock on the directory pins a common lock inode without creating a
        // persistent lockfile that could be unlinked during a transaction.
        // SAFETY: self owns the directory descriptor for the entire lock.
        while unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_EX) } != 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        Ok(DirLock(&self.0))
    }

    pub(crate) fn create(&self, name: &OsStr, data: &[u8]) -> io::Result<()> {
        let mut file = open(
            &self.0,
            &c_name(name)?,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )?;
        if let Err(error) = file.write_all(data).and_then(|()| file.sync_all()) {
            let _ = self.remove(name);
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn replace(&self, name: &OsStr, data: &[u8]) -> io::Result<()> {
        static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
        let target = c_name(name)?;
        let temporary = loop {
            let candidate = format!(
                ".nsh-{}-{}.tmp",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            );
            match self.create(OsStr::new(&candidate), data) {
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                result => result?,
            }
            break candidate;
        };
        let temporary_c = c_name(OsStr::new(&temporary))?;
        let fd = self.0.as_raw_fd();
        // SAFETY: The descriptor and names remain live. The rename is atomic
        // within the same directory and does not dereference the target name.
        if unsafe { libc::renameat(fd, temporary_c.as_ptr(), fd, target.as_ptr()) } != 0 {
            let error = io::Error::last_os_error();
            let _ = self.remove(OsStr::new(&temporary));
            return Err(error);
        }
        self.0.sync_all()
    }

    pub(crate) fn remove(&self, name: &OsStr) -> io::Result<()> {
        let name = c_name(name)?;
        // SAFETY: The descriptor and C string outlive unlinkat.
        if unsafe { libc::unlinkat(self.0.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

pub(crate) struct DirLock<'a>(&'a File);

impl Drop for DirLock<'_> {
    fn drop(&mut self) {
        // SAFETY: The File remains live while the lock borrows it.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}
