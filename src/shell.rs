use std::collections::{BTreeMap, HashMap};
use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, IntoRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::expand;
use crate::jobs::Jobs;
use crate::session::{Action, Session};
use crate::syntax::{self, AndOr, Chain, Command, Kind, List, Pipeline, Redir, RedirKind};
use crate::sys;

#[derive(Clone)]
pub struct Variable {
    pub value: Option<Vec<u8>>,
    pub exported: bool,
    pub readonly: bool,
}

#[derive(Clone, Default)]
pub struct Options {
    pub allexport: bool,
    pub errexit: bool,
    pub noclobber: bool,
    pub noexec: bool,
    pub noglob: bool,
    pub nounset: bool,
    pub pipefail: bool,
    pub verbose: bool,
    pub xtrace: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Status(i32),
    Exit(i32),
    Return(i32),
    Break(usize),
    Continue(usize),
}

impl Flow {
    pub fn code(self) -> i32 {
        match self {
            Self::Status(n) | Self::Exit(n) | Self::Return(n) => n,
            Self::Break(_) | Self::Continue(_) => 0,
        }
    }
}

pub struct Shell {
    pub vars: BTreeMap<Vec<u8>, Variable>,
    pub args: Vec<Vec<u8>>,
    pub arg0: Vec<u8>,
    pub options: Options,
    pub status: i32,
    pub jobs: Jobs,
    pub aliases: HashMap<Vec<u8>, Vec<u8>>,
    pub(crate) hashed_commands: BTreeMap<Vec<u8>, Vec<u8>>,
    pub functions: BTreeMap<Vec<u8>, Arc<Command>>,
    pub traps: BTreeMap<i32, Vec<u8>>,
    pub session: Session,
    pub startup_failure: Option<sys::StartupFailure>,
    pub plugins: Option<Box<crate::plugins::Plugins>>,
    pub plugin_allowed: bool,
    pub plugin_pending: Option<std::collections::BTreeSet<String>>,
    pub suppress_history: bool,
    pub interactive: bool,
    pub config: bool,
    pub loops: usize,
    pub call_depth: usize,
    pub option_offset: usize,
    pid: i32,
    pub(crate) substitution: Option<i32>,
    running_trap: bool,
    pub exec_redirs: bool,
    errexit_exempt: bool,
    command_site: Option<Arc<syntax::CommandSite>>,
    failed_site: Option<Arc<syntax::CommandSite>>,
}

