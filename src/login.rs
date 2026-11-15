//! Arch/CachyOS login-shell changes. Only util-linux chsh can write the account.
use std::ffi::{CStr, CString, OsString};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

const TARGET: &str = "/usr/bin/nsh";
const CHSH: &str = "/usr/bin/chsh";
const PACMAN: &str = "/usr/bin/pacman";
const SHELLS: &str = "/etc/shells";
const PASSWD: &str = "/etc/passwd";

#[derive(Debug, PartialEq, Eq)]
struct Account {
    name: OsString,
    uid: libc::uid_t,
    gid: libc::gid_t,
    shell: PathBuf,
}

fn uid() -> Result<libc::uid_t, String> {
    // SAFETY: these process identity calls take no pointers and do not modify state.
    unsafe {
        let uid = libc::getuid();
        if uid == 0 {
            return Err("this mode requires a non-root local account".into());
        }
        if uid != libc::geteuid() || libc::getgid() != libc::getegid() {
            return Err("real and effective user and group IDs must match".into());
        }
        Ok(uid)
    }
}

fn numeric_id(field: &[u8]) -> Option<u32> {
    if field.is_empty() || !field.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(field).ok()?.parse().ok()
}

fn local_account(content: &[u8], uid: libc::uid_t) -> Result<Account, String> {
    let records = || {
        content
            .split(|byte| *byte == b'\n')
            .map(|line| line.split(|byte| *byte == b':').collect::<Vec<_>>())
    };
    let mut candidates =
        records().filter(|fields| fields.get(2).and_then(|field| numeric_id(field)) == Some(uid));
    let fields = candidates
        .next()
        .ok_or("the real UID has no local /etc/passwd entry")?;
    if candidates.next().is_some() || fields.len() != 7 || fields[0].is_empty() {
        return Err("the local UID has an ambiguous or malformed account entry".into());
    }
    if records().filter(|record| record[0] == fields[0]).count() != 1 {
        return Err("the local account name is ambiguous".into());
    }
    Ok(Account {
        name: OsString::from_vec(fields[0].to_vec()),
        uid,
        gid: numeric_id(fields[3]).ok_or("the local account has an invalid GID")?,
        shell: PathBuf::from(OsString::from_vec(fields[6].to_vec())),
    })
}

fn lookup_account(
    mut lookup: impl FnMut(
        *mut libc::passwd,
        *mut libc::c_char,
        usize,
        *mut *mut libc::passwd,
    ) -> libc::c_int,
) -> Result<Account, String> {
    let mut capacity = 1024;
    loop {
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut buffer = vec![0_u8; capacity];
        let mut result = std::ptr::null_mut();
        let code = lookup(
            entry.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        );
        if code == libc::ERANGE && capacity < 1024 * 1024 {
            capacity *= 2;
            continue;
        }
        if code != 0 {
            return Err(format!(
                "account lookup failed: {}",
                io::Error::from_raw_os_error(code)
            ));
        }
        if result.is_null() {
            return Err("no matching account in the system account database".into());
        }
        // SAFETY: success with a non-null result initialized entry and the two
        // NUL-terminated fields in buffer. Copy them before freeing buffer.
        let entry = unsafe { entry.assume_init() };
        if entry.pw_name.is_null() || entry.pw_shell.is_null() {
            return Err("system account entry is missing its name or shell".into());
        }
        let owned = |pointer| {
            // SAFETY: the caller supplies only the verified non-null fields above.
            OsString::from_vec(unsafe { CStr::from_ptr(pointer) }.to_bytes().to_vec())
        };
        return Ok(Account {
            name: owned(entry.pw_name),
            uid: entry.pw_uid,
            gid: entry.pw_gid,
            shell: PathBuf::from(owned(entry.pw_shell)),
        });
    }
}

fn read_account(uid: libc::uid_t) -> Result<Account, String> {
    let contents = system_file(PASSWD)?;
    let local = local_account(&contents, uid)?;
    let name = CString::new(local.name.as_bytes()).map_err(|_| "invalid account name")?;
    // SAFETY: lookup_account provides a passwd slot, a writable buffer of the
    // given length and a result pointer, valid throughout each lookup.
    let by_uid = lookup_account(|entry, buffer, length, result| unsafe {
        libc::getpwuid_r(uid, entry, buffer, length, result)
    })?;
    let by_name = lookup_account(|entry, buffer, length, result| unsafe {
        libc::getpwnam_r(name.as_ptr(), entry, buffer, length, result)
    })?;
    if local != by_uid || local != by_name {
        return Err("the local and system account entries disagree".into());
    }
    Ok(local)
}

