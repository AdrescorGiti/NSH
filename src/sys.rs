use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicU64, Ordering};

static PENDING: AtomicU64 = AtomicU64::new(0);
extern "C" fn signal_handler(sig: libc::c_int) {
    if let Some(bit) = 1u64.checked_shl(sig as u32) {
        PENDING.fetch_or(bit, Ordering::Relaxed);
    }
}

pub fn signal_waiting() -> bool {
    PENDING.load(Ordering::Relaxed) != 0
}

pub fn signal_pending(sig: i32) -> bool {
    1u64.checked_shl(sig as u32)
        .is_some_and(|bit| PENDING.load(Ordering::Relaxed) & bit != 0)
}

pub fn signals_pending() -> u64 {
    PENDING.swap(0, Ordering::Relaxed)
}

pub fn signal(sig: i32, disposition: usize) -> io::Result<()> {
    // sigaction copies the initialized action. The handler only sets an atomic bit.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = disposition;
        libc::sigemptyset(&mut action.sa_mask);
        cvt(libc::sigaction(sig, &action, std::ptr::null_mut()))?;
    }
    Ok(())
}

pub fn catch(sig: i32) -> io::Result<()> {
    signal(sig, signal_handler as *const () as usize)
}

pub fn cvt(n: i32) -> io::Result<i32> {
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n)
    }
}

pub fn tty(fd: RawFd) -> bool {
    // isatty only probes the descriptor.
    unsafe { libc::isatty(fd) == 1 }
}

pub fn uid() -> u32 {
    // POSIX process identifiers have no pointer arguments.
    unsafe { libc::getuid() }
}

pub fn privilege_matches() -> bool {
    // POSIX process identifiers have no pointer arguments.
    unsafe { libc::getuid() == libc::geteuid() && libc::getgid() == libc::getegid() }
}

pub fn write(fd: RawFd, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        // bytes remains readable throughout the call.
        let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if n < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        } else if n == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        } else {
            bytes = &bytes[n as usize..];
        }
    }
    Ok(())
}

pub fn byte(fd: RawFd) -> io::Result<Option<u8>> {
    let mut byte = [0];
    // Reading one byte avoids taking a following line from a command sharing stdin.
    Ok((read(fd, &mut byte)? != 0).then_some(byte[0]))
}

pub fn read(fd: RawFd, bytes: &mut [u8]) -> io::Result<usize> {
    // The writable slice stays valid for the call, which reads at most its length.
    let n = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

pub fn dup_min(fd: RawFd, min: i32) -> io::Result<OwnedFd> {
    // fcntl returns a new, owned descriptor. On failure handle remains untouched.
    unsafe {
        let n = cvt(libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, min))?;
        Ok(OwnedFd::from_raw_fd(n))
    }
}

pub fn dup_to(from: RawFd, to: RawFd) -> io::Result<()> {
    // Both descriptor numbers are ordinary process-local integers.
    unsafe {
        if from == to {
            cvt(libc::fcntl(from, libc::F_SETFD, 0))?;
        } else {
            cvt(libc::dup2(from, to))?;
        }
    }
    Ok(())
}

pub fn install_fd(fd: OwnedFd, to: RawFd) -> io::Result<()> {
    dup_to(fd.as_raw_fd(), to)?;
    if fd.as_raw_fd() == to {
        let _ = fd.into_raw_fd();
    }
    Ok(())
}

pub fn close(fd: RawFd) {
    // Descriptor is deliberately unowned here.
    unsafe { libc::close(fd) };
}

pub fn open(path: &[u8], flags: i32) -> io::Result<OwnedFd> {
    let path = CString::new(path)?;
    // open copies the pathname and yields a new descriptor on success.
    unsafe {
        let n = cvt(libc::open(path.as_ptr(), flags | libc::O_CLOEXEC, 0o666))?;
        Ok(OwnedFd::from_raw_fd(n))
    }
}

pub fn data_file(data: &[u8], min: RawFd) -> io::Result<OwnedFd> {
    // The Linux syscall avoids a dependency on the GLIBC_2.27 wrapper.
    let name = c"nsh-heredoc";
    let fd = unsafe { libc::syscall(libc::SYS_memfd_create, name.as_ptr(), libc::MFD_CLOEXEC) };
    let mut handle = if fd >= 0 {
        // The syscall returns an owned file descriptor on success.
        unsafe { File::from_raw_fd(fd as RawFd) }
    } else {
        let error = io::Error::last_os_error();
        if !matches!(
            error.raw_os_error(),
            Some(libc::ENOSYS | libc::EPERM | libc::EACCES)
        ) {
            return Err(error);
        }
        temporary_data_file()?
    };
    handle.write_all(data)?;
    handle.seek(SeekFrom::Start(0))?;
    dup_min(handle.as_raw_fd(), min)
}

fn temporary_data_file() -> io::Result<File> {
    let path = std::env::temp_dir().join("nsh.XXXXXX");
    let mut name = CString::new(path.as_os_str().as_bytes())?.into_bytes_with_nul();
    // mkostemp creates an exclusive 0600 file and fills in the template. Unlink it
    // before writing data so only the open descriptor can access the contents.
    unsafe {
        let fd = cvt(libc::mkostemp(name.as_mut_ptr().cast(), libc::O_CLOEXEC))?;
        let handle = File::from_raw_fd(fd);
        cvt(libc::unlink(name.as_ptr().cast()))?;
        Ok(handle)
    }
}

/// The startup parser and its forked workers share this file, so a rejected
/// setting remains visible to the transaction even when shell code masks its status.
pub struct StartupFailure(File);

