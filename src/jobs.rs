use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;

use crate::{
    shell::{Flow, Shell, diagnostic, show},
    syntax::{Chain, List, Pipeline},
    sys,
};

#[derive(PartialEq, Eq)]
enum WaitState {
    Done,
    Stopped,
    Interrupted,
}

struct Job {
    pgid: i32,
    pids: Vec<i32>,
    label: Vec<u8>,
    mode: Option<libc::termios>,
}

pub struct Jobs {
    jobs: BTreeMap<usize, Job>,
    events: BTreeMap<i32, i32>,
    live_children: Vec<i32>,
    pgid: i32,
    tty: Option<OwnedFd>,
    inherited_job_group: bool,
    next_job: usize,
    pub last_background: Option<i32>,
}

impl Jobs {
    pub fn new() -> Self {
        Self {
            jobs: BTreeMap::new(),
            events: BTreeMap::new(),
            live_children: Vec::new(),
            pgid: 0,
            tty: None,
            inherited_job_group: false,
            next_job: 1,
            last_background: None,
        }
    }

    pub(crate) fn child(&self) -> Self {
        Self {
            inherited_job_group: self.controlled() || self.inherited_job_group,
            ..Self::new()
        }
    }

    fn finish_fork(&self, pid: i32, restore: sys::ForkSignalMask) {
        drop(restore);
        // The kernel does not copy signals already pending before fork.
        // A worker's new child belongs to the same terminal job, so apply a
        // just-delivered HUP to it before waiting.
        if self.inherited_job_group && sys::signal_pending(libc::SIGHUP) {
            let _ = sys::send(pid, libc::SIGHUP);
        }
    }

    pub fn interactive(&mut self) -> io::Result<()> {
        sys::catch(libc::SIGINT)?;
        sys::signal(libc::SIGQUIT, libc::SIG_IGN)?;
        if !sys::tty(0) {
            return Ok(());
        }
        let tty = sys::open(b"/dev/tty", libc::O_RDWR)?;
        let tty = sys::dup_min(tty.as_raw_fd(), 64)?;
        let fd = tty.as_raw_fd();
        if sys::foreground(fd)? != sys::pgrp() {
            return Err(io::Error::other("shell is not in the foreground"));
        }
        for signal in [libc::SIGTSTP, libc::SIGTTIN, libc::SIGTTOU] {
            sys::signal(signal, libc::SIG_IGN)?;
        }
        if sys::pgrp() != sys::pid() {
            sys::set_pgrp(0, 0)?;
        }
        self.pgid = sys::pgrp();
        sys::give_terminal(fd, self.pgid)?;
        self.tty = Some(tty);
        Ok(())
    }

    pub fn controlled(&self) -> bool {
        self.tty.is_some()
    }

