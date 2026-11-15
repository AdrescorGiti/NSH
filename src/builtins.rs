use std::ffi::OsStr;
use std::fs::File;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use crate::{
    expand,
    shell::{Flow, Shell, assignment, decimal, empty_variable, is_name, output, show},
    sys,
};

pub fn special(name: &[u8]) -> bool {
    matches!(
        name,
        b"." | b":"
            | b"break"
            | b"continue"
            | b"eval"
            | b"exec"
            | b"exit"
            | b"export"
            | b"readonly"
            | b"return"
            | b"set"
            | b"shift"
            | b"times"
            | b"trap"
            | b"unset"
    )
}

pub(crate) fn builtin(name: &[u8]) -> bool {
    special(name)
        || matches!(
            name,
            b"alias"
                | b"bg"
                | b"cd"
                | b"command"
                | b"echo"
                | b"false"
                | b"fc"
                | b"fg"
                | b"getopts"
                | b"hash"
                | b"jobs"
                | b"kill"
                | b"pwd"
                | b"read"
                | b"true"
                | b"type"
                | b"ulimit"
                | b"umask"
                | b"unalias"
                | b"wait"
        )
}

impl Shell {
    pub fn builtin(
        &mut self,
        name: &[u8],
        args: &[Vec<u8>],
        tested: bool,
        bypass: bool,
    ) -> Option<Flow> {
        let fatal = special(name) && !bypass;
        let result = match name {
            b":" | b"true" => Flow::Status(0),
            b"false" => Flow::Status(1),
            b"printf" | b"test" | b"[" if crate::regular::selected(self, name) => {
                crate::regular::run(self, name, args)?
            }
            b"echo" => {
                if args.first().is_some_and(|v| v == b"-n") {
                    output(&args[1..].join(&b' '))
                } else {
                    output(&[args.join(&b' '), vec![b'\n']].concat())
                }
            }
            b"exit" | b"return" => {
                let status = args
                    .first()
                    .map_or(Some(self.status), |value| decimal(value));
                match status {
                    Some(n) if args.len() <= 1 => {
                        if name == b"exit" {
                            Flow::Exit(n & 255)
                        } else if self.call_depth > 0 {
                            Flow::Return(n & 255)
                        } else {
                            self.error(1, "return: outside a function or sourced script", fatal)
                        }
                    }
                    _ => self.error(
                        2,
                        &format!("{}: expected one numeric status", show(name)),
                        fatal,
                    ),
                }
            }
            b"break" | b"continue" => match args.first().map_or(Some(1), |v| decimal(v)) {
                Some(n) if n > 0 && args.len() <= 1 && self.loops > 0 => {
                    let depth = (n as usize).min(self.loops);
                    if name == b"break" {
                        Flow::Break(depth)
                    } else {
                        Flow::Continue(depth)
                    }
                }
                Some(n) if n > 0 && self.loops == 0 && args.len() <= 1 => Flow::Status(0),
                _ => self.error(
                    2,
                    &format!("{}: expected positive count", show(name)),
                    fatal,
                ),
            },
            b"eval" => {
                if args.is_empty() {
                    Flow::Status(0)
                } else {
                    self.code(&args.join(&b' '), tested)
                }
            }
            b"." => self.source_builtin(args, tested, fatal),
            b"exec" if args.is_empty() => {
                self.exec_redirs = true;
                Flow::Status(0)
            }
            b"exec" => {
                let args = if args[0] == b"--" { &args[1..] } else { args };
                if args.is_empty() {
                    self.exec_redirs = true;
                    Flow::Status(0)
                } else if self.owns_startup_transaction() {
                    self.error(
                        2,
                        "exec cannot replace NSH during automatic configuration",
                        false,
                    )
                } else {
                    let flow = self.exec_external(args, false);
                    if !self.interactive && !matches!(flow, Flow::Exit(_)) {
                        Flow::Exit(flow.code())
                    } else {
                        flow
                    }
                }
            }
            b"export" | b"readonly" => self.attributes(args, name == b"export", fatal),
            b"set" => self.set_builtin(args, fatal),
            b"shift" => match args
                .first()
                .map_or(Some(1), |v| decimal(v))
                .and_then(|n| usize::try_from(n).ok())
            {
                Some(n) if n <= self.args.len() && args.len() <= 1 => {
                    self.args.drain(..n);
                    Flow::Status(0)
                }
                _ => self.error(1, "shift: invalid shift count", fatal),
            },
            b"unset" => self.unset(args, fatal),
            b"trap" => self.trap_builtin(args, fatal),
            b"times" => times(),
            b"cd" => self.cd(args),
            b"pwd" => self.pwd(args),
            b"read" => self.read_builtin(args),
            b"getopts" => self.getopts(args),
            b"alias" => self.alias_builtin(args),
            b"unalias" => {
                let mut code = 0;
                for arg in args {
                    if arg == b"-a" {
                        self.aliases.clear();
                    } else if self.aliases.remove(arg).is_none() {
                        self.error(1, &format!("unalias: {}: not found", show(arg)), false);
                        code = 1;
                    }
                }
                Flow::Status(code)
            }
            b"fc" => crate::fc::run(self, args, tested),
            b"hash" => crate::hash::run(self, args),
            b"kill" => crate::resources::kill(self, args),
            b"ulimit" => crate::resources::ulimit(self, args),
            b"umask" => crate::resources::umask(self, args),
            b"command" => self.command_builtin(args, tested),
            b"type" => self.describe(args, true, false),
            b"jobs" => self.jobs.report(args),
            b"wait" => self.wait_jobs(args),
            b"fg" | b"bg" if args.len() <= 1 => {
                self.resume(args.first().map(Vec::as_slice), name == b"fg")
            }
            b"fg" | b"bg" => self.error(2, "expected at most one job ID", false),
            _ => return None,
        };
        Some(result)
    }