fn metadata(path: &Path) -> Result<Metadata, String> {
    let info = fs::symlink_metadata(path).map_err(|error| format!("{path:?}: {error}"))?;
    if info.uid() != 0 || (!info.is_symlink() && info.mode() & 0o022 != 0) {
        return Err(format!("{path:?} is not restricted to root for writes"));
    }
    Ok(info)
}

/// Walk each component and each symlink target through root-controlled
/// directories. This accepts Arch's /bin -> usr/bin for rollback.
fn resolve_system_path(
    path: &Path,
    allow_links: bool,
    remaining_links: &mut usize,
) -> Result<(PathBuf, Metadata), String> {
    if !path.is_absolute() {
        return Err(format!("{path:?} must be an absolute system path"));
    }
    let mut current = PathBuf::from("/");
    let mut info = metadata(&current)?;
    let mut components = path.components().peekable();
    while let Some(component) = components.next() {
        match component {
            Component::RootDir | Component::CurDir => continue,
            Component::ParentDir => {
                current.pop();
            }
            Component::Normal(segment) => current.push(segment),
            Component::Prefix(_) => return Err("unexpected system path prefix".into()),
        }
        info = metadata(&current)?;
        if info.is_symlink() {
            if !allow_links || *remaining_links == 0 {
                return Err(format!("unexpected symlink at {current:?}"));
            }
            *remaining_links -= 1;
            let link = fs::read_link(&current).map_err(|error| format!("{current:?}: {error}"))?;
            current.pop();
            (current, info) =
                resolve_system_path(&current.join(link), allow_links, remaining_links)?;
        }
        if components.peek().is_some() && !info.is_dir() {
            return Err(format!("{current:?} is not a directory"));
        }
    }
    Ok((current, info))
}

fn system_file(path: &str) -> Result<Vec<u8>, String> {
    let path = Path::new(path);
    let (_, info) = resolve_system_path(path, false, &mut 40)?;
    if !info.is_file() {
        return Err(format!("{path:?} is not a regular file"));
    }
    fs::read(path).map_err(|error| format!("{path:?}: {error}"))
}

fn executable(path: &Path, allow_links: bool) -> Result<Metadata, String> {
    let (_, info) = resolve_system_path(path, allow_links, &mut 40)?;
    let c_path =
        CString::new(path.as_os_str().as_bytes()).map_err(|_| "invalid executable path")?;
    // SAFETY: c_path is NUL-terminated and remains alive for the call.
    if !info.is_file() || unsafe { libc::access(c_path.as_ptr(), libc::X_OK) } != 0 {
        return Err(format!(
            "{path:?} is not an executable regular file for the current account"
        ));
    }
    Ok(info)
}

fn installed_executable() -> Result<(), String> {
    let invoked = std::env::current_exe()
        .map_err(|error| format!("cannot identify the running NSH binary: {error}"))?;
    if invoked != Path::new(TARGET) {
        return Err(format!(
            "run the package-installed {TARGET} (running {invoked:?})"
        ));
    }
    let info = executable(Path::new(TARGET), false)?;
    let running = fs::metadata("/proc/self/exe")
        .map_err(|error| format!("cannot verify the running NSH executable: {error}"))?;
    if (info.dev(), info.ino()) != (running.dev(), running.ino()) || info.mode() & 0o6000 != 0 {
        return Err(format!(
            "the running executable does not match the installed {TARGET}"
        ));
    }
    executable(Path::new(PACMAN), false)?;
    let owner = Command::new(PACMAN)
        .args(["--query", "--quiet", "--owns", "--", TARGET])
        .env_clear()
        .env("LC_ALL", "C")
        .output()
        .map_err(|error| format!("cannot check {TARGET} ownership with pacman: {error}"))?;
    if !owner.status.success() || !matches!(owner.stdout.as_slice(), b"nsh\n" | b"neo-sh\n") {
        return Err(format!(
            "{TARGET} must be owned by the installed NSH package"
        ));
    }
    Ok(())
}

