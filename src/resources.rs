//! Intrinsic utilities that operate on the shell or its jobs.
use crate::{
    shell::{Flow, Shell, decimal, diagnostic, output, show},
    sys,
};

const SIGNALS: &[(&[u8], i32)] = &[
    (b"EXIT", 0),
    (b"HUP", libc::SIGHUP),
    (b"INT", libc::SIGINT),
    (b"QUIT", libc::SIGQUIT),
    (b"ILL", libc::SIGILL),
    (b"TRAP", libc::SIGTRAP),
    (b"ABRT", libc::SIGABRT),
    (b"BUS", libc::SIGBUS),
    (b"FPE", libc::SIGFPE),
    (b"KILL", libc::SIGKILL),
    (b"USR1", libc::SIGUSR1),
    (b"SEGV", libc::SIGSEGV),
    (b"USR2", libc::SIGUSR2),
    (b"PIPE", libc::SIGPIPE),
    (b"ALRM", libc::SIGALRM),
    (b"TERM", libc::SIGTERM),
    (b"CHLD", libc::SIGCHLD),
    (b"CONT", libc::SIGCONT),
    (b"STOP", libc::SIGSTOP),
    (b"TSTP", libc::SIGTSTP),
    (b"TTIN", libc::SIGTTIN),
    (b"TTOU", libc::SIGTTOU),
    (b"URG", libc::SIGURG),
    (b"XCPU", libc::SIGXCPU),
    (b"XFSZ", libc::SIGXFSZ),
    (b"VTALRM", libc::SIGVTALRM),
    (b"PROF", libc::SIGPROF),
    (b"WINCH", libc::SIGWINCH),
    (b"IO", libc::SIGIO),
    (b"SYS", libc::SIGSYS),
];

pub fn signal_number(name: &[u8]) -> Option<i32> {
    if let Some(n) = decimal(name) {
        return (0..=64).contains(&n).then_some(n);
    }
    let upper = name.to_ascii_uppercase();
    let name = upper.strip_prefix(b"SIG").unwrap_or(&upper);
    SIGNALS
        .iter()
        .find_map(|(label, number)| (*label == name).then_some(*number))
}

pub fn kill(shell: &Shell, mut args: &[Vec<u8>]) -> Flow {
    if args.first().is_some_and(|a| a == b"-l") {
        return match &args[1..] {
            [] => {
                let mut out = SIGNALS
                    .iter()
                    .skip(1)
                    .map(|(name, _)| *name)
                    .collect::<Vec<_>>()
                    .join(&b' ');
                out.push(b'\n');
                output(&out)
            }
            [arg] => {
                let name = decimal(arg).and_then(|n| {
                    let signal = if n > 128 { n - 128 } else { n };
                    SIGNALS
                        .iter()
                        .skip(1)
                        .find_map(|(name, number)| (*number == signal).then_some(*name))
                });
                match name {
                    Some(name) => output(&[name, b"\n"].concat()),
                    None => shell.error(2, "kill: invalid signal status", false),
                }
            }
            _ => shell.error(2, "kill -l: expected at most one exit status", false),
        };
    }
    let mut sig = libc::SIGTERM;
    if let Some(value) = args
        .first()
        .and_then(|a| a.strip_prefix(b"-"))
        .filter(|v| !v.is_empty() && *v != b"-")
    {
        let choice = if value == b"s" {
            let Some(arg) = args.get(1) else {
                return shell.error(2, "kill -s requires a signal", false);
            };
            args = &args[1..];
            arg.as_slice()
        } else {
            value
        };
        let Some(number) = signal_number(choice) else {
            return shell.error(2, "kill: unknown signal", false);
        };
        sig = number;
        args = &args[1..];
    }
    if args.first().is_some_and(|a| a == b"--") {
        args = &args[1..];
    }
    if args.is_empty() {
        return shell.error(2, "kill: expected a process or job ID", false);
    }
    let mut status = 0;
    for target in args {
        let pid = if target.starts_with(b"%") {
            shell.jobs.signal_target(target)
        } else {
            decimal(target)
        };
        let result = pid
            .ok_or_else(|| "invalid or unknown process or job".to_owned())
            .and_then(|id| sys::send(id, sig).map_err(|error| error.to_string()));
        if let Err(error) = result {
            diagnostic(&format!("kill: {}: {error}", show(target)));
            status = 1;
        }
    }
    Flow::Status(status)
}

pub fn umask(shell: &Shell, mut args: &[Vec<u8>]) -> Flow {
    let symbolic = args.first().is_some_and(|a| a == b"-S");
    if symbolic {
        args = &args[1..];
    }
    if args.first().is_some_and(|a| a == b"--") {
        args = &args[1..];
    }
    // NSH is single-threaded and executes no work between the two calls.
    let previous = unsafe {
        let value = libc::umask(0);
        libc::umask(value);
        value
    };
    let mask = match args {
        [] if symbolic => {
            let permissions = |shift| {
                String::from_iter([(4_u32, 'r'), (2, 'w'), (1, 'x')].into_iter().filter_map(
                    |(bit, label)| (previous & (bit << shift) == 0_u32).then_some(label),
                ))
            };
            return output(
                format!(
                    "u={},g={},o={}\n",
                    permissions(6),
                    permissions(3),
                    permissions(0)
                )
                .as_bytes(),
            );
        }
        [] => return output(format!("{previous:04o}\n").as_bytes()),
        [arg] if !symbolic => {
            if !arg.is_empty() && arg.iter().all(u8::is_ascii_digit) {
                u32::from_str_radix(std::str::from_utf8(arg).unwrap(), 8)
                    .ok()
                    .filter(|mask| *mask <= 0o777)
            } else {
                symbolic_mask(previous, arg)
            }
        }
        _ => return shell.error(2, "usage: umask [-S] [mask]", false),
    };
    let Some(mask) = mask else {
        return shell.error(1, "umask: invalid mask", false);
    };
    // Supplying mode permission bits changes only the current process mask.
    unsafe {
        libc::umask(mask);
    }
    Flow::Status(0)
}