    fn update(&mut self, pid: i32, status: i32) {
        let Some(index) = self.live_children.iter().position(|&live| live == pid) else {
            // The asynchronous editor filesystem helper owns its process.
            return;
        };
        if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
            self.live_children.swap_remove(index);
        }
        if libc::WIFCONTINUED(status) {
            self.events.remove(&pid);
        } else {
            self.events.insert(pid, status);
        }
    }

    fn track_child(&mut self, pid: i32) {
        self.events.remove(&pid);
        self.live_children.push(pid);
    }

    fn forget(&mut self, job: &Job) {
        for pid in &job.pids {
            self.events.remove(pid);
        }
    }

    pub fn reap(&mut self) {
        let options = libc::WNOHANG | libc::WUNTRACED | libc::WCONTINUED;
        while !self.live_children.is_empty() {
            match sys::wait(options) {
                Ok(Some((pid, status))) => self.update(pid, status),
                Err(error) if error.raw_os_error() == Some(libc::ECHILD) => {
                    self.live_children.clear();
                    break;
                }
                _ => break,
            }
        }
    }

    fn terminal(&self, pgid: i32, mode: Option<&libc::termios>) -> io::Result<Option<Terminal>> {
        let Some(fd) = self.tty.as_ref().map(AsRawFd::as_raw_fd) else {
            return Ok(None);
        };
        let saved = sys::terminal_mode(fd)?;
        let guard = Terminal {
            fd,
            pgid: self.pgid,
            mode: saved,
        };
        sys::give_terminal(fd, pgid)?;
        if let Some(mode) = mode {
            sys::set_terminal_mode(fd, mode)?;
        }
        Ok(Some(guard))
    }

    fn finish(&self, job: &Job, pipefail: bool) -> Option<(i32, bool)> {
        let mut stopped = None;
        let mut last = 0;
        for pid in &job.pids {
            let raw = *self.events.get(pid)?;
            let value = status(raw);
            if libc::WIFSTOPPED(raw) {
                stopped = Some(value);
            }
            if !pipefail || value != 0 {
                last = value;
            }
        }
        Some(stopped.map(|n| (n, true)).unwrap_or((last, false)))
    }

    fn register(&mut self, job: Job) -> usize {
        let id = self.next_job;
        self.next_job += 1;
        self.jobs.insert(id, job);
        id
    }

    fn job_id(&self, arg: Option<&[u8]>) -> Option<usize> {
        match arg {
            None | Some(b"%+" | b"%%") => self.jobs.keys().next_back().copied(),
            Some(b"%-") => self.jobs.keys().rev().nth(1).copied(),
            Some(value) => {
                let (job, value) = match value.strip_prefix(b"%") {
                    Some(value) => (true, value),
                    None => (false, value),
                };
                let n = crate::shell::decimal(value)?;
                if job {
                    let n = usize::try_from(n).ok()?;
                    self.jobs.contains_key(&n).then_some(n)
                } else {
                    self.jobs
                        .iter()
                        .find_map(|(id, job)| job.pids.contains(&n).then_some(*id))
                }
            }
        }
    }

    pub fn signal_target(&self, word: &[u8]) -> Option<i32> {
        let job = self.jobs.get(&self.job_id(Some(word))?)?;
        (job.pgid > 0).then_some(-job.pgid)
    }

    pub fn report(&mut self, args: &[Vec<u8>]) -> Flow {
        self.reap();
        let (pids_only, args) = if args.first().is_some_and(|a| a == b"-p") {
            (true, &args[1..])
        } else {
            (false, args)
        };
        let selection = if args.is_empty() {
            self.jobs.keys().copied().collect::<Vec<_>>()
        } else {
            let Some(selection) = args
                .iter()
                .map(|id| self.job_id(Some(id)))
                .collect::<Option<Vec<_>>>()
            else {
                diagnostic("jobs: unknown job");
                return Flow::Status(1);
            };
            selection
        };
        for id in selection {
            let job = &self.jobs[&id];
            let state = match self.finish(job, false) {
                Some((_, true)) => "Stopped",
                Some((_, false)) => "Done",
                None => "Running",
            };
            let line = if pids_only {
                format!("{}\n", job.pgid.max(job.pids[0]))
            } else {
                format!("[{id}] {state}\t{}\n", show(&job.label))
            };
            if sys::write(1, line.as_bytes()).is_err() {
                return Flow::Status(1);
            }
        }
        Flow::Status(0)
    }
}

struct Terminal {
    fd: i32,
    pgid: i32,
    mode: libc::termios,
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = sys::give_terminal(self.fd, self.pgid);
        let _ = sys::set_terminal_mode(self.fd, &self.mode);
    }
}

fn status(raw: i32) -> i32 {
    if libc::WIFEXITED(raw) {
        libc::WEXITSTATUS(raw)
    } else if libc::WIFSIGNALED(raw) {
        128 + libc::WTERMSIG(raw)
    } else if libc::WIFSTOPPED(raw) {
        128 + libc::WSTOPSIG(raw)
    } else {
        1
    }
}