fn registered(content: &[u8], path: &Path) -> bool {
    content.split(|byte| *byte == b'\n').any(|line| {
        line.split(|byte| *byte == b'#')
            .next()
            .unwrap_or_default()
            .trim_ascii()
            == path.as_os_str().as_bytes()
    })
}

fn shell_argument(path: &Path) -> Result<String, String> {
    let name = path
        .to_str()
        .filter(|name| path.is_absolute() && !name.chars().any(char::is_control))
        .ok_or("the current shell pathname cannot be shown as a safe rollback command")?;
    Ok(format!("'{}'", name.replace('\'', r#"'"'"'"#)))
}

#[derive(PartialEq, Eq)]
struct Plan {
    account: Account,
    rollback: Option<String>,
}

impl Plan {
    fn check(account: Account) -> Result<Self, String> {
        let shells = system_file(SHELLS)?;
        let rollback = if account.shell == Path::new(TARGET) {
            None
        } else {
            let old_shell = shell_argument(&account.shell)?;
            executable(&account.shell, true)?;
            if !registered(&shells, &account.shell) {
                return Err(format!(
                    "rollback shell {old_shell} is missing from {SHELLS}"
                ));
            }
            Some(format!("{CHSH} -s {old_shell}"))
        };
        installed_executable()?;
        executable(Path::new(CHSH), false)?;
        if !registered(&shells, Path::new(TARGET)) {
            return Err(format!(
                "{TARGET} is missing from {SHELLS}. Ask the administrator to add the exact pathname on its own line, preserving the existing entries."
            ));
        }
        Ok(Self { account, rollback })
    }

    fn report(&self) -> String {
        let summary = format!(
            "Current login shell: {:?}\nTarget login shell: {TARGET}\nChecks: passed\n",
            self.account.shell,
        );
        if let Some(rollback) = &self.rollback {
            format!("{summary}Rollback from the retained terminal: {rollback}\n")
        } else {
            format!(
                "{summary}NSH is already the login shell. Use the rollback command shown before the original change.\n"
            )
        }
    }
}

fn controlling_terminal() -> io::Result<File> {
    let tty = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
    // SAFETY: tty owns its descriptor throughout this lookup.
    let owner = unsafe { libc::tcgetpgrp(tty.as_raw_fd()) };
    // SAFETY: getpgrp takes no pointers and does not modify state.
    if owner < 0 || owner != unsafe { libc::getpgrp() } {
        return Err(io::Error::other(
            "a foreground controlling terminal is required",
        ));
    }
    Ok(tty)
}

fn confirmation(tty: &mut impl Read) -> io::Result<bool> {
    // Read exactly one line, one byte at a time, so PAM owns all later input.
    let mut length = 0;
    let mut matches = true;
    let expected = b"yes";
    let mut byte = [0];
    loop {
        match tty.read(&mut byte) {
            Ok(0) => return Ok(false),
            Ok(_) if byte[0] == b'\n' => return Ok(matches && length == expected.len()),
            Ok(_) => {
                matches &= expected.get(length) == Some(&byte[0]);
                length = (length + 1).min(expected.len() + 1);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

fn perform(plan: Plan) -> Result<(), String> {
    let mut tty = match controlling_terminal() {
        Ok(tty) => tty,
        Err(error) => {
            print!("{}", plan.report());
            return Err(format!(
                "a foreground controlling terminal is required: {error}"
            ));
        }
    };
    write!(tty, "{}Type yes to change the login shell: ", plan.report())
        .and_then(|()| tty.flush())
        .map_err(|error| format!("cannot display confirmation: {error}"))?;
    if !confirmation(&mut tty).map_err(|error| format!("cannot read confirmation: {error}"))? {
        return Err("change cancelled".into());
    }
    let uid = uid()?;
    if Plan::check(read_account(uid)?)? != plan {
        return Err(
            "the account or rollback plan changed during confirmation. Run the check again.".into(),
        );
    }
    let descriptors = || {
        tty.try_clone()
            .map_err(|error| format!("cannot attach chsh to the terminal: {error}"))
    };
    let status = Command::new(CHSH)
        .args(["-s", TARGET])
        .env("PATH", "/usr/bin")
        .stdin(Stdio::from(descriptors()?))
        .stdout(Stdio::from(descriptors()?))
        .stderr(Stdio::from(descriptors()?))
        .status();
    let after = read_account(uid);
    match &after {
        Ok(account) => eprintln!("Actual login shell: {:?}", account.shell),
        Err(error) => eprintln!("Cannot verify the actual login shell: {error}"),
    }
    let status = status.map_err(|error| format!("cannot start {CHSH}: {error}"))?;
    if !status.success() {
        return Err(format!("{CHSH} failed ({status})"));
    }
    let after = after?;
    if after.uid != plan.account.uid
        || after.gid != plan.account.gid
        || after.name != plan.account.name
        || after.shell != Path::new(TARGET)
    {
        return Err("account readback does not match the requested change".into());
    }
    eprintln!("Keep this terminal open and verify a new login in a separate terminal.");
    Ok(())
}

pub fn run(dry_run: bool) -> i32 {
    let account = match uid().and_then(read_account) {
        Ok(account) => account,
        Err(error) => {
            println!("Target login shell: {TARGET}");
            eprintln!("Checks: blocked: {error}");
            return 1;
        }
    };
    let previous = account.shell.clone();
    let plan = match Plan::check(account) {
        Ok(plan) => plan,
        Err(error) => {
            println!("Current login shell: {previous:?}\nTarget login shell: {TARGET}");
            if let Ok(rollback) = shell_argument(&previous) {
                if previous != Path::new(TARGET)
                    && executable(&previous, true).is_ok()
                    && system_file(SHELLS).is_ok_and(|shells| registered(&shells, &previous))
                    && executable(Path::new(CHSH), false).is_ok()
                {
                    println!("Rollback from the retained terminal: {CHSH} -s {rollback}");
                }
            }
            eprintln!("Checks: blocked: {error}");
            return 1;
        }
    };
    if dry_run || plan.rollback.is_none() {
        print!("{}", plan.report());
        return 0;
    }
    if let Err(error) = perform(plan) {
        eprintln!("nsh: {error}");
        return 1;
    }
    0
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn passwd_requires_exactly_one_local_uid_and_name() {
        let fish = b"me:x:1000:1000::/home/me:/bin/fish\n";
        let account = local_account(fish, 1000).unwrap();
        assert_eq!(account.shell, Path::new("/bin/fish"));
        let mut duplicate = fish.to_vec();
        duplicate.extend_from_slice(b"other:x:1000:1000::/home/other:/bin/bash\n");
        assert!(local_account(&duplicate, 1000).is_err());
        let mut duplicate = fish.to_vec();
        duplicate.extend_from_slice(b"me:x:2000:2000::/home/other:/bin/bash\n");
        assert!(local_account(&duplicate, 1000).is_err());
        assert!(local_account(fish, 2000).is_err());
    }

    #[test]
    fn shell_registry_requires_exact_path_on_a_non_comment_line() {
        let shells = b"# /usr/bin/nsh\n /usr/bin/nsh-test\n  /bin/fish # old\n";
        assert!(!registered(shells, Path::new("/usr/bin/nsh")));
        assert!(registered(shells, Path::new("/bin/fish")));
        assert!(registered(b"/usr/bin/nsh\n", Path::new("/usr/bin/nsh")));
    }

    #[test]
    fn confirmation_reads_only_a_complete_matching_line() {
        let mut stream = Cursor::new(b"yes\npassword\n");
        assert!(confirmation(&mut stream).unwrap());
        assert_eq!(stream.position(), 4);
        for denied in [b"yes".as_slice(), b"y\n", b"YES\n", b"no\n", b""] {
            assert!(!confirmation(&mut Cursor::new(denied)).unwrap());
        }
    }

    #[test]
    fn oversized_confirmation_does_not_leave_a_partial_command_in_the_terminal() {
        let mut line = vec![b'x'; 90];
        line.extend_from_slice(b"\nnext input\n");
        let mut stream = Cursor::new(line);
        assert!(!confirmation(&mut stream).unwrap());
        assert_eq!(stream.position(), 91);
    }

    #[test]
    fn rollback_display_quotes_a_system_path() {
        assert_eq!(
            shell_argument(Path::new("/usr/bin/old's shell")).unwrap(),
            r#"'/usr/bin/old'"'"'s shell'"#
        );
        assert!(shell_argument(Path::new("/usr/bin/old\nshell")).is_err());
        assert!(shell_argument(Path::new("bin/old")).is_err());
    }
}