    fn source_builtin(&mut self, args: &[Vec<u8>], tested: bool, fatal: bool) -> Flow {
        let args = if args.first().is_some_and(|a| a == b"--") {
            &args[1..]
        } else {
            args
        };
        let Some(name) = args.first() else {
            return self.error(2, ".: expected a file", fatal);
        };
        let locate = self.locating_source();
        let file = self.search_paths(name).find_map(|path| {
            let path = Path::new(OsStr::from_bytes(&path));
            let file = File::open(path).ok()?;
            let location = locate.then(|| {
                if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    std::env::current_dir()
                        .map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path))
                }
            });
            Some((file, location))
        });
        let Some((file, location)) = file else {
            return self.error(1, &format!(".: {}: file not found", show(name)), fatal);
        };
        if self.call_depth >= 512 {
            return self.error(2, "too many nested calls", fatal);
        }
        let old = (args.len() > 1).then(|| std::mem::replace(&mut self.args, args[1..].to_vec()));
        self.call_depth += 1;
        let result = match location {
            Some(path) => self.run_file_at(file, &path, tested),
            None => self.run_file(file, tested),
        };
        self.call_depth -= 1;
        if let Some(old) = old {
            self.args = old;
        }
        match result {
            Flow::Return(n) => Flow::Status(n),
            other => other,
        }
    }

    fn attributes(&mut self, args: &[Vec<u8>], export: bool, fatal: bool) -> Flow {
        let label = if export {
            b"export".as_slice()
        } else {
            b"readonly".as_slice()
        };
        if args.is_empty() || args[0] == b"-p" {
            let mut out = Vec::new();
            for (name, var) in &self.vars {
                if (export && var.exported) || (!export && var.readonly) {
                    out.extend([label, b" ", name].concat());
                    if let Some(value) = &var.value {
                        out.push(b'=');
                        out.extend(quote(value));
                    }
                    out.push(b'\n');
                }
            }
            return output(&out);
        }
        for arg in args.iter().skip(usize::from(args[0] == b"--")) {
            let name = if let Some((name, value)) = assignment(arg) {
                if let Err(e) = self.assign(name, value.to_vec(), export) {
                    return self.error(1, &e, fatal);
                }
                name
            } else {
                arg
            };
            if !is_name(name) {
                return self.error(2, &format!("{}: invalid name", show(name)), fatal);
            }
            let var = self.vars.entry(name.to_vec()).or_insert(empty_variable());
            if export {
                var.exported = true;
            } else {
                var.readonly = true;
            }
        }
        Flow::Status(0)
    }

    fn set_builtin(&mut self, args: &[Vec<u8>], fatal: bool) -> Flow {
        if args.is_empty() {
            let mut out = Vec::new();
            for (name, value) in self
                .vars
                .iter()
                .filter_map(|(k, v)| v.value.as_ref().map(|v| (k, v)))
            {
                out.extend_from_slice(name);
                out.push(b'=');
                out.extend(quote(value));
                out.push(b'\n');
            }
            return output(&out);
        }
        let mut index = 0;
        while let Some(arg) = args.get(index) {
            if arg == b"--" {
                self.args = args[index + 1..].to_vec();
                return Flow::Status(0);
            }
            if arg.is_empty() || arg == b"-" || arg == b"+" {
                break;
            }
            let enabled = match arg[0] {
                b'-' => true,
                b'+' => false,
                _ => break,
            };
            index += 1;
            for option in &arg[1..] {
                if *option == b'o' {
                    if let Some(name) = args.get(index) {
                        if let Err(e) = set_named(self, name, enabled) {
                            return self.error(2, &e, fatal);
                        }
                        index += 1;
                    } else {
                        return print_options(self, enabled);
                    }
                } else if let Err(e) = set_letter(&mut self.options, *option, enabled) {
                    return self.error(2, &e, fatal);
                }
            }
        }
        if index < args.len() {
            self.args = args[index..].to_vec();
        }
        Flow::Status(0)
    }

    fn unset(&mut self, args: &[Vec<u8>], fatal: bool) -> Flow {
        let f = args.first().is_some_and(|a| a == b"-f");
        let v = args.first().is_some_and(|a| a == b"-v");
        for name in args.iter().skip(usize::from(f || v)) {
            if name == b"--" {
                continue;
            }
            if !is_name(name) {
                return self.error(2, "unset: invalid variable or function name", fatal);
            }
            if f || !v && !self.vars.contains_key(name) {
                self.functions.remove(name);
            } else {
                if self.vars.get(name).is_some_and(|v| v.readonly) {
                    return self.error(1, "unset: readonly variable", fatal);
                }
                self.vars.remove(name);
                if name == b"PATH" {
                    self.hashed_commands.clear();
                }
            }
        }
        Flow::Status(0)
    }

    fn trap_builtin(&mut self, args: &[Vec<u8>], fatal: bool) -> Flow {
        let args = if args.first().is_some_and(|a| a == b"--") {
            &args[1..]
        } else {
            args
        };
        if args.is_empty() || args[0] == b"-p" {
            let mut out = Vec::new();
            for (id, handler) in &self.traps {
                out.extend_from_slice(b"trap -- ");
                out.extend(quote(handler));
                out.extend(format!(" {id}\n").as_bytes());
            }
            return output(&out);
        }
        let (action, signals) = if args[0].as_slice().iter().all(u8::is_ascii_digit) {
            (b"-".as_slice(), args)
        } else {
            (args[0].as_slice(), &args[1..])
        };
        if signals.is_empty() {
            return self.error(2, "trap: missing signal", fatal);
        }
        for signal in signals {
            let Some(sig) = crate::resources::signal_number(signal) else {
                return self.error(2, &format!("trap: {}: unknown signal", show(signal)), fatal);
            };
            if sig != 0 {
                let result = match action {
                    b"-" if self.interactive && sig == libc::SIGINT => sys::catch(sig),
                    b"-" if self.interactive
                        && matches!(
                            sig,
                            libc::SIGTSTP | libc::SIGTTIN | libc::SIGTTOU | libc::SIGQUIT
                        ) =>
                    {
                        sys::signal(sig, libc::SIG_IGN)
                    }
                    b"-" => sys::signal(sig, libc::SIG_DFL),
                    b"" => sys::signal(sig, libc::SIG_IGN),
                    _ => sys::catch(sig),
                };
                if let Err(e) = result {
                    return self.error(1, &format!("trap: {e}"), fatal);
                }
            }
            if action == b"-" {
                self.traps.remove(&sig);
            } else {
                self.traps.insert(sig, action.to_vec());
            }
        }
        Flow::Status(0)
    }

    fn cd(&mut self, args: &[Vec<u8>]) -> Flow {
        let (physical, args) = mode_args(args);
        if args.len() > 1 {
            return self.error(2, "cd: too many arguments", false);
        }
        let old = logical_dir(self).unwrap_or_else(expand::cwd);
        let (print, path) = match args.first().map(Vec::as_slice) {
            Some(b"-") => (true, self.get(b"OLDPWD")),
            Some(path) => (false, Some(path)),
            None => (false, self.get(b"HOME")),
        };
        let Some(path) = path else {
            return self.error(1, "cd: directory variable is unset", false);
        };
        if path.is_empty() {
            return self.error(1, "cd: empty pathname", false);
        }
        let mut paths = Vec::new();
        if !path.starts_with(b"/")
            && !matches!(path.split(|&b| b == b'/').next(), Some(b"." | b".."))
        {
            if let Some(cdpath) = self.get(b"CDPATH") {
                for root in cdpath.split(|&b| b == b':') {
                    let candidate = if root.is_empty() {
                        path.to_vec()
                    } else {
                        [root, b"/", path].concat()
                    };
                    paths.push((print || !root.is_empty(), candidate));
                }
            }
        }
        paths.push((print, path.to_vec()));
        let mut error = None;
        for (print, candidate) in paths {
            let path = Path::new(OsStr::from_bytes(&candidate));
            let target = if physical || old.is_empty() {
                path.to_path_buf()
            } else {
                let Some(path) = normalize(&Path::new(OsStr::from_bytes(&old)).join(path)) else {
                    error = Some(std::io::Error::from_raw_os_error(libc::ENOTDIR));
                    continue;
                };
                path
            };
            match std::env::set_current_dir(&target) {
                Ok(()) => {
                    if self.get(b"PATH").is_some_and(|path| {
                        path.split(|byte| *byte == b':')
                            .any(|dir| !dir.starts_with(b"/"))
                    }) {
                        self.hashed_commands.clear();
                    }
                    let new = if physical || !target.is_absolute() {
                        expand::cwd()
                    } else {
                        target.as_os_str().as_bytes().to_vec()
                    };
                    for (name, value) in [
                        (b"OLDPWD".as_slice(), old),
                        (b"PWD".as_slice(), new.clone()),
                    ] {
                        if let Err(e) = self.assign(name, value, true) {
                            return self.error(1, &e, false);
                        }
                    }
                    return if print {
                        output(&[new, vec![b'\n']].concat())
                    } else {
                        Flow::Status(0)
                    };
                }
                Err(e) => error = Some(e),
            }
        }
        self.error(1, &format!("cd: {}: {}", show(path), error.unwrap()), false)
    }

    fn pwd(&self, args: &[Vec<u8>]) -> Flow {
        let (physical, args) = mode_args(args);
        if !args.is_empty() {
            return self.error(2, "pwd: unexpected operand", false);
        }
        let mut path = (!physical)
            .then(|| logical_dir(self))
            .flatten()
            .unwrap_or_else(expand::cwd);
        if path.is_empty() {
            return self.error(1, "pwd: current directory unavailable", false);
        }
        path.push(b'\n');
        output(&path)
    }

    fn read_builtin(&mut self, args: &[Vec<u8>]) -> Flow {
        let mut raw = false;
        let mut index = 0;
        while let Some(arg) = args.get(index) {
            match arg.as_slice() {
                b"-r" => raw = true,
                b"--" => {
                    index += 1;
                    break;
                }
                _ => break,
            }
            index += 1;
        }
        let defaults = [b"REPLY".to_vec()];
        let names = if index == args.len() {
            &defaults
        } else {
            &args[index..]
        };
        if !names.iter().all(|name| is_name(name)) {
            return self.error(2, "read: invalid variable name", false);
        }
        let mut data = Vec::new();
        let mut escaped = false;
        let mut eof = false;
        loop {
            let b = match sys::byte(0) {
                Ok(Some(b)) => b,
                Ok(None) => {
                    eof = true;
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                    return self.dispatch_signals().unwrap_or(Flow::Status(1));
                }
                Err(e) => return self.error(1, &format!("read: {e}"), false),
            };
            if !raw && !escaped && b == b'\\' {
                escaped = true;
                continue;
            }
            if b == b'\n' && !escaped {
                break;
            }
            if b != 0 && !(b == b'\n' && escaped) {
                data.push((b, escaped));
            }
            escaped = false;
        }
        let ifs = self.ifs().to_vec();
        let delim = |(b, escaped): (u8, bool)| !escaped && ifs.contains(&b);
        let white = |(b, escaped)| delim((b, escaped)) && b" \t\n".contains(&b);
        let mut data = data.as_slice();
        while data.first().is_some_and(|&b| white(b)) {
            data = &data[1..];
        }
        while data.last().is_some_and(|&b| white(b)) {
            data = &data[..data.len() - 1];
        }
        for (i, name) in names.iter().enumerate() {
            let end = if i + 1 == names.len() {
                data.len()
            } else {
                data.iter().position(|&b| delim(b)).unwrap_or(data.len())
            };
            let value = data[..end].iter().map(|(b, _)| *b).collect();
            if let Err(e) = self.assign(name, value, false) {
                return self.error(1, &e, false);
            }
            data = &data[end..];
            if let Some(&first) = data.first() {
                if white(first) {
                    while data.first().is_some_and(|&b| white(b)) {
                        data = &data[1..];
                    }
                }
                if data.first().is_some_and(|&b| delim(b) && !white(b)) {
                    data = &data[1..];
                }
                while data.first().is_some_and(|&b| white(b)) {
                    data = &data[1..];
                }
            }
        }
        Flow::Status(i32::from(eof))
    }

    fn getopts(&mut self, args: &[Vec<u8>]) -> Flow {
        let [spec, name, opts @ ..] = args else {
            return self.error(2, "getopts: requires optstring and name", false);
        };
        if !is_name(name) {
            return self.error(2, "getopts: invalid name", false);
        }
        let opts = if opts.is_empty() { &self.args } else { opts };
        let index = self
            .get(b"OPTIND")
            .and_then(decimal)
            .filter(|&n| n >= 1)
            .unwrap_or(1) as usize
            - 1;
        let silent = spec.starts_with(b":");
        let offset = self.option_offset;
        let mut advance = 0;
        let mut next_offset = 1;
        let mut optarg = None;
        let mut found = false;
        let mut opt = b'?';
        if let Some(arg) = opts
            .get(index)
            .filter(|v| v.starts_with(b"-") && v.len() > 1)
        {
            if arg == b"--" && offset == 1 {
                advance = 1;
            } else if let Some(&letter) = arg.get(offset) {
                opt = letter;
                found = true;
                if offset + 1 >= arg.len() {
                    advance = 1;
                } else {
                    next_offset = offset + 1;
                }
                match spec.iter().position(|b| *b == letter && *b != b':') {
                    None => {
                        opt = b'?';
                        if silent {
                            optarg = Some(vec![letter]);
                        } else {
                            self.error(
                                1,
                                &format!("getopts: unknown option -{}", letter as char),
                                false,
                            );
                        }
                    }
                    Some(p) if spec.get(p + 1) == Some(&b':') => {
                        if offset + 1 < arg.len() {
                            optarg = Some(arg[offset + 1..].to_vec());
                            advance = 1;
                        } else if let Some(value) = opts.get(index + 1) {
                            optarg = Some(value.clone());
                            advance = 2;
                        } else if silent {
                            opt = b':';
                            optarg = Some(vec![letter]);
                        } else {
                            self.error(
                                1,
                                &format!("getopts: option -{} needs an argument", letter as char),
                                false,
                            );
                            opt = b'?';
                        }
                        next_offset = 1;
                    }
                    _ => {}
                }
            }
        }
        for (key, val) in [
            (name.as_slice(), vec![opt]),
            (b"OPTIND", (index + advance + 1).to_string().into_bytes()),
        ] {
            if let Err(e) = self.assign(key, val, false) {
                return self.error(1, &e, false);
            }
        }
        if let Some(arg) = optarg {
            if let Err(e) = self.assign(b"OPTARG", arg, false) {
                return self.error(1, &e, false);
            }
        } else if !self
            .vars
            .get(b"OPTARG".as_slice())
            .is_some_and(|v| v.readonly)
        {
            self.vars.remove(b"OPTARG".as_slice());
        }
        self.option_offset = next_offset;
        Flow::Status(i32::from(!found))
    }

    fn alias_builtin(&mut self, args: &[Vec<u8>]) -> Flow {
        let mut result = 0;
        let mut out = Vec::new();
        if args.is_empty() {
            let mut names = self.aliases.iter().collect::<Vec<_>>();
            names.sort_by_key(|(name, _)| *name);
            for (name, value) in names {
                alias_line(&mut out, name, value);
            }
        }
        for arg in args {
            if arg == b"--" {
                continue;
            }
            if let Some(index) = arg.iter().position(|&b| b == b'=') {
                let name = &arg[..index];
                if name.is_empty() || name.iter().any(|b| b" \n\t/;'\"\\$\x60|&()<>".contains(b)) {
                    result = self.error(1, "alias: invalid name", false).code();
                } else {
                    self.aliases
                        .insert(name.to_vec(), arg[index + 1..].to_vec());
                }
            } else if let Some(value) = self.aliases.get(arg) {
                alias_line(&mut out, arg, value);
            } else {
                result = self
                    .error(1, &format!("alias: {}: not found", show(arg)), false)
                    .code();
            }
        }
        if !out.is_empty() && output(&out).code() != 0 {
            return Flow::Status(1);
        }
        Flow::Status(result)
    }

    fn command_builtin(&mut self, args: &[Vec<u8>], tested: bool) -> Flow {
        let mut describe = None;
        let mut default_path = false;
        let mut i = 0;
        while let Some(arg) = args.get(i) {
            match arg.as_slice() {
                b"-p" => default_path = true,
                b"-v" => describe = Some(false),
                b"-V" => describe = Some(true),
                b"--" => {
                    i += 1;
                    break;
                }
                _ => break,
            }
            i += 1;
        }
        let args = &args[i..];
        if args.is_empty() {
            return Flow::Status(0);
        }
        if let Some(verbose) = describe {
            return self.describe(args, verbose, default_path);
        }
        if !default_path || builtin(&args[0]) {
            return self.dispatch(args, tested, true);
        }
        let original_commands = std::mem::take(&mut self.hashed_commands);
        let original = self.vars.insert(
            b"PATH".to_vec(),
            crate::shell::Variable {
                value: Some(b"/usr/bin:/bin".to_vec()),
                exported: true,
                readonly: false,
            },
        );
        let result = self.dispatch(args, tested, true);
        if let Some(original) = original {
            self.vars.insert(b"PATH".to_vec(), original);
        } else {
            self.vars.remove(b"PATH".as_slice());
        }
        self.hashed_commands = original_commands;
        result
    }

    fn describe(&self, args: &[Vec<u8>], verbose: bool, default_path: bool) -> Flow {
        let mut status = 0;
        for name in args {
            let command = if let Some(value) = self.aliases.get(name) {
                if verbose {
                    [name.as_slice(), b" is aliased to ", &quote(value)].concat()
                } else {
                    [b"alias ", name.as_slice(), b"=", &quote(value)].concat()
                }
            } else if !default_path && self.plugin_command(name).is_some() {
                if verbose {
                    [name.as_slice(), b" is an NSH Lua plugin command"].concat()
                } else {
                    name.clone()
                }
            } else if self.functions.contains_key(name) {
                if verbose {
                    [name.as_slice(), b" is a function"].concat()
                } else {
                    name.clone()
                }
            } else if builtin(name) || name == b"nsh" && self.config {
                if verbose {
                    [name.as_slice(), b" is a shell builtin"].concat()
                } else {
                    name.clone()
                }
            } else if matches!(
                name.as_slice(),
                b"if"
                    | b"then"
                    | b"elif"
                    | b"else"
                    | b"fi"
                    | b"case"
                    | b"in"
                    | b"esac"
                    | b"for"
                    | b"while"
                    | b"until"
                    | b"do"
                    | b"done"
                    | b"{"
                    | b"}"
                    | b"!"
            ) {
                if verbose {
                    [name.as_slice(), b" is a reserved word"].concat()
                } else {
                    name.clone()
                }
            } else {
                let executable = |p: &[u8]| {
                    sys::access(p, libc::X_OK) && !Path::new(OsStr::from_bytes(p)).is_dir()
                };
                let path = if default_path && !name.contains(&b'/') {
                    [
                        [b"/usr/bin/", name.as_slice()].concat(),
                        [b"/bin/", name.as_slice()].concat(),
                    ]
                    .into_iter()
                    .find(|p| executable(p))
                } else {
                    crate::hash::search(self, name)
                        .find(|p| executable(p))
                        .map(std::borrow::Cow::into_owned)
                };
                let Some(path) = path else {
                    status = 1;
                    continue;
                };
                if verbose {
                    [name.as_slice(), b" is ", &path].concat()
                } else {
                    path
                }
            };
            if sys::write(1, &[command, vec![b'\n']].concat()).is_err() {
                return Flow::Status(1);
            }
        }
        Flow::Status(status)
    }
}