impl Shell {
    fn wait_on(&mut self, job: &Job, pipefail: bool, interruptible: bool) -> (Flow, WaitState) {
        loop {
            if interruptible {
                if let Some(flow) = self.wait_signal() {
                    return (flow, WaitState::Interrupted);
                }
            }
            if let Some((status, stopped)) =
                self.jobs.finish(job, pipefail).filter(|(_, stopped)| {
                    // A worker in the outer job's process group may receive its
                    // own child's stop status only after resuming. Let the outer
                    // shell represent the stopped job and keep waiting here.
                    !*stopped || job.pgid > 0
                })
            {
                return (
                    Flow::Status(status),
                    if stopped {
                        WaitState::Stopped
                    } else {
                        WaitState::Done
                    },
                );
            }
            match sys::wait(libc::WUNTRACED | libc::WCONTINUED) {
                Ok(Some((pid, raw))) => self.jobs.update(pid, raw),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.raw_os_error() == Some(libc::ECHILD) => {
                    self.jobs.live_children.clear();
                    return (Flow::Status(127), WaitState::Done);
                }
                _ => return (Flow::Status(127), WaitState::Done),
            }
        }
    }

    fn spawned(
        &mut self,
        count: usize,
        label: Vec<u8>,
        background: bool,
        pipefail: bool,
        mut child: impl FnMut(&mut Self, usize) -> Flow,
    ) -> Flow {
        if sys::signal_pending(libc::SIGHUP) {
            if let Some(flow) = self.dispatch_signals() {
                return flow;
            }
        }
        let mut pipes = Vec::new();
        for _ in 1..count {
            match sys::pipe() {
                Ok(pipe) => pipes.push(pipe),
                Err(e) => return self.error(1, &e.to_string(), false),
            }
        }
        let controlled = self.jobs.controlled();
        let mut gate = if controlled {
            match sys::pipe().and_then(|gate| sys::pipe().map(|ready| (gate, ready))) {
                Ok(pipes) => Some(pipes),
                Err(e) => return self.error(1, &e.to_string(), false),
            }
        } else {
            None
        };
        let mut job = Job {
            pgid: 0,
            pids: Vec::with_capacity(count),
            mode: None,
            label,
        };
        for stage in 0..count {
            match sys::fork() {
                Ok((0, restore)) => {
                    if controlled && sys::set_pgrp(0, job.pgid).is_err() {
                        sys::exit(1);
                    }
                    self.enter_child();
                    drop(restore);
                    if let Some(((read, write), (ready_read, ready_write))) = gate.take() {
                        drop(write);
                        drop(ready_read);
                        if sys::write(ready_write.as_raw_fd(), b"1").is_err() {
                            sys::exit(1);
                        }
                        drop(ready_write);
                        if sys::byte(read.as_raw_fd()).is_err() {
                            sys::exit(1);
                        }
                    }
                    if stage > 0 {
                        if sys::dup_to(pipes[stage - 1].0.as_raw_fd(), 0).is_err() {
                            sys::exit(1);
                        }
                    } else if background && !controlled {
                        let r = sys::open(b"/dev/null", libc::O_RDONLY)
                            .and_then(|null| sys::install_fd(null, 0));
                        if r.is_err() {
                            sys::exit(1);
                        }
                    }
                    if stage + 1 < count && sys::dup_to(pipes[stage].1.as_raw_fd(), 1).is_err() {
                        sys::exit(1);
                    }
                    drop(pipes);
                    let flow = child(self, stage);
                    sys::exit(self.exit_trap(flow.code()));
                }
                Ok((pid, restore)) => {
                    self.jobs.finish_fork(pid, restore);
                    if controlled {
                        if job.pgid == 0 {
                            job.pgid = pid;
                        }
                        let _ = sys::set_pgrp(pid, job.pgid);
                    }
                    self.jobs.track_child(pid);
                    job.pids.push(pid);
                }
                Err(e) => {
                    for &pid in &job.pids {
                        let _ = sys::send(pid, libc::SIGKILL);
                    }
                    drop(gate);
                    drop(pipes);
                    if !job.pids.is_empty() {
                        self.wait_on(&job, false, false);
                        self.jobs.forget(&job);
                    }
                    return self.error(1, &e.to_string(), false);
                }
            }
        }
        drop(pipes);
        let gate = gate.map(|(gate, (read, write))| {
            drop(write);
            (gate, read)
        });
        let gate = if let Some((gate, ready)) = gate {
            for _ in 0..count {
                loop {
                    match sys::byte(ready.as_raw_fd()) {
                        Ok(Some(_)) => break,
                        Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                        _ => {
                            for &pid in &job.pids {
                                let _ = sys::send(pid, libc::SIGKILL);
                            }
                            drop(gate);
                            self.wait_on(&job, false, false);
                            self.jobs.forget(&job);
                            return self.error(1, "job exited before terminal handoff", false);
                        }
                    }
                }
            }
            Some(gate)
        } else {
            None
        };
        if background {
            let pid = *job.pids.last().unwrap();
            self.jobs.last_background = Some(pid);
            let id = self.jobs.register(job);
            drop(gate);
            if self.interactive {
                let _ = sys::write(2, format!("[{id}] {pid}\n").as_bytes());
            }
            Flow::Status(0)
        } else {
            let terminal = match self.jobs.terminal(job.pgid, None) {
                Ok(terminal) => terminal,
                Err(e) => {
                    for &pid in &job.pids {
                        let _ = sys::send(pid, libc::SIGKILL);
                    }
                    drop(gate);
                    self.wait_on(&job, false, false);
                    self.jobs.forget(&job);
                    return self.error(1, &e.to_string(), false);
                }
            };
            drop(gate);
            let (flow, stopped) = self.wait_on(&job, pipefail, false);
            if stopped == WaitState::Stopped {
                job.mode = terminal
                    .as_ref()
                    .and_then(|t| sys::terminal_mode(t.fd).ok());
                let label = show(&job.label).into_owned();
                let id = self.jobs.register(job);
                if self.interactive {
                    let _ = sys::write(2, format!("[{id}] Stopped\t{label}\n").as_bytes());
                }
            } else {
                self.jobs.forget(&job);
            }
            drop(terminal);
            flow
        }
    }