fn symbolic_mask(previous: u32, operand: &[u8]) -> Option<u32> {
    let mut mode = !previous & 0o777;
    for clause in operand.split(|&b| b == b',') {
        let mut who = 0;
        let mut tail = clause;
        while let Some((&ch, next)) = tail.split_first() {
            who |= match ch {
                b'u' => 0o700,
                b'g' => 0o070,
                b'o' => 0o007,
                b'a' => 0o777,
                _ => break,
            };
            tail = next;
        }
        if tail.is_empty() {
            return None;
        }
        if who == 0 {
            who = 0o777;
        }
        while let Some((&action, operations)) = tail.split_first() {
            let n = operations
                .iter()
                .position(|b| b"+-=".contains(b))
                .unwrap_or(operations.len());
            let (perms, next) = operations.split_at(n);
            tail = next;
            let bits = match perms {
                [b'u'] => (mode >> 6) & 7,
                [b'g'] => (mode >> 3) & 7,
                [b'o'] => mode & 7,
                _ => {
                    let mut bits = 0;
                    for permission in perms {
                        bits |= match permission {
                            b'r' => 4,
                            b'w' => 2,
                            b'x' => 1,
                            b'X' => u32::from(mode & 0o111 != 0),
                            _ => return None,
                        };
                    }
                    bits
                }
            };
            let changed = (bits * 0o111) & who;
            mode = match action {
                b'=' => (mode & !who) | changed,
                b'+' if !perms.is_empty() => mode | changed,
                b'-' if !perms.is_empty() => mode & !changed,
                _ => return None,
            };
        }
    }
    Some(!mode & 0o777)
}

const LIMITS: &[(u8, u32, u64, &str)] = &[
    (b'c', libc::RLIMIT_CORE, 512, "core file (blocks)"),
    (b'd', libc::RLIMIT_DATA, 1024, "data segment (KiB)"),
    (b'f', libc::RLIMIT_FSIZE, 512, "file size (blocks)"),
    (b'n', libc::RLIMIT_NOFILE, 1, "open descriptors"),
    (b's', libc::RLIMIT_STACK, 1024, "stack size (KiB)"),
    (b't', libc::RLIMIT_CPU, 1, "CPU time (seconds)"),
    (b'v', libc::RLIMIT_AS, 1024, "address space (KiB)"),
];

pub fn ulimit(shell: &Shell, args: &[Vec<u8>]) -> Flow {
    let mut hard = false;
    let mut soft = false;
    let mut resource = None;
    let mut i = 0;
    for arg in args {
        if arg == b"--" {
            i += 1;
            break;
        }
        if !arg.starts_with(b"-") || arg.len() == 1 {
            break;
        }
        i += 1;
        for option in &arg[1..] {
            match option {
                b'H' => hard = true,
                b'S' => soft = true,
                flag if (*flag == b'a' || LIMITS.iter().any(|(name, _, _, _)| name == flag))
                    && resource.is_none() =>
                {
                    resource = Some(*flag)
                }
                _ => return shell.error(2, "ulimit: invalid options", false),
            }
        }
    }
    let resource = resource.unwrap_or(b'f');
    if args.len() - i > 1 || resource == b'a' && i < args.len() {
        return shell.error(2, "ulimit: unexpected operand", false);
    }
    let operand = args.get(i);
    let mut out = String::new();
    for &(_, id, units, label) in LIMITS
        .iter()
        .filter(|(flag, _, _, _)| *flag == resource || resource == b'a')
    {
        // getrlimit initializes the writable buffer on success.
        let limits = unsafe {
            let mut limits = std::mem::MaybeUninit::<libc::rlimit>::uninit();
            if libc::getrlimit(id, limits.as_mut_ptr()) != 0 {
                return shell.error(1, &std::io::Error::last_os_error().to_string(), false);
            }
            limits.assume_init()
        };
        if let Some(operand) = operand {
            let value = if operand == b"unlimited" {
                Some(libc::RLIM_INFINITY)
            } else {
                std::str::from_utf8(operand)
                    .ok()
                    .and_then(|s| s.parse::<u64>().ok())
                    .and_then(|value| value.checked_mul(units))
                    .filter(|&v| v != libc::RLIM_INFINITY)
            };
            let Some(value) = value else {
                return shell.error(2, "ulimit: invalid limit", false);
            };
            let limits = libc::rlimit {
                rlim_cur: if soft || !hard {
                    value
                } else {
                    limits.rlim_cur
                },
                rlim_max: if hard || !soft {
                    value
                } else {
                    limits.rlim_max
                },
            };
            // setrlimit reads the value during the call.
            if unsafe { libc::setrlimit(id, &limits) } != 0 {
                return shell.error(1, &std::io::Error::last_os_error().to_string(), false);
            }
        } else {
            let value = if hard && !soft {
                limits.rlim_max
            } else {
                limits.rlim_cur
            };
            if resource == b'a' {
                out.push_str(&format!("{label:<24} "));
            }
            if value == libc::RLIM_INFINITY {
                out.push_str("unlimited\n");
            } else {
                out.push_str(&format!("{}\n", value / units));
            }
        }
    }
    output(out.as_bytes())
}