impl Shell {
    pub fn new(
        arg0: Vec<u8>,
        args: Vec<Vec<u8>>,
        options: Options,
        interactive: bool,
        config: bool,
        persist: bool,
    ) -> Self {
        let mut vars = std::env::vars_os()
            .map(|(name, value)| {
                (
                    name.into_vec(),
                    Variable {
                        value: Some(value.into_vec()),
                        readonly: false,
                        exported: true,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        // Imported IFS must not affect expansion or scripts.
        vars.insert(
            b"IFS".to_vec(),
            Variable {
                value: Some(b" \t\n".to_vec()),
                exported: false,
                readonly: false,
            },
        );
        vars.entry(b"OPTIND".to_vec()).or_insert(Variable {
            value: Some(b"1".to_vec()),
            exported: false,
            readonly: false,
        });
        Self {
            vars,
            args,
            arg0,
            options,
            interactive,
            config,
            status: 0,
            jobs: Jobs::new(),
            aliases: HashMap::new(),
            hashed_commands: BTreeMap::new(),
            functions: BTreeMap::new(),
            traps: BTreeMap::new(),
            session: Session::new(persist),
            startup_failure: None,
            plugins: None,
            plugin_allowed: false,
            plugin_pending: None,
            suppress_history: false,
            loops: 0,
            call_depth: 0,
            option_offset: 1,
            pid: sys::pid(),
            substitution: None,
            running_trap: false,
            exec_redirs: false,
            errexit_exempt: false,
            command_site: None,
            failed_site: None,
        }
    }

    pub fn get(&self, name: &[u8]) -> Option<&[u8]> {
        self.vars.get(name)?.value.as_deref()
    }

    pub fn ifs(&self) -> &[u8] {
        self.get(b"IFS").unwrap_or(b" \t\n")
    }

    pub fn assign(&mut self, name: &[u8], value: Vec<u8>, exported: bool) -> Result<(), String> {
        if !is_name(name) {
            return Err(format!("{}: invalid variable name", show(name)));
        }
        let slot = self.vars.entry(name.to_vec()).or_insert(empty_variable());
        if slot.readonly {
            return Err(format!("{}: readonly variable", show(name)));
        }
        slot.value = Some(value);
        slot.exported |= exported || self.options.allexport;
        if name == b"OPTIND" {
            self.option_offset = 1;
        } else if name == b"PATH" {
            self.hashed_commands.clear();
        }
        Ok(())
    }

    pub fn parameter(&self, name: &[u8]) -> Option<Vec<u8>> {
        Some(match name {
            b"0" => self.arg0.clone(),
            b"?" => self.status.to_string().into_bytes(),
            b"$" => self.pid.to_string().into_bytes(),
            b"#" => self.args.len().to_string().into_bytes(),
            b"!" => self.jobs.last_background?.to_string().into_bytes(),
            b"-" => self.option_letters(),
            b"*" | b"@" => self.args.join(&self.ifs()[..self.ifs().len().min(1)]),
            name if name[0].is_ascii_digit() => {
                let n = std::str::from_utf8(name).ok()?.parse::<usize>().ok()?;
                if n == 0 {
                    self.arg0.clone()
                } else {
                    self.args.get(n - 1)?.clone()
                }
            }
            _ => self.get(name)?.to_vec(),
        })
    }

    pub fn option_letters(&self) -> Vec<u8> {
        let mut letters = Vec::new();
        for (active, letter) in [
            (self.options.allexport, b'a'),
            (self.options.errexit, b'e'),
            (self.options.noglob, b'f'),
            (self.interactive, b'i'),
            (self.jobs.controlled(), b'm'),
            (self.options.noexec, b'n'),
            (self.options.nounset, b'u'),
            (self.options.verbose, b'v'),
            (self.options.xtrace, b'x'),
            (self.options.noclobber, b'C'),
        ] {
            if active {
                letters.push(letter);
            }
        }
        letters
    }

    pub fn code(&mut self, source: &[u8], tested: bool) -> Flow {
        self.code_from(source, tested, None)
    }

    pub(crate) fn code_at(&mut self, path: &Path, source: &[u8], tested: bool) -> Flow {
        self.code_from(
            source,
            tested,
            Some(&syntax::SourceLocation::new(source, path)),
        )
    }

    fn sourced_code_at(&mut self, path: &Path, source: &[u8], tested: bool) -> Flow {
        if let Some((line, command)) = self.startup_fence(source) {
            return self.report_markdown_fence(path, line, command);
        }
        self.code_at(path, source, tested)
    }

    fn code_from(
        &mut self,
        source: &[u8],
        tested: bool,
        location: Option<&syntax::SourceLocation<'_>>,
    ) -> Flow {
        if self.options.noexec {
            return match self.parse_code(source, location, 0, false) {
                Ok(_) => Flow::Status(0),
                Err(e) => self.parse_error(location, 0, &e),
            };
        }
        let mut offset = 0;
        let mut result = Flow::Status(self.status);
        while offset < source.len() {
            let parsed = self.parse_code(source, location, offset, true);
            match parsed {
                Ok((list, used)) if used != 0 => {
                    if self.options.verbose {
                        let _ = sys::write(2, &source[offset..offset + used]);
                    }
                    offset += used;
                    result = self.execute(&list, tested);
                    if !matches!(result, Flow::Status(_)) {
                        break;
                    }
                }
                Ok(_) => break,
                Err(e) if e.incomplete => {
                    return match self.parse_code(source, location, offset, false) {
                        Ok((list, _)) => self.execute(&list, tested),
                        Err(error) => self.parse_error(location, offset, &error),
                    };
                }
                Err(e) => return self.parse_error(location, offset, &e),
            }
        }
        result
    }

    fn parse_code(
        &self,
        source: &[u8],
        location: Option<&syntax::SourceLocation<'_>>,
        offset: usize,
        one_input: bool,
    ) -> Result<(List, usize), syntax::ParseError> {
        if let Some(location) = location {
            syntax::parse_located(location, offset, &self.aliases, one_input)
        } else if one_input {
            syntax::parse_next(&source[offset..], &self.aliases)
        } else {
            syntax::parse(&source[offset..], &self.aliases)
                .map(|list| (list, source.len() - offset))
        }
    }

    fn parse_error(
        &self,
        location: Option<&syntax::SourceLocation<'_>>,
        offset: usize,
        error: &syntax::ParseError,
    ) -> Flow {
        if let Some(location) = location {
            self.mark_startup_failed();
            diagnostic(&location.syntax_error(offset, error));
            self.error_status(2, true)
        } else {
            self.error(2, &error.message, true)
        }
    }

    pub fn input(&mut self, line: impl FnMut(&mut Vec<u8>) -> io::Result<usize>) -> Flow {
        self.input_from(line, None)
    }

    fn input_from(
        &mut self,
        mut line: impl FnMut(&mut Vec<u8>) -> io::Result<usize>,
        path: Option<Arc<PathBuf>>,
    ) -> Flow {
        let mut source = Vec::new();
        let mut first_line = 1;
        let mut processed = 0;
        let mut needs_input = true;
        let mut eof = false;
        loop {
            if needs_input {
                if processed == source.len() {
                    source.clear();
                } else if processed != 0 {
                    source.drain(..processed);
                }
                processed = 0;
                match line(&mut source) {
                    Ok(n) => eof = n == 0,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                        if let Some(flow) = self.dispatch_signals() {
                            return flow;
                        }
                        continue;
                    }
                    Err(e) => return self.error(2, &e.to_string(), true),
                }
            }
            let remaining = &source[processed..];
            let location = path.as_ref().map(|path| {
                syntax::SourceLocation::at_line(remaining, Arc::clone(path), first_line)
            });
            if let Some(path) = &path {
                if let Some((line, command)) = self.startup_fence(remaining) {
                    return self.report_markdown_fence(path, first_line + line - 1, command);
                }
            }
            if self.options.noexec {
                if eof {
                    return if remaining.is_empty() {
                        Flow::Status(0)
                    } else {
                        self.code_from(remaining, false, location.as_ref())
                    };
                }
                needs_input = true;
                continue;
            }
            if remaining.is_empty() {
                return Flow::Status(self.status);
            }
            let (list, used) = match self.parse_code(remaining, location.as_ref(), 0, true) {
                Ok((list, used)) => (list, used),
                Err(e) if e.incomplete && !eof => {
                    needs_input = true;
                    continue;
                }
                Err(e) if e.incomplete => {
                    return self.code_from(remaining, false, location.as_ref());
                }
                Err(e) => return self.parse_error(location.as_ref(), 0, &e),
            };
            if self.options.verbose {
                let _ = sys::write(2, &remaining[..used]);
            }
            let flow = self.execute(&list, false);
            if !matches!(flow, Flow::Status(_)) {
                return flow;
            }
            if used != 0 {
                if path.is_some() {
                    first_line += remaining[..used]
                        .iter()
                        .filter(|&&byte| byte == b'\n')
                        .count();
                }
                processed += used;
            } else {
                processed = source.len();
            }
            needs_input = processed == source.len();
            if eof && needs_input {
                return flow;
            }
        }
    }

    pub fn run_file(&mut self, file: File, tested: bool) -> Flow {
        self.run_file_from(file, tested, None)
    }

    pub(crate) fn run_file_at(&mut self, file: File, path: &Path, tested: bool) -> Flow {
        self.run_file_from(file, tested, Some(path))
    }

    fn run_file_from(&mut self, file: File, tested: bool, path: Option<&Path>) -> Flow {
        // Source and script descriptors stay out of the usual user fd range.
        let file = sys::dup_min(file.as_raw_fd(), 64)
            .map(File::from)
            .unwrap_or(file);
        let mut reader = BufReader::new(file);
        if tested {
            let mut bytes = Vec::new();
            return match reader.read_to_end(&mut bytes) {
                Ok(_) => match path {
                    Some(path) => self.sourced_code_at(path, &bytes, true),
                    None => self.code(&bytes, true),
                },
                Err(e) => self.error(2, &e.to_string(), true),
            };
        }
        self.input_from(
            |source| {
                if path.is_some() {
                    reader.read_until(b'\n', source)
                } else {
                    read_buffered_lines(&mut reader, source)
                }
            },
            path.map(|path| Arc::new(path.to_path_buf())),
        )
    }

    pub fn execute(&mut self, list: &List, tested: bool) -> Flow {
        for item in list {
            if let Some(flow) = self.dispatch_signals() {
                return flow;
            }
            let flow = if item.background {
                self.spawn_background(&item.chain)
            } else {
                self.chain(&item.chain, tested)
            };
            self.status = flow.code();
            if !matches!(flow, Flow::Status(_)) {
                return flow;
            }
        }
        self.dispatch_signals().unwrap_or(Flow::Status(self.status))
    }

    pub(crate) fn chain(&mut self, chain: &Chain, tested: bool) -> Flow {
        let flow = self.pipeline(&chain.first, tested || !chain.rest.is_empty());
        let Flow::Status(mut status) = flow else {
            return flow;
        };
        self.status = status;
        let mut exempt = !chain.rest.is_empty() && status != 0;
        for (index, (op, pipeline)) in chain.rest.iter().enumerate() {
            if matches!(op, AndOr::And) != (status == 0) {
                continue;
            }
            let flow = self.pipeline(pipeline, tested || index + 1 != chain.rest.len());
            let Flow::Status(next) = flow else {
                return flow;
            };
            status = next;
            self.status = status;
            exempt = self.errexit_exempt;
        }
        self.errexit_exempt |= exempt;
        Flow::Status(status)
    }

    fn pipeline(&mut self, pipeline: &Pipeline, tested: bool) -> Flow {
        let invert = pipeline.negate;
        self.errexit_exempt = false;
        let flow = if pipeline.commands.len() == 1 {
            self.run_command(&pipeline.commands[0], tested || invert)
        } else {
            self.at_command_site(&pipeline.commands[0], |shell| {
                shell.spawn_pipeline(pipeline)
            })
        };
        if self.startup_failed() {
            return Flow::Exit(flow.code().max(1));
        }
        match flow {
            Flow::Status(n) if invert => {
                self.errexit_exempt = n == 0;
                Flow::Status(i32::from(n == 0))
            }
            Flow::Status(n)
                if n != 0 && self.options.errexit && !tested && !self.errexit_exempt =>
            {
                Flow::Exit(n)
            }
            Flow::Status(n) if n != 0 && tested => {
                self.errexit_exempt = true;
                Flow::Status(n)
            }
            other => other,
        }
    }

    pub fn run_command(&mut self, command: &Command, tested: bool) -> Flow {
        self.at_command_site(command, |shell| shell.run_command_inner(command, tested))
    }

    fn at_command_site(&mut self, command: &Command, run: impl FnOnce(&mut Self) -> Flow) -> Flow {
        let Some(site) = &command.site else {
            return run(self);
        };
        let previous = self.command_site.replace(Arc::clone(site));
        let previous_failure = self.failed_site.take();
        let flow = run(self);
        self.command_site = previous;
        if self.failed_site.is_none() {
            self.failed_site = if flow.code() != 0 && self.startup_failure.is_some() {
                Some(Arc::clone(site))
            } else {
                previous_failure
            };
        }
        flow
    }

    fn run_command_inner(&mut self, command: &Command, tested: bool) -> Flow {
        if let Kind::Simple(words) = &command.kind {
            return self.simple(words, &command.redirs, tested, false);
        }
        if let Kind::Function(name, body) = &command.kind {
            let mut body = (**body).clone();
            body.redirs.extend(command.redirs.clone());
            self.functions.insert(name.clone(), Arc::new(body));
            return Flow::Status(0);
        }
        let mut restore = Fds::new(&command.redirs);
        if let Err(error) = self.redirections(&command.redirs, &mut restore) {
            return self.error(1, &error, false);
        }
        match &command.kind {
            Kind::Group(list) => self.execute(list, tested),
            Kind::Subshell(list) => self.spawn_subshell(list, tested),
            Kind::If(clauses, otherwise) => {
                for (condition, body) in clauses {
                    match self.execute(condition, true) {
                        Flow::Status(0) => return self.execute(body, tested),
                        Flow::Status(_) => {}
                        flow => return flow,
                    }
                }
                if otherwise.is_empty() {
                    Flow::Status(0)
                } else {
                    self.execute(otherwise, tested)
                }
            }
            Kind::Loop {
                condition,
                body,
                until,
            } => {
                self.loops += 1;
                let mut last = 0;
                let flow = loop {
                    match self.execute(condition, true) {
                        Flow::Status(n) if (n == 0) == *until => break Flow::Status(last),
                        Flow::Status(_) => {}
                        flow => break loop_outer(flow),
                    }
                    match self.execute(body, tested) {
                        Flow::Status(n) => last = n,
                        Flow::Continue(1) => {}
                        flow => break loop_outer(flow),
                    }
                };
                self.loops -= 1;
                flow
            }
            Kind::For { name, words, body } => {
                let items = if let Some(words) = words {
                    match self.fields(words) {
                        Ok(items) => items,
                        Err(e) => return self.error(1, &e, true),
                    }
                } else {
                    self.args.clone()
                };
                self.loops += 1;
                let mut flow = Flow::Status(0);
                for item in items {
                    if let Err(e) = self.assign(name, item, false) {
                        flow = self.error(1, &e, true);
                        break;
                    }
                    match self.execute(body, tested) {
                        Flow::Status(n) => flow = Flow::Status(n),
                        Flow::Continue(1) => {}
                        other => {
                            flow = loop_outer(other);
                            break;
                        }
                    }
                }
                self.loops -= 1;
                flow
            }
            Kind::Case { word, arms } => {
                let value = match expand::scalar(self, &word.0) {
                    Ok(value) => value,
                    Err(e) => return self.error(1, &e, true),
                };
                let mut flow = Flow::Status(0);
                let mut fallthrough = false;
                for arm in arms {
                    let mut matched = fallthrough;
                    if !matched {
                        for pat in &arm.patterns {
                            match expand::pattern(self, &pat.0) {
                                Ok(pat) if expand::matches(&pat, &value) => {
                                    matched = true;
                                    break;
                                }
                                Ok(_) => {}
                                Err(e) => return self.error(1, &e, true),
                            }
                        }
                    }
                    if matched {
                        if !arm.body.is_empty() {
                            flow = self.execute(&arm.body, tested);
                        }
                        if !arm.fallthrough || !matches!(flow, Flow::Status(_)) {
                            return flow;
                        }
                        fallthrough = true;
                    }
                }
                flow
            }
            Kind::Simple(_) | Kind::Function(_, _) => unreachable!(),
        }
    }

    fn fields(&mut self, words: &[syntax::Word]) -> Result<Vec<Vec<u8>>, String> {
        let mut args = Vec::new();
        for word in words {
            expand::words_into(self, &word.0, &mut args)?;
        }
        Ok(args)
    }

    fn command_words(&mut self, words: &[syntax::Word]) -> Result<Vec<Vec<u8>>, String> {
        let mut args: Vec<Vec<u8>> = Vec::new();
        for word in words {
            let name = match args.as_slice() {
                [name, rest @ ..] if name == b"command" => {
                    rest.iter().find(|arg| !arg.starts_with(b"-"))
                }
                [name, ..] => Some(name),
                [] => None,
            };
            if name.is_some_and(|name| matches!(name.as_slice(), b"export" | b"readonly")) {
                if let Some((name, value)) = assignment(&word.0) {
                    args.push([name, b"=", &expand::assignment_value(self, value)?].concat());
                    continue;
                }
            }
            expand::words_into(self, &word.0, &mut args)?;
        }
        Ok(args)
    }

    /// The caller owns a disposable job worker. A successful external exec
    /// can replace it once this single simple command has expanded and redirected.
    pub fn run_worker_command(&mut self, command: &Command, tested: bool) -> Flow {
        self.at_command_site(command, |shell| match &command.kind {
            Kind::Simple(words) => shell.simple(words, &command.redirs, tested, true),
            _ => shell.run_command_inner(command, tested),
        })
    }

    fn simple(
        &mut self,
        words: &[syntax::Word],
        redirs: &[Redir],
        tested: bool,
        direct_external: bool,
    ) -> Flow {
        self.substitution = None;
        let prefixed_count = words
            .iter()
            .take_while(|word| assignment(&word.0).is_some())
            .count();
        let args = match self.command_words(&words[prefixed_count..]) {
            Ok(args) => args,
            Err(e) => return self.error(1, &e, true),
        };
        let name = args.first().map(Vec::as_slice).unwrap_or_default();
        let special = crate::builtins::special(name);
        let persistent = args.is_empty() || special;
        let mut restore = Fds::new(redirs);
        if let Err(e) = self.redirections(redirs, &mut restore) {
            return self.error(1, &e, special || args.is_empty());
        }
        let mut saved = Vec::new();
        let mut saved_commands = None;
        for word in &words[..prefixed_count] {
            let (name, raw) = assignment(&word.0).unwrap();
            if !persistent {
                saved.push((name, self.vars.get(name).cloned()));
                if name == b"PATH" && saved_commands.is_none() {
                    saved_commands = Some(std::mem::take(&mut self.hashed_commands));
                }
            }
            let result = expand::assignment_value(self, raw)
                .and_then(|value| self.assign(name, value, !persistent));
            if let Err(e) = result {
                restore_vars(&mut self.vars, saved);
                if let Some(commands) = saved_commands {
                    self.hashed_commands = commands;
                }
                return self.error(1, &e, true);
            }
        }
        if self.options.xtrace && !args.is_empty() {
            let mut line = self.get(b"PS4").unwrap_or(b"+ ").to_vec();
            line.extend_from_slice(&args.join(&b' '));
            line.push(b'\n');
            let _ = sys::write(2, &line);
        }
        self.exec_redirs = false;
        let flow = if args.is_empty() {
            Flow::Status(self.substitution.take().unwrap_or(0))
        } else {
            self.dispatch_simple(&args, tested, false, direct_external)
        };
        if self.exec_redirs {
            restore.saved.clear();
            self.exec_redirs = false;
        }
        restore_vars(&mut self.vars, saved);
        if let Some(commands) = saved_commands {
            self.hashed_commands = commands;
        }
        flow
    }

    pub fn dispatch(&mut self, args: &[Vec<u8>], tested: bool, bypass_func: bool) -> Flow {
        self.dispatch_simple(args, tested, bypass_func, false)
    }

    fn dispatch_simple(
        &mut self,
        args: &[Vec<u8>],
        tested: bool,
        bypass_func: bool,
        direct_external: bool,
    ) -> Flow {
        let name = args[0].as_slice();
        if !bypass_func
            && name == b"nsh"
            && args.get(1).is_some_and(|arg| arg == b"plugin")
            && (self.config || self.plugin_allowed)
        {
            return self.plugin_manage(&args[2..]);
        }
        if !bypass_func && self.plugins.is_some() {
            let event_args = args;
            self.run_plugin_event("before-dispatch", event_args);
            let flow = match self.plugin_command(name) {
                Some(crate::plugin_lua::CommandMode::Wrap) => {
                    match self.dispatch_base(args, tested, bypass_func, false) {
                        Flow::Status(code) => self.run_plugin(args, Some(code)),
                        other => other,
                    }
                }
                Some(crate::plugin_lua::CommandMode::Register)
                    if self.functions.contains_key(name) =>
                {
                    self.dispatch_base(args, tested, bypass_func, direct_external)
                }
                Some(_) => self.run_plugin(args, None),
                None => {
                    let needs_post = self
                        .plugins
                        .as_ref()
                        .is_some_and(|plugins| plugins.has_event("after-dispatch"));
                    self.dispatch_base(args, tested, bypass_func, direct_external && !needs_post)
                }
            };
            if self
                .plugins
                .as_ref()
                .is_some_and(|plugins| plugins.has_event("after-dispatch"))
            {
                let mut data = args.to_vec();
                data.insert(0, flow.code().to_string().into_bytes());
                self.run_plugin_event("after-dispatch", &data);
            }
            return flow;
        }
        self.dispatch_base(args, tested, bypass_func, direct_external)
    }

    fn dispatch_base(
        &mut self,
        args: &[Vec<u8>],
        tested: bool,
        bypass_func: bool,
        direct_external: bool,
    ) -> Flow {
        let name = args[0].as_slice();
        if !bypass_func && !crate::builtins::special(name) {
            if name == b"nsh" && self.config && args.len() > 1 && !args[1].starts_with(b"-") {
                return self.setting(&args[1..], tested);
            }
            if let Some(command) = self.functions.get(name).cloned() {
                return self.function(&command, &args[1..], tested);
            }
        }
        if let Some(flow) = self.builtin(name, &args[1..], tested, bypass_func) {
            flow
        } else if direct_external {
            self.exec_external(args, true)
        } else {
            self.spawn_external(args, true)
        }
    }

    pub fn function(&mut self, command: &Command, args: &[Vec<u8>], tested: bool) -> Flow {
        if self.call_depth >= 512 {
            return self.error(2, "too many nested calls", true);
        }
        let old = std::mem::replace(&mut self.args, args.to_vec());
        self.call_depth += 1;
        let flow = self.run_command(command, tested);
        self.call_depth -= 1;
        self.args = old;
        match flow {
            Flow::Return(n) => Flow::Status(n),
            other => other,
        }
    }

    pub(crate) fn setting(&mut self, args: &[Vec<u8>], tested: bool) -> Flow {
        let flow = match self.session.command(args) {
            Ok(Action::Done) => Flow::Status(0),
            Ok(Action::Print(text)) => output(text.as_bytes()),
            Ok(Action::Source { path, source }) => {
                self.call_depth += 1;
                let result = self.sourced_code_at(&path, &source, tested);
                self.call_depth -= 1;
                self.session.finish_module(result.code() == 0);
                match result {
                    Flow::Return(n) => Flow::Status(n),
                    other => other,
                }
            }
            Ok(Action::SyntaxCheck(path)) => self.check_configuration(&path),
            Err(message) => self.error(2, &message, false),
        };
        if flow.code() != 0 {
            if let Some(failure) = &self.startup_failure {
                failure.record();
            }
        }
        flow
    }

    fn check_configuration(&self, path: &Path) -> Flow {
        let data = match std::fs::read(path) {
            Ok(data) => data,
            Err(error) => return self.error(1, &format!("{}: {error}", path.display()), false),
        };
        if let Some((line, command)) = syntax::configuration_fence(&data) {
            return self.report_markdown_fence(path, line, command);
        }
        let source = syntax::SourceLocation::new(&data, path);
        match syntax::parse_located(&source, 0, &self.aliases, false) {
            Ok(_) => output(b"Syntax OK. Dynamic settings are checked when loaded.\n"),
            Err(error) => {
                diagnostic(&source.syntax_error(0, &error));
                Flow::Status(2)
            }
        }
    }

    fn startup_fence<'a>(&self, source: &'a [u8]) -> Option<(usize, &'a [u8])> {
        self.startup_failure.as_ref()?;
        syntax::configuration_fence(source)
    }

    fn report_markdown_fence(&self, path: &Path, line: usize, command: &[u8]) -> Flow {
        self.mark_startup_failed();
        diagnostic(&format!(
            "{}:{line}: {:?}: Markdown code fence is not shell source",
            path.display(),
            show(command)
        ));
        Flow::Status(2)
    }

    pub fn redirections(&mut self, redirs: &[Redir], fds: &mut Fds) -> Result<(), String> {
        for redir in redirs {
            let fd = redir.fd;
            fds.save(fd).map_err(|e| format!("descriptor {fd}: {e}"))?;
            let result = match &redir.kind {
                RedirKind::Here {
                    body,
                    expand: subst,
                } => {
                    let data = if *subst {
                        expand::here(self, body)?
                    } else {
                        body.clone()
                    };
                    let source = sys::data_file(&data, fds.min).map_err(|e| e.to_string())?;
                    sys::dup_to(source.as_raw_fd(), fd)
                }
                RedirKind::DupRead | RedirKind::DupWrite => {
                    let word = expand::scalar(self, &redir.word.0)?;
                    if word == b"-" {
                        sys::close(fd);
                        Ok(())
                    } else {
                        let from = decimal(&word).ok_or("invalid descriptor")?;
                        if fds.saved.iter().any(|(_, save)| {
                            save.as_ref().is_some_and(|save| save.as_raw_fd() == from)
                        }) {
                            return Err(format!("descriptor {from} is not open"));
                        }
                        sys::dup_to(from, fd)
                    }
                }
                kind => {
                    let path = expand::scalar(self, &redir.word.0)?;
                    let flags = match kind {
                        RedirKind::Read => libc::O_RDONLY,
                        RedirKind::ReadWrite => libc::O_RDWR | libc::O_CREAT,
                        RedirKind::Write
                            if self.options.noclobber
                                && !std::fs::metadata(OsStr::from_bytes(&path))
                                    .is_ok_and(|m| !m.is_file()) =>
                        {
                            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL
                        }
                        RedirKind::Write | RedirKind::Clobber => {
                            libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC
                        }
                        RedirKind::Append => libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
                        _ => unreachable!(),
                    };
                    let source =
                        sys::open(&path, flags).map_err(|e| format!("{}: {e}", show(&path)))?;
                    sys::dup_to(source.as_raw_fd(), fd).map_err(|e| e.to_string())?;
                    if source.as_raw_fd() == fd {
                        let _ = source.into_raw_fd();
                    }
                    Ok(())
                }
            };
            result.map_err(|e| format!("descriptor {fd}: {e}"))?;
        }
        Ok(())
    }

    pub fn error(&self, code: i32, message: &str, fatal: bool) -> Flow {
        self.mark_startup_failed();
        if let Some(site) = &self.command_site {
            diagnostic(&site.report(message));
        } else {
            diagnostic(message);
        }
        self.error_status(code, fatal)
    }

    fn error_status(&self, code: i32, fatal: bool) -> Flow {
        if fatal && !self.interactive {
            Flow::Exit(code)
        } else {
            Flow::Status(code)
        }
    }

    fn mark_startup_failed(&self) {
        if let Some(failure) = &self.startup_failure {
            failure.record();
        }
    }

    fn startup_failed(&self) -> bool {
        self.startup_failure
            .as_ref()
            .is_some_and(sys::StartupFailure::failed)
    }

    pub(crate) fn locating_source(&self) -> bool {
        self.command_site.is_some() || self.startup_failure.is_some()
    }

    pub(crate) fn reset_source_failure(&mut self) {
        self.failed_site = None;
    }

    pub(crate) fn owns_startup_transaction(&self) -> bool {
        self.startup_failure.is_some() && self.pid == sys::pid()
    }

    pub(crate) fn is_fork_worker(&self) -> bool {
        self.pid != sys::pid()
    }

    pub(crate) fn report_source_failure(&mut self, path: &Path, status: i32) {
        let message = format!("configuration transaction discarded (status {status})");
        if let Some(site) = self.failed_site.take() {
            diagnostic(&site.report(&message));
        } else {
            diagnostic(&format!("{}: {message}", path.display()));
        }
    }

    pub fn environment(&self) -> Vec<CString> {
        self.vars
            .iter()
            .filter_map(|(key, val)| {
                let value = val.value.as_ref().filter(|_| val.exported)?;
                let mut entry = Vec::with_capacity(key.len() + value.len() + 2);
                entry.extend_from_slice(key);
                entry.push(b'=');
                entry.extend_from_slice(value);
                CString::new(entry).ok()
            })
            .collect()
    }

    pub fn enter_child(&mut self) {
        self.interactive = false;
        self.jobs = self.jobs.child();
        self.loops = 0;
        self.traps.retain(|sig, action| {
            if *sig == 0 {
                return false;
            }
            if action.is_empty() {
                return true;
            }
            let _ = sys::signal(*sig, libc::SIG_DFL);
            false
        });
        for sig in [
            libc::SIGINT,
            libc::SIGQUIT,
            libc::SIGPIPE,
            libc::SIGTSTP,
            libc::SIGTTIN,
            libc::SIGTTOU,
            libc::SIGCHLD,
        ] {
            if !self.traps.contains_key(&sig) {
                let _ = sys::signal(sig, libc::SIG_DFL);
            }
        }
        sys::signals_pending();
    }

    pub fn dispatch_signals(&mut self) -> Option<Flow> {
        self.jobs.reap();
        if self.running_trap {
            return None;
        }
        self.process_signals(sys::signals_pending())
    }

    pub(crate) fn wait_signal(&mut self) -> Option<Flow> {
        if self.running_trap {
            return None;
        }
        let pending = sys::signals_pending();
        let interrupted = (1..64).find(|n| {
            pending & (1u64 << n) != 0
                && (*n == libc::SIGINT
                    || self.traps.get(n).is_some_and(|action| !action.is_empty()))
        });
        self.process_signals(pending)
            .or_else(|| interrupted.map(|sig| Flow::Status(128 + sig)))
    }

    fn process_signals(&mut self, pending: u64) -> Option<Flow> {
        for n in 1..64 {
            if pending & (1u64 << n) != 0 {
                if let Some(action) = self.traps.get(&n).cloned() {
                    if action.is_empty() {
                        continue;
                    }
                    let flow = self.invoke_trap(&action);
                    if !matches!(flow, Flow::Status(_)) {
                        return Some(flow);
                    }
                } else if n == libc::SIGHUP {
                    return Some(Flow::Exit(128 + n));
                } else if n == libc::SIGINT {
                    self.status = 128 + n;
                    if !self.interactive {
                        return Some(Flow::Exit(self.status));
                    }
                }
            }
        }
        None
    }

    pub fn invoke_trap(&mut self, action: &[u8]) -> Flow {
        let original = (self.status, self.errexit_exempt);
        self.running_trap = true;
        let flow = self.code(action, true);
        self.running_trap = false;
        (self.status, self.errexit_exempt) = original;
        flow
    }

    pub fn exit_trap(&mut self, status: i32) -> i32 {
        self.status = status;
        if let Some(action) = self.traps.remove(&0) {
            if let Flow::Exit(code) = self.invoke_trap(&action) {
                return code;
            }
        }
        status
    }
}

fn read_buffered_lines(reader: &mut impl BufRead, source: &mut Vec<u8>) -> io::Result<usize> {
    // Consume complete lines already fetched by BufReader. Reading only one
    // physical line here makes the parser rebuild an unfinished compound
    // command for every line, even if the rest is already in memory.
    let buffered = reader.fill_buf()?;
    let Some(newline) = buffered.iter().rposition(|&byte| byte == b'\n') else {
        return reader.read_until(b'\n', source);
    };
    let count = newline + 1;
    source.extend_from_slice(&buffered[..count]);
    reader.consume(count);
    Ok(count)
}

fn loop_outer(flow: Flow) -> Flow {
    match flow {
        Flow::Break(1) => Flow::Status(0),
        Flow::Continue(n) => Flow::Continue(n - 1),
        Flow::Break(n) => Flow::Break(n - 1),
        other => other,
    }
}

pub fn empty_variable() -> Variable {
    Variable {
        value: None,
        readonly: false,
        exported: false,
    }
}

pub fn is_name(name: &[u8]) -> bool {
    name.first()
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        && name.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_')
}

pub fn assignment(word: &[u8]) -> Option<(&[u8], &[u8])> {
    let n = word.iter().position(|&b| b == b'=')?;
    let name = &word[..n];
    is_name(name).then_some((name, &word[n + 1..]))
}

fn restore_vars(vars: &mut BTreeMap<Vec<u8>, Variable>, saved: Vec<(&[u8], Option<Variable>)>) {
    for (name, old) in saved.into_iter().rev() {
        if let Some(old) = old {
            vars.insert(name.to_vec(), old);
        } else {
            vars.remove(name);
        }
    }
}

pub fn decimal(value: &[u8]) -> Option<i32> {
    std::str::from_utf8(value).ok()?.parse().ok()
}

pub fn show(value: &[u8]) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(value)
}

pub fn diagnostic(message: &str) {
    let _ = sys::write(2, format!("nsh: {message}\n").as_bytes());
}

pub fn output(bytes: &[u8]) -> Flow {
    Flow::Status(if sys::write(1, bytes).is_ok() { 0 } else { 1 })
}

pub struct Fds {
    saved: Vec<(i32, Option<OwnedFd>)>,
    min: i32,
}

impl Fds {
    pub fn startup_stdio() -> io::Result<Self> {
        let mut restore = Self::new(&[]);
        for fd in 0..=2 {
            restore.save(fd)?;
        }
        let null = sys::open(b"/dev/null", libc::O_RDONLY)?;
        sys::install_fd(null, 0)?;
        Ok(restore)
    }

    fn new(redirs: &[Redir]) -> Self {
        let min = redirs
            .iter()
            .map(|r| r.fd)
            .max()
            .unwrap_or(2)
            .saturating_add(1)
            .max(10);
        Self {
            min,
            saved: Vec::new(),
        }
    }

    fn save(&mut self, fd: i32) -> io::Result<()> {
        if self.saved.iter().any(|(n, _)| *n == fd) {
            return Ok(());
        }
        match sys::dup_min(fd, self.min) {
            Ok(saved) => self.saved.push((fd, Some(saved))),
            Err(e) if e.raw_os_error() == Some(libc::EBADF) => self.saved.push((fd, None)),
            Err(e) => return Err(e),
        }
        Ok(())
    }
}

impl Drop for Fds {
    fn drop(&mut self) {
        if self.saved.is_empty() {
            return;
        }
        let _ = io::stdout().flush();
        let _ = io::stderr().flush();
        for (fd, saved) in self.saved.drain(..).rev() {
            if let Some(saved) = saved {
                let _ = sys::dup_to(saved.as_raw_fd(), fd);
            } else {
                sys::close(fd);
            }
        }
    }
}