    pub fn spawn_pipeline(&mut self, pipeline: &Pipeline) -> Flow {
        self.spawned(
            pipeline.commands.len(),
            b"pipeline".to_vec(),
            false,
            self.options.pipefail,
            |shell, stage| shell.run_worker_command(&pipeline.commands[stage], true),
        )
    }

    pub fn spawn_background(&mut self, chain: &Chain) -> Flow {
        if chain.rest.is_empty() && !chain.first.negate {
            let pipe = &chain.first;
            if pipe.commands.len() > 1
                || matches!(pipe.commands[0].kind, crate::syntax::Kind::Simple(_))
            {
                let label = if pipe.commands.len() > 1 {
                    b"pipeline".as_slice()
                } else {
                    b"background command"
                };
                return self.spawned(
                    pipe.commands.len(),
                    label.to_vec(),
                    true,
                    self.options.pipefail,
                    |shell, stage| shell.run_worker_command(&pipe.commands[stage], true),
                );
            }
        }
        self.spawned(
            1,
            b"background command".to_vec(),
            true,
            false,
            |shell, _| shell.chain(chain, false),
        )
    }

    pub fn spawn_subshell(&mut self, list: &List, tested: bool) -> Flow {
        self.spawned(1, b"subshell".to_vec(), false, false, |shell, _| {
            shell.execute(list, tested)
        })
    }

    pub fn spawn_external(&mut self, args: &[Vec<u8>], regular_builtin: bool) -> Flow {
        let label = if self.jobs.controlled() {
            args.join(&b' ')
        } else {
            Vec::new()
        };
        self.spawned(1, label, false, false, |shell, _| {
            shell.exec_external(args, regular_builtin)
        })
    }

    pub fn exec_external(&mut self, args: &[Vec<u8>], regular_builtin: bool) -> Flow {
        if args[0].is_empty() {
            return self.error(127, "empty command name", false);
        }
        let argv = args
            .iter()
            .map(|a| CString::new(a.as_slice()))
            .collect::<Result<Vec<_>, _>>();
        let Ok(argv) = argv else {
            return self.error(126, "argument contains NUL", false);
        };
        let env = self.environment();
        let exec = sys::ExecArgs::new(&argv, &env);
        let mut denied = false;
        let mut script = None;
        let name = args[0].as_slice();
        let may_use_builtin = regular_builtin && matches!(name, b"printf" | b"test" | b"[");
        for path in crate::hash::search(self, name) {
            if may_use_builtin && crate::regular::associated_path(&path) {
                if let Some(result) = crate::regular::run(self, name, &args[1..]) {
                    return result;
                }
            }
            let encoded = match path {
                Cow::Borrowed(path) => CString::new(path),
                Cow::Owned(path) => CString::new(path),
            };
            let Ok(encoded) = encoded else {
                continue;
            };
            let error = exec.exec(&encoded);
            let path = encoded.to_bytes();
            match error.raw_os_error() {
                Some(libc::ENOEXEC) => {
                    script = Some(encoded.into_bytes());
                    break;
                }
                Some(libc::EACCES | libc::EISDIR) => denied = true,
                Some(libc::ENOENT | libc::ENOTDIR) => {
                    if std::fs::metadata(OsStr::from_bytes(path)).is_ok() {
                        denied = true;
                    }
                }
                _ => return self.error(126, &format!("{}: {error}", show(path)), false),
            }
        }
        if let Some(path) = script {
            self.exec_script(&path, &args[1..])
        } else if denied {
            self.error(126, &format!("{}: cannot execute", show(&args[0])), false)
        } else {
            self.error(127, &format!("{}: not found", show(&args[0])), false)
        }
    }