pub fn set_letter(opts: &mut crate::shell::Options, option: u8, value: bool) -> Result<(), String> {
    let field = match option {
        b'a' => &mut opts.allexport,
        b'e' => &mut opts.errexit,
        b'f' => &mut opts.noglob,
        b'n' => &mut opts.noexec,
        b'u' => &mut opts.nounset,
        b'v' => &mut opts.verbose,
        b'x' => &mut opts.xtrace,
        b'C' => &mut opts.noclobber,
        _ => return Err(format!("unknown option -{}", option as char)),
    };
    *field = value;
    Ok(())
}

const NAMED: [(&[u8], u8); 9] = [
    (b"allexport", b'a'),
    (b"errexit", b'e'),
    (b"noclobber", b'C'),
    (b"noexec", b'n'),
    (b"noglob", b'f'),
    (b"nounset", b'u'),
    (b"pipefail", 0),
    (b"verbose", b'v'),
    (b"xtrace", b'x'),
];

pub fn named_option(
    opts: &mut crate::shell::Options,
    name: &[u8],
    enabled: bool,
) -> Result<(), String> {
    let Some((_, letter)) = NAMED.iter().find(|(n, _)| *n == name) else {
        return Err(format!("unknown option {}", show(name)));
    };
    if *letter == 0 {
        opts.pipefail = enabled;
        Ok(())
    } else {
        set_letter(opts, *letter, enabled)
    }
}