impl StartupFailure {
    pub fn new() -> io::Result<Self> {
        data_file(&[0], 128).map(|descriptor| Self(File::from(descriptor)))
    }

    pub fn record(&self) {
        let _ = self.0.write_all_at(&[1], 0);
    }

    pub fn failed(&self) -> bool {
        let mut value = [1];
        self.0.read_exact_at(&mut value, 0).is_err() || value[0] != 0
    }
}

pub fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [-1; 2];
    // pipe2 initializes both entries together on success.
    let (read, write) = unsafe {
        cvt(libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC))?;
        (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1]))
    };
    // Closing the original pipe endpoints in a child must not close new stdio.
    let above_stdio = |fd: OwnedFd| {
        if fd.as_raw_fd() < 3 {
            dup_min(fd.as_raw_fd(), 3)
        } else {
            Ok(fd)
        }
    };
    Ok((above_stdio(read)?, above_stdio(write)?))
}

pub struct ForkSignalMask(libc::sigset_t);

impl Drop for ForkSignalMask {
    fn drop(&mut self) {
        // SAFETY: fork initialized this copy of the preceding mask.
        unsafe {
            libc::sigprocmask(libc::SIG_SETMASK, &self.0, std::ptr::null_mut());
        }
    }
}

pub fn fork() -> io::Result<(i32, ForkSignalMask)> {
    // SAFETY: NSH is single-threaded, and the two sigset pointers refer to disjoint
    // slots valid throughout sigprocmask. The child owns its memory and descriptors.
    unsafe {
        let mut all = std::mem::MaybeUninit::uninit();
        cvt(libc::sigfillset(all.as_mut_ptr()))?;
        let mut previous = std::mem::MaybeUninit::uninit();
        cvt(libc::sigprocmask(
            libc::SIG_BLOCK,
            all.as_ptr(),
            previous.as_mut_ptr(),
        ))?;
        let restore = ForkSignalMask(previous.assume_init());
        // Block through the child's disposition reset so an inherited handler
        // cannot consume a group signal before the child starts its command.
        Ok((cvt(libc::fork())?, restore))
    }
}

pub fn exit(code: i32) -> ! {
    // Child termination skips parent cleanup buffered in copied state.
    unsafe { libc::_exit(code & 255) }
}

pub fn pid() -> i32 {
    // getpid/getpgrp return process-local scalar identifiers.
    unsafe { libc::getpid() }
}

pub fn pgrp() -> i32 {
    unsafe { libc::getpgrp() }
}

pub fn set_pgrp(pid: i32, pgid: i32) -> io::Result<()> {
    // Only scalar identifiers are supplied.
    unsafe { cvt(libc::setpgid(pid, pgid)).map(|_| ()) }
}

pub fn foreground(fd: RawFd) -> io::Result<i32> {
    // tcgetpgrp and tcsetpgrp operate only on an existing terminal descriptor.
    unsafe { cvt(libc::tcgetpgrp(fd)) }
}

pub fn give_terminal(fd: RawFd, pgid: i32) -> io::Result<()> {
    unsafe { cvt(libc::tcsetpgrp(fd, pgid)).map(|_| ()) }
}

pub fn terminal_mode(fd: RawFd) -> io::Result<libc::termios> {
    // tcgetattr initializes the supplied termios on success.
    unsafe {
        let mut mode = std::mem::zeroed();
        cvt(libc::tcgetattr(fd, &mut mode))?;
        Ok(mode)
    }
}

pub fn set_terminal_mode(fd: RawFd, mode: &libc::termios) -> io::Result<()> {
    // The action takes a read-only pointer to the termios during the call.
    unsafe { cvt(libc::tcsetattr(fd, libc::TCSADRAIN, mode)).map(|_| ()) }
}

pub fn send(pid: i32, signal: i32) -> io::Result<()> {
    // kill takes only scalar identifiers.
    unsafe { cvt(libc::kill(pid, signal)).map(|_| ()) }
}

pub fn wait(options: i32) -> io::Result<Option<(i32, i32)>> {
    let mut status = 0;
    // The status pointer remains writable for the call. Positive pid means it was initialized.
    match unsafe { libc::waitpid(-1, &mut status, options) } {
        0 => Ok(None),
        pid if pid > 0 => Ok(Some((pid, status))),
        _ => Err(io::Error::last_os_error()),
    }
}

pub struct ExecArgs<'a> {
    argv: Vec<*const libc::c_char>,
    envp: Vec<*const libc::c_char>,
    storage: std::marker::PhantomData<&'a [CString]>,
}

impl<'a> ExecArgs<'a> {
    pub fn new(args: &'a [CString], env: &'a [CString]) -> Self {
        fn pointers(values: &[CString]) -> Vec<*const libc::c_char> {
            let mut pointers = Vec::with_capacity(values.len() + 1);
            pointers.extend(values.iter().map(|s| s.as_ptr()));
            pointers.push(std::ptr::null());
            pointers
        }
        Self {
            argv: pointers(args),
            envp: pointers(env),
            storage: std::marker::PhantomData,
        }
    }

    pub fn exec(&self, path: &CStr) -> io::Error {
        // The terminated arrays and all borrowed C strings live through execve.
        unsafe { libc::execve(path.as_ptr(), self.argv.as_ptr(), self.envp.as_ptr()) };
        io::Error::last_os_error()
    }
}

pub fn access(path: &[u8], mode: i32) -> bool {
    CString::new(path).is_ok_and(|path| {
        // access does not retain the pathname.
        unsafe { libc::access(path.as_ptr(), mode) == 0 }
    })
}