    fn exec_script(&mut self, path: &[u8], args: &[Vec<u8>]) -> Flow {
        use std::os::unix::fs::FileExt;
        let file = match File::open(OsStr::from_bytes(path)) {
            Ok(file) => file,
            Err(e) => return self.error(126, &format!("{}: {e}", show(path)), false),
        };
        let mut probe = [0u8; 4096];
        match file.read_at(&mut probe, 0) {
            Ok(n) if !probe[..n].contains(&0) => {}
            _ => {
                return self.error(
                    126,
                    &format!("{}: invalid executable format", show(path)),
                    false,
                );
            }
        }
        let vars = self
            .vars
            .iter()
            .filter(|(_, val)| val.exported)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut new = Shell::new(
            path.to_vec(),
            args.to_vec(),
            Default::default(),
            false,
            false,
            false,
        );
        new.vars = vars;
        let flow = new.run_file(file, false);
        Flow::Exit(new.exit_trap(flow.code()))
    }

    pub fn search_paths<'a>(&'a self, name: &'a [u8]) -> impl Iterator<Item = Cow<'a, [u8]>> {
        let direct = name.contains(&b'/');
        let path = self.get(b"PATH").unwrap_or(b"/usr/bin:/bin");
        direct.then_some(Cow::Borrowed(name)).into_iter().chain(
            path.split(|b| *b == b':')
                .take_while(move |_| !direct)
                .map(move |dir| {
                    if dir.is_empty() {
                        Cow::Borrowed(name)
                    } else {
                        // Reserve the trailing NUL that execve's CString will add.
                        let mut candidate = Vec::with_capacity(dir.len() + name.len() + 2);
                        candidate.extend_from_slice(dir);
                        candidate.push(b'/');
                        candidate.extend_from_slice(name);
                        Cow::Owned(candidate)
                    }
                }),
        )
    }

    pub fn substitute(&mut self, source: &[u8]) -> Result<Vec<u8>, String> {
        let status = self.substitution_impl(source).map_err(|e| e.to_string())?;
        self.substitution = Some(status.1);
        Ok(status.0)
    }

    fn substitution_impl(&mut self, source: &[u8]) -> io::Result<(Vec<u8>, i32)> {
        let (read, write) = sys::pipe()?;
        let (pid, restore) = sys::fork()?;
        if pid == 0 {
            drop(read);
            if sys::dup_to(write.as_raw_fd(), 1).is_err() {
                sys::exit(1);
            }
            drop(write);
            self.enter_child();
            drop(restore);
            let flow = self.code(source, false);
            sys::exit(self.exit_trap(flow.code()));
        }
        self.jobs.finish_fork(pid, restore);
        self.jobs.track_child(pid);
        drop(write);
        let mut output = Vec::new();
        let result = File::from(read).read_to_end(&mut output);
        let job = Job {
            pgid: 0,
            pids: vec![pid],
            label: Vec::new(),
            mode: None,
        };
        let (flow, _) = self.wait_on(&job, false, false);
        self.jobs.forget(&job);
        result?;
        while output.last() == Some(&b'\n') {
            output.pop();
        }
        output.retain(|b| *b != 0);
        Ok((output, flow.code()))
    }