fn set_named(shell: &mut Shell, name: &[u8], enabled: bool) -> Result<(), String> {
    named_option(&mut shell.options, name, enabled)
}

fn print_options(shell: &Shell, human: bool) -> Flow {
    let current = shell.option_letters();
    let mut out = Vec::new();
    for (name, letter) in NAMED {
        let on = if letter == 0 {
            shell.options.pipefail
        } else {
            current.contains(&letter)
        };
        if human {
            out.extend(
                format!("{:<12} {}\n", show(name), if on { "on" } else { "off" }).as_bytes(),
            );
        } else {
            out.extend(format!("set {}o {}\n", if on { '-' } else { '+' }, show(name)).as_bytes());
        }
    }
    output(&out)
}

fn mode_args(mut args: &[Vec<u8>]) -> (bool, &[Vec<u8>]) {
    let mut physical = false;
    while let Some(value) = args.first() {
        match value.as_slice() {
            b"-P" => physical = true,
            b"-L" => physical = false,
            b"--" => {
                args = &args[1..];
                break;
            }
            _ => break,
        }
        args = &args[1..];
    }
    (physical, args)
}

fn normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                if !out.is_dir() {
                    return None;
                }
                out.pop();
            }
            Component::CurDir => {}
            part => out.push(part),
        }
    }
    Some(out)
}

fn logical_dir(shell: &Shell) -> Option<Vec<u8>> {
    use std::os::unix::fs::MetadataExt;
    let path = shell.get(b"PWD")?;
    let as_path = Path::new(OsStr::from_bytes(path));
    if !as_path.is_absolute()
        || as_path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return None;
    }
    let here = std::fs::metadata(".").ok()?;
    let recorded = std::fs::metadata(as_path).ok()?;
    (here.dev() == recorded.dev() && here.ino() == recorded.ino()).then(|| path.to_vec())
}

fn times() -> Flow {
    // times initializes the struct and sysconf returns the active clock tick frequency.
    let (clock, time, ticks) = unsafe {
        let mut time: libc::tms = std::mem::zeroed();
        let clock = libc::times(&mut time);
        (clock, time, libc::sysconf(libc::_SC_CLK_TCK))
    };
    if clock == -1 || ticks <= 0 {
        return Flow::Status(1);
    }
    let fmt = |number| {
        let s = number as f64 / ticks as f64;
        format!("{}m{:.3}s", (s / 60.0) as u64, s % 60.0)
    };
    output(
        format!(
            "{} {}\n{} {}\n",
            fmt(time.tms_utime),
            fmt(time.tms_stime),
            fmt(time.tms_cutime),
            fmt(time.tms_cstime)
        )
        .as_bytes(),
    )
}

fn quote(value: &[u8]) -> Vec<u8> {
    let mut out = vec![b'\''];
    for &byte in value {
        if byte == b'\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            out.push(byte);
        }
    }
    out.push(b'\'');
    out
}

fn alias_line(out: &mut Vec<u8>, name: &[u8], value: &[u8]) {
    out.extend([name, b"=", &quote(value), b"\n"].concat());
}