    pub fn wait_jobs(&mut self, args: &[Vec<u8>]) -> Flow {
        self.jobs.reap();
        let mut result = Flow::Status(0);
        if args.is_empty() {
            let ids = self.jobs.jobs.keys().copied().collect::<Vec<_>>();
            for id in ids {
                let (flow, interrupted) = self.wait_job(id, false, false);
                if interrupted {
                    return flow;
                }
            }
        } else {
            for arg in args {
                result = match self.jobs.job_id(Some(arg)) {
                    Some(id) => {
                        let (flow, interrupted) = self.wait_job(id, false, false);
                        if interrupted {
                            return flow;
                        }
                        flow
                    }
                    None => Flow::Status(127),
                };
                if !matches!(result, Flow::Status(_)) {
                    break;
                }
            }
        }
        result
    }

    pub fn resume(&mut self, arg: Option<&[u8]>, foreground: bool) -> Flow {
        self.jobs.reap();
        if !self.jobs.controlled() {
            return self.error(1, "job control is unavailable", false);
        }
        let Some(id) = self.jobs.job_id(arg) else {
            return self.error(1, "unknown job", false);
        };
        if foreground {
            self.wait_job(id, true, true).0
        } else {
            let job = &self.jobs.jobs[&id];
            if let Err(e) = sys::send(-job.pgid, libc::SIGCONT) {
                return self.error(1, &e.to_string(), false);
            }
            for pid in &job.pids {
                if self
                    .jobs
                    .events
                    .get(pid)
                    .is_some_and(|s| libc::WIFSTOPPED(*s))
                {
                    self.jobs.events.remove(pid);
                }
            }
            Flow::Status(0)
        }
    }

    fn wait_job(&mut self, id: usize, foreground: bool, resume: bool) -> (Flow, bool) {
        let mut job = self.jobs.jobs.remove(&id).unwrap();
        let mut terminal = None;
        if foreground {
            match self.jobs.terminal(job.pgid, job.mode.as_ref()) {
                Ok(handle) => terminal = handle,
                Err(e) => {
                    self.jobs.jobs.insert(id, job);
                    return (self.error(1, &e.to_string(), false), false);
                }
            }
        }
        if resume {
            if let Err(e) = sys::send(-job.pgid, libc::SIGCONT) {
                self.jobs.jobs.insert(id, job);
                return (self.error(1, &e.to_string(), false), false);
            }
            for pid in &job.pids {
                if self
                    .jobs
                    .events
                    .get(pid)
                    .is_some_and(|s| libc::WIFSTOPPED(*s))
                {
                    self.jobs.events.remove(pid);
                }
            }
        }
        let (flow, stopped) = self.wait_on(&job, self.options.pipefail, !foreground);
        if stopped != WaitState::Done {
            job.mode = terminal
                .as_ref()
                .and_then(|t| sys::terminal_mode(t.fd).ok())
                .or(job.mode);
            self.jobs.jobs.insert(id, job);
        } else {
            self.jobs.forget(&job);
        }
        (flow, stopped == WaitState::Interrupted)
    }

    pub fn terminate_jobs(&mut self) {
        self.jobs.reap();
        for job in self.jobs.jobs.values() {
            if job.pgid > 0
                && !self
                    .jobs
                    .finish(job, false)
                    .is_some_and(|(_, stopped)| !stopped)
            {
                let _ = sys::send(-job.pgid, libc::SIGHUP);
                let _ = sys::send(-job.pgid, libc::SIGCONT);
            }
        }
    }
}

impl Shell {
    pub fn run_hooks(&mut self, event: &str, args: &[Vec<u8>]) {
        self.run_plugin_event(event, args);
        if !self.config {
            return;
        }
        for handler in self.session.hooks(event) {
            let Some(command) = self.functions.get(&handler).cloned() else {
                diagnostic(&format!(
                    "{event}: unknown hook function {}",
                    show(&handler)
                ));
                continue;
            };
            let result = self.spawned(1, handler.clone(), false, false, |shell, _| {
                shell.function(&command, args, true)
            });
            if result.code() != 0 {
                diagnostic(&format!(
                    "{event}: {} exited with {}",
                    show(&handler),
                    result.code()
                ));
            }
        }
    }
}
